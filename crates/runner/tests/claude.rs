//! The runner with the real Claude adapter on a copy of the fixture transcript.

#![allow(clippy::unwrap_used)]

mod common;

use common::{CollectSink, append, config, fixture_transcript, labels};
use pitcrew_ingest::claude::ClaudeAdapter;
use pitcrew_protocol::events::EventBody;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

const WAIT: Duration = Duration::from_secs(5);

/// The fixture's lines, each with its newline.
fn fixture_lines() -> Vec<Vec<u8>> {
    std::fs::read(fixture_transcript())
        .unwrap()
        .split_inclusive(|b| *b == b'\n')
        .map(<[u8]>::to_vec)
        .collect()
}

fn session_file(home: &Path) -> PathBuf {
    let dir = home.join("projects").join("-w-paper");
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("2b6f1a8e-4c1d-4f5e-9a37-0c8d1e2f3a4b.jsonl")
}

/// Log lines, captured from every thread.
fn logs() -> &'static Arc<Mutex<Vec<u8>>> {
    static LOGS: OnceLock<Arc<Mutex<Vec<u8>>>> = OnceLock::new();
    LOGS.get_or_init(|| {
        let buf = Arc::new(Mutex::new(Vec::new()));
        let writer = Arc::clone(&buf);
        tracing_subscriber::fmt()
            .with_ansi(false)
            .with_max_level(tracing::Level::INFO)
            .with_writer(move || LogWriter(Arc::clone(&writer)))
            .init();
        buf
    })
}

struct LogWriter(Arc<Mutex<Vec<u8>>>);

impl Write for LogWriter {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn reindex_notes() -> usize {
    String::from_utf8_lossy(&logs().lock().unwrap())
        .matches("re-indexing it from the start")
        .count()
}

#[test]
fn appended_lines_become_the_right_events() {
    let home = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let path = session_file(home.path());
    let lines = fixture_lines();
    std::fs::write(&path, lines[..5].concat()).unwrap();

    let sink = Arc::new(CollectSink::default());
    let runner = pitcrew_runner::start(
        config(home.path(), state.path()),
        vec![Arc::new(ClaudeAdapter::new())],
        sink.clone(),
    )
    .unwrap();

    sink.wait_for(3, WAIT).expect("discovery");
    let first = sink.events();
    assert_eq!(
        labels(&first),
        ["discovered:Working", "tool:TodoWrite", "tool:Read"]
    );
    let EventBody::SessionDiscovered { session } = &first[0].body else {
        panic!("first event is not session_discovered");
    };
    assert_eq!(session.native_id, "2b6f1a8e-4c1d-4f5e-9a37-0c8d1e2f3a4b");
    assert_eq!(session.cwd, "/home/sam/work/diffusion-paper/paper");
    assert_eq!(session.branch.as_deref(), Some("main"));
    // The custom title comes later in the file; at discovery the first prompt stands in.
    assert!(
        session
            .title
            .as_deref()
            .is_some_and(|t| t.starts_with("Draft section 3"))
    );

    // The rest arrives in two writes, the first ending mid-line.
    let rest = lines[5..].concat();
    let (a, b) = rest.split_at(rest.len() / 3);
    append(&path, a);
    std::thread::sleep(Duration::from_millis(300));
    append(&path, b);

    sink.wait_for(11, WAIT)
        .expect("events for the appended lines");
    std::thread::sleep(Duration::from_millis(300));
    runner.stop();
    let events = sink.events();
    assert_eq!(
        labels(&events[3..]),
        [
            "tool:Edit",
            "edit:method.tex",
            "tool:Bash",
            "state:Waiting",
            "state:Working",
            "tool:AskUserQuestion",
            "state:Idle",
            "turn@6514",
        ]
    );
    let bash = events
        .iter()
        .find_map(|e| match &e.body {
            EventBody::ToolRan {
                tool,
                target,
                outcome,
                failed,
                receipt,
                ..
            } if tool == "Bash" => {
                Some((target.clone(), outcome.clone(), *failed, receipt.clone()))
            }
            _ => None,
        })
        .unwrap();
    assert_eq!(bash.0, "latexmk -pdf main.tex");
    assert_eq!(bash.1, "Output written on main.pdf (9 pages).");
    assert!(!bash.2);
    assert!(matches!(
        bash.3,
        pitcrew_protocol::model::Receipt::Transcript { offset: 4304, .. }
    ));
    let waiting = events.iter().find_map(|e| match &e.body {
        EventBody::SessionStateChanged { status_line, .. } if label_is(e, "state:Waiting") => {
            status_line.clone()
        }
        _ => None,
    });
    assert!(waiting.unwrap().starts_with("Should §3.2 compare"));
}

#[test]
fn a_new_session_next_to_a_watched_one_is_discovered() {
    let home = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let path = session_file(home.path());
    let lines = fixture_lines();
    std::fs::write(&path, lines[..1].concat()).unwrap();

    let sink = Arc::new(CollectSink::default());
    // `config` sets rediscovery to every 10 minutes, so only the folder watch can find it.
    let runner = pitcrew_runner::start(
        config(home.path(), state.path()),
        vec![Arc::new(ClaudeAdapter::new())],
        sink.clone(),
    )
    .unwrap();
    sink.wait_for(1, WAIT).expect("first session");

    // A new session in a new project folder: the parent of the watched folder sees it.
    let other = home.path().join("projects").join("-w-other");
    std::fs::create_dir_all(&other).unwrap();
    std::fs::write(other.join("second.jsonl"), lines[..1].concat()).unwrap();
    sink.wait_for(2, WAIT).expect("second session");
    runner.stop();
    let events = sink.events();
    assert_eq!(
        labels(&events),
        ["discovered:Working", "discovered:Working"]
    );
    let EventBody::SessionDiscovered { session } = &events[1].body else {
        panic!("not discovered");
    };
    assert_eq!(session.native_id, "2b6f1a8e-4c1d-4f5e-9a37-0c8d1e2f3a4b");
}

fn label_is(e: &pitcrew_protocol::events::Event, l: &str) -> bool {
    common::label(e) == l
}

#[test]
fn truncation_and_replacement_are_reindexed_from_the_start() {
    let _ = logs();
    let home = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let path = session_file(home.path());
    let lines = fixture_lines();
    std::fs::write(&path, lines.concat()).unwrap();

    let sink = Arc::new(CollectSink::default());
    let runner = pitcrew_runner::start(
        config(home.path(), state.path()),
        vec![Arc::new(ClaudeAdapter::new())],
        sink.clone(),
    )
    .unwrap();
    sink.wait_for(1, WAIT).expect("discovery");
    std::thread::sleep(Duration::from_millis(300));
    let n = sink.len();
    assert_eq!(common::label(&sink.events()[n - 1]), "turn@6514");
    let before = reindex_notes();

    // Truncate in place to the first three lines.
    std::fs::write(&path, lines[..3].concat()).unwrap();
    sink.wait_for(n + 2, WAIT).expect("events after truncation");
    std::thread::sleep(Duration::from_millis(300));
    let events = sink.events();
    assert_eq!(labels(&events[n..]), ["state:Working", "tool:TodoWrite"]);
    assert_eq!(reindex_notes(), before + 1, "a note in the log");

    // Replace with a new file (a new inode), larger than the truncated one.
    let n = events.len();
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, lines.concat()).unwrap();
    std::fs::rename(&tmp, &path).unwrap();
    let got = sink.wait_for(n + 1, WAIT);
    std::thread::sleep(Duration::from_millis(300));
    runner.stop();
    assert!(got.is_some(), "events after replacement");
    let events = sink.events();
    let after = labels(&events[n..]);
    assert_eq!(after.first().map(String::as_str), Some("tool:TodoWrite"));
    assert_eq!(after.last().map(String::as_str), Some("turn@6514"));
    assert_eq!(reindex_notes(), before + 2, "a note in the log");

    // Re-indexed content gets new event ids, never a collision with the first reading.
    let ids: std::collections::HashSet<_> = events.iter().map(|e| e.id).collect();
    assert_eq!(ids.len(), events.len());
}
