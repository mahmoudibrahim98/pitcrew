//! The runner against `FakeSource`: latency, restart without re-reading, and backpressure.

#![allow(clippy::unwrap_used)]

mod common;

use common::{
    CollectSink, LoggedFake, age, append, config, eventually, labels, prompt, transcript_ref, turn,
};
use pitcrew_interfaces::source::{
    Cursor, ParseChunk, SourceAdapter, SourceError, TranscriptItem, TranscriptPage, TranscriptRef,
};
use pitcrew_protocol::events::{Event, EventBody};
use pitcrew_protocol::model::Engine;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
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
    let mut cfg = config(home.path(), state.path());
    cfg.cache_file_discovery = true;
    cfg.notification_window = Duration::from_millis(175);
    let runner = pitcrew_runner::start(cfg.clone(), vec![source.clone()], sink.clone()).unwrap();

    // Discovery reads everything there is, one item per read (the fake's way).
    sink.wait_for(4, Duration::from_secs(5))
        .expect("discovery events");
    assert_eq!(
        labels(&sink.events()),
        ["discovered:Idle", "turn@0", "turn@1", "turn@2"]
    );
    assert_eq!(source.reads(), [0, 1, 2, 3]);

    // A loaded runner may delay one notification. Keep the product's 300 ms budget for the
    // median, and bound every sample well below the 600 s missed-notification sweep.
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
    latencies.sort_unstable();
    assert!(
        latencies[latencies.len() / 2] < Duration::from_millis(300),
        "{latencies:?}"
    );
    assert!(
        latencies.iter().all(|l| *l < Duration::from_millis(1500)),
        "{latencies:?}"
    );
    // A second notification for one write can read the next item early; let the last write's
    // own check finish, so the index holds the file's final size.
    std::thread::sleep(Duration::from_millis(300));
    runner.stop();
    let first_run = sink.events();
    assert_eq!(first_run.len(), 9);

    // Restart on the same index, with the file unchanged: nothing is read at all.
    source.reads.lock().unwrap().clear();
    let runner = pitcrew_runner::start(cfg, vec![source.clone()], sink.clone()).unwrap();
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

/// Accepts the first `n` batches, then refuses everything. Remembers every batch offered.
struct Refusing {
    left: Mutex<usize>,
    inner: CollectSink,
    offered: Mutex<Vec<Vec<Event>>>,
}

impl Refusing {
    fn new(accept: usize) -> Self {
        Self {
            left: Mutex::new(accept),
            inner: CollectSink::default(),
            offered: Mutex::new(Vec::new()),
        }
    }

    fn offered(&self) -> Vec<Vec<Event>> {
        self.offered.lock().unwrap().clone()
    }
}

impl pitcrew_runner::EventSink for Refusing {
    fn accept(&self, events: &[Event]) -> Result<(), pitcrew_runner::SinkError> {
        self.offered.lock().unwrap().push(events.to_vec());
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
    let refusing = Arc::new(Refusing::new(2));
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

#[test]
fn a_crash_between_split_batches_resends_the_same_ids() {
    let home = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let path = home.path().join("proj").join("long.jsonl");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, b"x").unwrap();

    // 300 turns in one read: session_discovered plus 300 turn_ended events, more than the 256 a
    // batch holds, so the discovery read goes out as two batches. Each prompt also changes the
    // state, which the discovery read folds silently.
    let items: Vec<TranscriptItem> = (0..300u64)
        .flat_map(|i| [prompt(2 * i), turn(2 * i + 1)])
        .collect();
    let source = Arc::new(LoggedFake::new(vec![transcript_ref(&path)], items).per_read(usize::MAX));

    // The sink takes the first batch; the runner then "crashes" with the second one unsaved.
    let refusing = Arc::new(Refusing::new(1));
    let runner = pitcrew_runner::start(
        config(home.path(), state.path()),
        vec![source.clone()],
        refusing.clone(),
    )
    .unwrap();
    assert!(eventually(Duration::from_secs(5), || refusing
        .offered()
        .len()
        >= 2));
    runner.stop();
    let accepted = refusing.inner.events();
    let refused = refusing.offered()[1].clone();
    assert_eq!(accepted.len(), 256);
    assert_eq!(refused.len(), 45);
    assert_eq!(common::label(&accepted[0]), "discovered:Idle");

    // After the restart the read is replayed. The refused events come again with the same ids,
    // and only session_discovered (already accepted, same id) is repeated.
    let sink = Arc::new(CollectSink::default());
    let runner = pitcrew_runner::start(
        config(home.path(), state.path()),
        vec![source.clone()],
        sink.clone(),
    )
    .unwrap();
    sink.wait_for(1 + refused.len(), Duration::from_secs(5))
        .expect("replayed events");
    std::thread::sleep(Duration::from_millis(300));
    runner.stop();
    let resent = sink.events();
    assert_eq!(resent.len(), 1 + refused.len(), "{:?}", labels(&resent));
    assert_eq!(
        resent[0].id, accepted[0].id,
        "session_discovered keeps its id"
    );
    let ids = |events: &[Event]| events.iter().map(|e| e.id).collect::<Vec<_>>();
    assert_eq!(labels(&resent[1..]), labels(&refused));
    assert_eq!(ids(&resent[1..]), ids(&refused));
}

fn call(offset: u64, id: &str, tool: &str) -> TranscriptItem {
    TranscriptItem::ToolUse {
        at: 1_790_000_000_000,
        call_id: id.into(),
        tool: tool.into(),
        target: "x".into(),
        input: None,
        offset,
    }
}

fn result(offset: u64, id: &str) -> TranscriptItem {
    TranscriptItem::ToolResult {
        at: 1_790_000_000_001,
        call_id: id.into(),
        is_error: false,
        summary: "done".into(),
        offset,
    }
}

/// OpenCode's offsets are positions: a call and its result share one, and a result can arrive
/// in a later read, after items with higher offsets. It must still become `tool_ran`.
#[test]
fn a_late_result_below_later_offsets_is_not_dropped() {
    let home = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let path = home.path().join("proj").join("oc.jsonl");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, b"x").unwrap();
    let text = TranscriptItem::AssistantText {
        at: 1_790_000_000_002,
        text: "meanwhile".into(),
        offset: 200,
    };
    let mut items = vec![call(100, "c1", "Bash"), text];
    let source = Arc::new(LoggedFake::new(vec![transcript_ref(&path)], items.clone()));
    let sink = Arc::new(CollectSink::default());
    let runner = pitcrew_runner::start(
        config(home.path(), state.path()),
        vec![source.clone()],
        sink.clone(),
    )
    .unwrap();
    sink.wait_for(1, Duration::from_secs(5)).expect("discovery");

    items.push(result(100, "c1"));
    source.set_items(items);
    append(&path, b"x");
    sink.wait_for(2, Duration::from_secs(5))
        .expect("the late result");
    std::thread::sleep(Duration::from_millis(200));
    runner.stop();
    assert_eq!(labels(&sink.events()), ["discovered:Working", "tool:Bash"]);
    let EventBody::ToolRan { receipt, .. } = &sink.events()[1].body else {
        panic!("not tool_ran");
    };
    assert!(matches!(
        receipt,
        pitcrew_protocol::model::Receipt::Transcript { offset: 100, .. }
    ));
}

/// Items that share an offset are told apart when a crash is replayed: exactly the refused
/// events come again, with their ids.
#[test]
fn items_sharing_an_offset_are_resent_exactly_after_a_crash() {
    let home = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let path = home.path().join("proj").join("shared.jsonl");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, b"x").unwrap();
    let items = vec![
        call(100, "a", "Bash"),
        result(100, "a"),
        call(100, "b", "Edit"),
        result(100, "b"),
        TranscriptItem::TurnEnded {
            at: 1_790_000_000_003,
            offset: 100,
        },
    ];
    let source = Arc::new(LoggedFake::new(vec![transcript_ref(&path)], items).per_read(usize::MAX));
    let mut cfg = config(home.path(), state.path());
    // One item's events per batch: the read is accepted in parts.
    cfg.max_batch_events = 1;

    let refusing = Arc::new(Refusing::new(2));
    let runner =
        pitcrew_runner::start(cfg.clone(), vec![source.clone()], refusing.clone()).unwrap();
    assert!(eventually(Duration::from_secs(5), || refusing
        .offered()
        .len()
        >= 3));
    runner.stop();
    assert_eq!(
        labels(&refusing.inner.events()),
        ["discovered:Idle", "tool:Bash"]
    );
    let refused: Vec<Event> = refusing.offered()[2].clone();
    assert_eq!(labels(&refused), ["tool:Edit"]);

    let sink = Arc::new(CollectSink::default());
    let runner = pitcrew_runner::start(cfg, vec![source.clone()], sink.clone()).unwrap();
    sink.wait_for(3, Duration::from_secs(5)).expect("replayed");
    std::thread::sleep(Duration::from_millis(300));
    runner.stop();
    let resent = sink.events();
    // session_discovered (accepted, but saved only with the cursor) comes again with its id.
    assert_eq!(
        labels(&resent),
        ["discovered:Idle", "tool:Edit", "turn@100"]
    );
    assert_eq!(resent[0].id, refusing.inner.events()[0].id);
    assert_eq!(resent[1].id, refused[0].id);
}

/// Like OpenCode: progress lives in `cursor.state` (a row id); the offset never moves.
struct StateCursor {
    path: PathBuf,
    items: Vec<TranscriptItem>,
}

impl SourceAdapter for StateCursor {
    fn engine(&self) -> Engine {
        Engine::Claude
    }

    fn discover(&self, _home: &Path) -> Result<Vec<TranscriptRef>, SourceError> {
        Ok(vec![transcript_ref(&self.path)])
    }

    fn read_from(&self, _t: &TranscriptRef, cursor: &Cursor) -> Result<ParseChunk, SourceError> {
        let next = cursor
            .state
            .as_ref()
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let Some(item) = self.items.get(usize::try_from(next).unwrap()) else {
            return Ok(ParseChunk {
                cursor: cursor.clone(),
                ..ParseChunk::default()
            });
        };
        Ok(ParseChunk {
            cursor: Cursor {
                offset: 0,
                state: Some(serde_json::json!(next + 1)),
            },
            meta: None,
            items: vec![item.clone()],
        })
    }

    fn read_page(
        &self,
        _t: &TranscriptRef,
        _before: Option<u64>,
        _limit: usize,
    ) -> Result<TranscriptPage, SourceError> {
        Ok(TranscriptPage {
            items: Vec::new(),
            from: 0,
            to: 0,
            at_start: true,
        })
    }
}

#[test]
fn progress_in_the_cursor_state_keeps_reading() {
    let home = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let path = home.path().join("opencode.db");
    std::fs::write(&path, b"x").unwrap();
    let source = Arc::new(StateCursor {
        path: path.clone(),
        items: (0..3).map(turn).collect(),
    });
    let sink = Arc::new(CollectSink::default());
    let runner = pitcrew_runner::start(
        config(home.path(), state.path()),
        vec![source],
        sink.clone(),
    )
    .unwrap();
    // No file changes after the start: all three items come from the discovery read alone.
    let all = sink.wait_for(4, Duration::from_secs(3));
    runner.stop();
    assert!(all.is_some(), "{:?}", labels(&sink.events()));
    assert_eq!(
        labels(&sink.events()),
        ["discovered:Idle", "turn@0", "turn@1", "turn@2"]
    );
}

/// The session name the fake gives a transcript: its file stem.
fn discovered_names(events: &[Event]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match &e.body {
            EventBody::SessionDiscovered { session } => Some(session.native_id.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn new_transcripts_are_indexed_newest_first() {
    let home = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let hour = Duration::from_secs(3600);
    let mut refs = Vec::new();
    for (name, hours) in [("a", 3), ("b", 1), ("c", 2)] {
        let path = home.path().join("proj").join(format!("{name}.jsonl"));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"x").unwrap();
        age(&path, hour * hours);
        refs.push(transcript_ref(&path));
    }
    let source = Arc::new(LoggedFake::new(refs, Vec::new()));
    let sink = Arc::new(CollectSink::default());
    let runner = pitcrew_runner::start(
        config(home.path(), state.path()),
        vec![source],
        sink.clone(),
    )
    .unwrap();
    sink.wait_for(3, Duration::from_secs(5)).expect("discovery");
    runner.stop();
    assert_eq!(discovered_names(&sink.events()), ["b", "c", "a"]);
}

#[test]
fn stop_is_prompt_during_a_long_backfill() {
    let home = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let refs: Vec<TranscriptRef> = (0..100)
        .map(|i| {
            let path = home.path().join("proj").join(format!("s{i:03}.jsonl"));
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, b"x").unwrap();
            transcript_ref(&path)
        })
        .collect();
    // 100 transcripts at 100 ms a read: a 10 s backfill.
    let source = Arc::new(LoggedFake::new(refs, Vec::new()).delay(Duration::from_millis(100)));
    let sink = Arc::new(CollectSink::default());
    let runner = pitcrew_runner::start(
        config(home.path(), state.path()),
        vec![source.clone()],
        sink.clone(),
    )
    .unwrap();
    sink.wait_for(2, Duration::from_secs(5))
        .expect("backfill started");
    let asked = Instant::now();
    runner.stop();
    let took = asked.elapsed();
    println!(
        "stop during backfill took {took:?} after {} of 100 reads",
        source.reads().len()
    );
    // A stop waits for the read in progress and the batch being saved, never the backfill. The
    // margin is wide (on Windows CI the first save, which creates the store's WAL files, has
    // taken a second), and still far below the backfill's 10 s.
    assert!(took < Duration::from_secs(2), "{took:?}");
    assert!(source.reads().len() < 100);
}

#[test]
fn a_panicking_adapter_skips_one_transcript_not_the_runner() {
    let home = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let dir = home.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let (bad, good) = (dir.join("bad.jsonl"), dir.join("good.jsonl"));
    std::fs::write(&bad, b"x").unwrap();
    std::fs::write(&good, b"x").unwrap();
    let source = Arc::new(LoggedFake::new(
        vec![transcript_ref(&bad), transcript_ref(&good)],
        vec![turn(0)],
    ));
    source.panic_on(&bad);
    let sink = Arc::new(CollectSink::default());
    let runner = pitcrew_runner::start(
        config(home.path(), state.path()),
        vec![source.clone()],
        sink.clone(),
    )
    .unwrap();
    sink.wait_for(2, Duration::from_secs(5))
        .expect("the good one");

    // The watcher is still alive: a new item in the good transcript arrives.
    source.set_items(vec![turn(0), turn(1)]);
    append(&good, b"x");
    sink.wait_for(3, Duration::from_secs(5))
        .expect("event after the panic");
    runner.stop();
    assert_eq!(discovered_names(&sink.events()), ["good"]);
    assert_eq!(
        labels(&sink.events()),
        ["discovered:Idle", "turn@0", "turn@1"]
    );
}
