//! The runner against `FakeSource`: latency, restart without re-reading, and backpressure.

#![allow(clippy::unwrap_used)]

mod common;

use common::{CollectSink, LoggedFake, append, config, labels, transcript_ref, turn};
use pitcrew_protocol::events::EventBody;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[test]
fn new_items_arrive_fast_and_a_restart_resumes_from_the_cursor() {
    let home = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let path = home.path().join("proj").join("s1.jsonl");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, b"x").unwrap();

    let mut items: Vec<_> = (0..3).map(turn).collect();
    let source = Arc::new(LoggedFake::new(vec![transcript_ref(&path)], items.clone()));
    let sink = Arc::new(CollectSink::default());
    let runner = pitcrew_runner::start(
        config(home.path(), state.path()),
        vec![source.clone()],
        sink.clone(),
    )
    .unwrap();

    // Discovery reads everything there is, one item per read (the fake's way).
    sink.wait_for(4, Duration::from_secs(5))
        .expect("discovery events");
    assert_eq!(
        labels(&sink.events()),
        ["discovered:Idle", "turn@0", "turn@1", "turn@2"]
    );
    assert_eq!(source.reads(), [0, 1, 2, 3]);

    // Each new item, announced by a write to the file, becomes an event in under 300 ms.
    let mut latencies = Vec::new();
    for i in 3..8 {
        items.push(turn(i));
        source.set_items(items.clone());
        let n = sink.len();
        let wrote = Instant::now();
        append(&path, b"x");
        let got = sink
            .wait_for(n + 1, Duration::from_secs(2))
            .expect("event for the new item");
        latencies.push(got.duration_since(wrote));
    }
    println!("fake source: write-to-event latencies {latencies:?}");
    assert!(
        latencies.iter().all(|l| *l < Duration::from_millis(300)),
        "{latencies:?}"
    );
    runner.stop();
    let first_run = sink.events();
    assert_eq!(first_run.len(), 9);

    // Restart on the same index, with the file unchanged: nothing is read at all.
    source.reads.lock().unwrap().clear();
    let runner = pitcrew_runner::start(
        config(home.path(), state.path()),
        vec![source.clone()],
        sink.clone(),
    )
    .unwrap();
    std::thread::sleep(Duration::from_millis(400));
    assert!(source.reads().is_empty(), "re-read: {:?}", source.reads());
    assert_eq!(sink.len(), 9, "no events repeated");

    // A new item after the restart is read from the stored cursor (8), never from 0.
    items.push(turn(8));
    source.set_items(items.clone());
    append(&path, b"x");
    sink.wait_for(10, Duration::from_secs(2))
        .expect("event after restart");
    runner.stop();
    let reads = source.reads();
    assert_eq!(reads.first(), Some(&8), "{reads:?}");
    assert!(reads.iter().all(|r| *r >= 8), "{reads:?}");

    let all = sink.events();
    assert_eq!(labels(&all[9..]), ["turn@8"]);
    let ids: HashSet<_> = all.iter().map(|e| e.id).collect();
    assert_eq!(ids.len(), all.len(), "event ids are unique");
    let sessions: HashSet<_> = all
        .iter()
        .filter_map(|e| match &e.body {
            EventBody::TurnEnded { session, .. } => Some(*session),
            EventBody::SessionDiscovered { session } => Some(session.id),
            _ => None,
        })
        .collect();
    assert_eq!(
        sessions.len(),
        1,
        "the session id is stable across restarts"
    );
}

/// Accepts the first `n` batches, then refuses everything.
struct Refusing {
    left: std::sync::Mutex<usize>,
    inner: CollectSink,
}

impl pitcrew_runner::EventSink for Refusing {
    fn accept(
        &self,
        events: &[pitcrew_protocol::events::Event],
    ) -> Result<(), pitcrew_runner::SinkError> {
        let mut left = self.left.lock().unwrap();
        if *left == 0 {
            return Err(pitcrew_runner::SinkError("hub away".into()));
        }
        *left -= 1;
        self.inner.accept(events)
    }
}

#[test]
fn unaccepted_events_are_sent_again_after_a_restart_and_accepted_ones_are_not() {
    let home = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let path = home.path().join("proj").join("c.jsonl");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, b"x").unwrap();
    let source = Arc::new(LoggedFake::new(
        vec![transcript_ref(&path)],
        (0..5).map(turn).collect(),
    ));

    // The sink takes two batches (discovery + item 0, then item 1) and then refuses.
    let refusing = Arc::new(Refusing {
        left: std::sync::Mutex::new(2),
        inner: CollectSink::default(),
    });
    let runner = pitcrew_runner::start(
        config(home.path(), state.path()),
        vec![source.clone()],
        refusing.clone(),
    )
    .unwrap();
    refusing.inner.wait_for(3, Duration::from_secs(5)).unwrap();
    std::thread::sleep(Duration::from_millis(300));
    runner.stop();
    assert_eq!(
        labels(&refusing.inner.events()),
        ["discovered:Idle", "turn@0", "turn@1"]
    );

    // Restart with a working sink: reading resumes at the last accepted cursor, and exactly the
    // refused events arrive.
    source.reads.lock().unwrap().clear();
    let sink = Arc::new(CollectSink::default());
    let runner = pitcrew_runner::start(
        config(home.path(), state.path()),
        vec![source.clone()],
        sink.clone(),
    )
    .unwrap();
    sink.wait_for(3, Duration::from_secs(5)).unwrap();
    std::thread::sleep(Duration::from_millis(300));
    runner.stop();
    assert_eq!(labels(&sink.events()), ["turn@2", "turn@3", "turn@4"]);
    assert_eq!(source.reads().first(), Some(&2));
}

#[test]
fn forced_polling_sees_appends() {
    let home = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let path = home.path().join("proj").join("p.jsonl");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, b"x").unwrap();

    let source = Arc::new(LoggedFake::new(vec![transcript_ref(&path)], vec![turn(0)]));
    let sink = Arc::new(CollectSink::default());
    let mut cfg = config(home.path(), state.path());
    cfg.poll = pitcrew_runner::PollMode::Always;
    cfg.timing.poll_min = Duration::from_millis(50);
    cfg.timing.poll_max = Duration::from_millis(400);
    let runner = pitcrew_runner::start(cfg, vec![source.clone()], sink.clone()).unwrap();
    sink.wait_for(2, Duration::from_secs(5)).expect("discovery");

    // Let the poll interval back off to its maximum, then append.
    std::thread::sleep(Duration::from_secs(1));
    source.set_items(vec![turn(0), turn(1)]);
    let wrote = Instant::now();
    append(&path, b"x");
    let got = sink
        .wait_for(3, Duration::from_secs(2))
        .expect("polled change");
    runner.stop();
    let latency = got.duration_since(wrote);
    println!("polling: write-to-event latency {latency:?}");
    assert!(latency < Duration::from_millis(600), "{latency:?}");
}

#[test]
fn a_full_sink_applies_backpressure_without_dropping() {
    let home = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let path = home.path().join("proj").join("big.jsonl");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, b"x").unwrap();

    const ITEMS: u64 = 1000;
    const CAPACITY: usize = 4;
    let source = Arc::new(LoggedFake::new(
        vec![transcript_ref(&path)],
        (0..ITEMS).map(turn).collect(),
    ));
    let sink = Arc::new(CollectSink::default());
    sink.close();
    let mut cfg = config(home.path(), state.path());
    cfg.channel_capacity = CAPACITY;
    let runner = pitcrew_runner::start(cfg, vec![source.clone()], sink.clone()).unwrap();

    // The sink takes nothing. The watcher fills the channel and then waits: reads stop at the
    // channel's capacity plus the batch the sink holds and the one being sent.
    std::thread::sleep(Duration::from_millis(500));
    let stalled = source.reads().len();
    std::thread::sleep(Duration::from_millis(500));
    println!("backpressure: {stalled} reads while the sink was closed");
    assert_eq!(source.reads().len(), stalled, "the watcher is waiting");
    assert!(stalled <= CAPACITY + 2, "{stalled} reads queued");
    assert_eq!(sink.len(), 0);

    // Open the sink: every event arrives, once, in order.
    sink.open();
    let n = usize::try_from(ITEMS).unwrap() + 1;
    sink.wait_for(n, Duration::from_secs(20))
        .expect("all events");
    runner.stop();
    let events = sink.events();
    assert_eq!(events.len(), n);
    let offsets: Vec<u64> = events
        .iter()
        .filter_map(|e| match &e.body {
            EventBody::TurnEnded {
                receipt: pitcrew_protocol::model::Receipt::Transcript { offset, .. },
                ..
            } => Some(*offset),
            _ => None,
        })
        .collect();
    assert_eq!(offsets, (0..ITEMS).collect::<Vec<_>>());
}
