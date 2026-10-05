//! The runner writing into the hub's store in one process (`StoreSink`): the fixture transcript
//! is stored exactly once, even when batches are sent again after a lost acknowledgement and a
//! crash.

#![allow(clippy::unwrap_used)]

mod common;

use common::{FIXTURE_ID, append, claude_file, config, fixture_lines, labels};
use pitcrew_ingest::claude::ClaudeAdapter;
use pitcrew_protocol::events::Event;
use pitcrew_protocol::ids::MemberId;
use pitcrew_runner::{EventSink, SinkError, StoreSink};
use pitcrew_store::{Store, StoreOptions};
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

fn stored(store: &Store) -> Vec<Event> {
    store
        .since(0, 10_000)
        .unwrap()
        .into_iter()
        .map(|s| s.event)
        .collect()
}

fn wait_for(store: &Store, n: usize) -> Vec<Event> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let events = stored(store);
        if events.len() >= n || Instant::now() > deadline {
            return events;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Stores every batch, then reports a failure: the acknowledgement is lost.
struct AckLost {
    inner: StoreSink,
    calls: AtomicUsize,
}

impl EventSink for AckLost {
    fn accept(&self, events: &[Event]) -> Result<(), SinkError> {
        self.inner.accept(events)?;
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(SinkError("the acknowledgement was lost".into()))
    }
}

const EXPECTED: [&str; 11] = [
    "discovered:Working",
    "tool:TodoWrite",
    "tool:Read",
    "tool:Edit",
    "edit:method.tex",
    "tool:Bash",
    "state:Waiting",
    "state:Working",
    "tool:AskUserQuestion",
    "state:Idle",
    "turn@6514",
];

#[test]
fn the_fixture_reaches_the_store_once_across_a_crash_and_resend() {
    let home = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let hub = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(hub.path().join("hub.db"), StoreOptions::default()).unwrap());
    let owner = MemberId::new();
    let path = claude_file(home.path(), FIXTURE_ID);
    let lines = fixture_lines();
    std::fs::write(&path, lines[..5].concat()).unwrap();

    // First run: every batch is stored, but its acknowledgement is lost. The runner retries
    // (stored again: skipped), then "crashes" without saving its cursor.
    let flaky = Arc::new(AckLost {
        inner: StoreSink::new(Arc::clone(&store), owner),
        calls: AtomicUsize::new(0),
    });
    let mut cfg = config(home.path(), state.path());
    cfg.timing.sink_retry_max = Duration::from_millis(50);
    let runner =
        pitcrew_runner::start(cfg, vec![Arc::new(ClaudeAdapter::new())], flaky.clone()).unwrap();
    assert_eq!(wait_for(&store, 3).len(), 3);
    let deadline = Instant::now() + Duration::from_secs(5);
    while flaky.calls.load(Ordering::SeqCst) < 3 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(flaky.calls.load(Ordering::SeqCst) >= 3, "retried");
    runner.stop();
    assert_eq!(stored(&store).len(), 3, "a resent batch is stored once");

    // Second run, a working link: the unsaved read is replayed (same ids: nothing new is
    // stored), and the rest of the transcript follows.
    let runner = pitcrew_runner::start(
        config(home.path(), state.path()),
        vec![Arc::new(ClaudeAdapter::new())],
        Arc::new(StoreSink::new(Arc::clone(&store), owner)),
    )
    .unwrap();
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(stored(&store).len(), 3, "the replay adds nothing");
    append(&path, &lines[5..].concat());
    // And the custom title the rest of the transcript sets: a session update.
    wait_for(&store, EXPECTED.len() + 1);
    std::thread::sleep(Duration::from_millis(300));
    runner.stop();

    let events = stored(&store);
    assert_eq!(common::labels_without_updates(&events), EXPECTED);
    assert_eq!(common::title_updates(&events), 1);
    let ids: HashSet<_> = events.iter().map(|e| e.id).collect();
    assert_eq!(ids.len(), events.len(), "each event once");
    // Agentless sessions' events are authored by the configured owner.
    assert!(
        events
            .iter()
            .all(|e| e.author == owner && e.on_behalf_of.is_none())
    );
}

#[test]
fn a_full_read_is_stored_in_order() {
    let home = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let hub = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(hub.path().join("hub.db"), StoreOptions::default()).unwrap());
    std::fs::write(
        claude_file(home.path(), FIXTURE_ID),
        fixture_lines().concat(),
    )
    .unwrap();
    let runner = pitcrew_runner::start(
        config(home.path(), state.path()),
        vec![Arc::new(ClaudeAdapter::new())],
        Arc::new(StoreSink::new(Arc::clone(&store), MemberId::new())),
    )
    .unwrap();
    let events = wait_for(&store, 8);
    runner.stop();
    // Read at once, the states are folded into session_discovered.
    assert_eq!(
        labels(&events),
        [
            "discovered:Idle",
            "tool:TodoWrite",
            "tool:Read",
            "tool:Edit",
            "edit:method.tex",
            "tool:Bash",
            "tool:AskUserQuestion",
            "turn@6514"
        ]
    );
}
