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

const REINDEXED: &str = "re-indexing it from the start";
const DROPPED: &str = "transcript deleted; no longer watching it";

/// Log lines that contain `note` and mention the canonical `path` (tests run in parallel, each in
/// its own folder).
fn notes(note: &str, path: &str) -> usize {
    String::from_utf8_lossy(&logs().lock().unwrap())
        .lines()
        .filter(|l| l.contains(note) && l.contains(path))
        .count()
}

fn canonical(path: &Path) -> String {
    path.canonicalize().unwrap().display().to_string()
}

fn reindex_notes(path: &Path) -> usize {
    notes(REINDEXED, &canonical(path))
}

fn session_of(e: &pitcrew_protocol::events::Event) -> Option<pitcrew_protocol::ids::SessionId> {
    match &e.body {
        EventBody::SessionDiscovered { session } => Some(session.id),
        EventBody::SessionStateChanged { session, .. }
        | EventBody::ToolRan { session, .. }
        | EventBody::FileEdited { session, .. }
        | EventBody::TurnEnded { session, .. } => Some(*session),
        _ => None,
    }
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

/// A sub-agent's transcript (`<session>/subagents/agent-*.jsonl`) is a session of its own whose
/// parent is the session that started it, whichever of the two is found first.
#[test]
fn a_sub_agent_session_names_its_parent() {
    let home = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let path = session_file(home.path());
    let lines = fixture_lines();
    std::fs::write(&path, lines[..3].concat()).unwrap();
    let sub_dir = path.with_extension("").join("subagents");
    std::fs::create_dir_all(&sub_dir).unwrap();
    let sub = String::from_utf8(lines[..3].concat()).unwrap().replace(
        r#""isSidechain":false"#,
        r#""isSidechain":true,"agentId":"agent-a1""#,
    );
    // The sub-agent is the newest, so it is indexed first.
    std::fs::write(sub_dir.join("agent-a1.jsonl"), sub).unwrap();
    common::age(&path, Duration::from_secs(60));

    let sink = Arc::new(CollectSink::default());
    let runner = pitcrew_runner::start(
        config(home.path(), state.path()),
        vec![Arc::new(ClaudeAdapter::new())],
        sink.clone(),
    )
    .unwrap();
    sink.wait_for(4, WAIT).expect("both sessions");
    runner.stop();
    let sessions: Vec<pitcrew_protocol::model::Session> = sink
        .events()
        .into_iter()
        .filter_map(|e| match e.body {
            EventBody::SessionDiscovered { session } => Some(session),
            _ => None,
        })
        .collect();
    assert_eq!(sessions.len(), 2, "{:?}", labels(&sink.events()));
    let parent = sessions
        .iter()
        .find(|s| s.native_id == "2b6f1a8e-4c1d-4f5e-9a37-0c8d1e2f3a4b")
        .unwrap();
    let child = sessions.iter().find(|s| s.native_id == "agent-a1").unwrap();
    assert_eq!(child.parent, Some(parent.id));
    assert_eq!(parent.parent, None);
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
    let before = reindex_notes(&path);

    // Truncate in place to the first three lines.
    std::fs::write(&path, lines[..3].concat()).unwrap();
    sink.wait_for(n + 2, WAIT).expect("events after truncation");
    std::thread::sleep(Duration::from_millis(300));
    let events = sink.events();
    assert_eq!(labels(&events[n..]), ["state:Working", "tool:TodoWrite"]);
    assert_eq!(reindex_notes(&path), before + 1, "a note in the log");

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
    assert_eq!(reindex_notes(&path), before + 2, "a note in the log");

    // Re-indexed content gets new event ids, never a collision with the first reading.
    let ids: std::collections::HashSet<_> = events.iter().map(|e| e.id).collect();
    assert_eq!(ids.len(), events.len());
}

#[test]
fn a_deleted_transcript_is_dropped_and_keeps_its_session_if_it_returns() {
    let _ = logs();
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
    let session = session_of(&sink.events()[0]).unwrap();
    let file = canonical(&path);
    assert_eq!(notes(REINDEXED, &file), 0);

    std::fs::remove_file(&path).unwrap();
    assert!(
        common::eventually(WAIT, || notes(DROPPED, &file) == 1),
        "the deletion is noticed"
    );

    // A file comes back at the path (restored, or rewritten; it may even reuse the inode): same
    // session, read again from the start.
    std::fs::write(&path, lines.concat()).unwrap();
    runner.rescan();
    sink.wait_for(4, WAIT).expect("events after it came back");
    std::thread::sleep(Duration::from_millis(300));
    runner.stop();
    let events = sink.events();
    assert!(events[3..].iter().all(|e| session_of(e) == Some(session)));
    assert_eq!(
        labels(&events[3..5]),
        ["tool:TodoWrite", "tool:Read"],
        "from the start: {:?}",
        labels(&events)
    );
    assert_eq!(common::label(events.last().unwrap()), "turn@6514");
    assert_eq!(notes(REINDEXED, &file), 1);
    assert_eq!(notes(DROPPED, &file), 1);
}

#[test]
fn a_new_session_in_a_cold_project_folder_is_discovered_at_once() {
    let home = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let path = session_file(home.path());
    let lines = fixture_lines();
    std::fs::write(&path, lines[..1].concat()).unwrap();
    // Untouched for two days: cold.
    common::age(&path, Duration::from_secs(2 * 24 * 60 * 60));

    let sink = Arc::new(CollectSink::default());
    // Rediscovery and sweeps every 10 minutes: only a folder watch can find the new session.
    let runner = pitcrew_runner::start(
        config(home.path(), state.path()),
        vec![Arc::new(ClaudeAdapter::new())],
        sink.clone(),
    )
    .unwrap();
    sink.wait_for(1, WAIT).expect("the cold session");

    std::fs::write(path.with_file_name("next.jsonl"), lines[..1].concat()).unwrap();
    let found = sink.wait_for(2, WAIT);
    runner.stop();
    assert!(found.is_some(), "{:?}", labels(&sink.events()));
}

#[test]
fn the_first_session_in_an_empty_home_is_discovered_at_once() {
    let home = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let sink = Arc::new(CollectSink::default());
    let runner = pitcrew_runner::start(
        config(home.path(), state.path()),
        vec![Arc::new(ClaudeAdapter::new())],
        sink.clone(),
    )
    .unwrap();
    // Let the start finish; the home has no `projects` folder yet.
    std::thread::sleep(Duration::from_millis(300));

    let path = session_file(home.path());
    std::fs::write(&path, fixture_lines()[..1].concat()).unwrap();
    let found = sink.wait_for(1, WAIT);
    runner.stop();
    assert!(found.is_some(), "not discovered without a rescan");
}

/// On HPC a home is often reached through a symlink (`/home` → `/gpfs/home`), and may not exist
/// before the first `claude` run. Either way a transcript must keep its session id when the home
/// is later named by its real path.
#[cfg(unix)]
#[test]
fn a_symlinked_home_keeps_its_session_ids_even_if_it_appears_later() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let real = root.path().join("real");
    let link = root.path().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let lines = fixture_lines();

    // First run: the home is a symlink to a folder that does not exist yet.
    let sink = Arc::new(CollectSink::default());
    let runner = pitcrew_runner::start(
        config(&link, state.path()),
        vec![Arc::new(ClaudeAdapter::new())],
        sink.clone(),
    )
    .unwrap();
    std::thread::sleep(Duration::from_millis(300));
    std::fs::create_dir(&real).unwrap();
    let path = session_file(&link);
    std::fs::write(&path, lines[..5].concat()).unwrap();
    runner.rescan();
    sink.wait_for(3, WAIT)
        .expect("discovered once the home exists");
    // Changes are seen through the watch on the real folders.
    append(&path, &lines[5..].concat());
    sink.wait_for(11, WAIT).expect("appended lines");
    std::thread::sleep(Duration::from_millis(300));
    runner.stop();
    let first = sink.events();
    assert_eq!(first.len(), 11, "{:?}", labels(&first));
    let session = session_of(&first[0]).unwrap();

    // Second run, the same home by its real path: nothing is sent again...
    let runner = pitcrew_runner::start(
        config(&real, state.path()),
        vec![Arc::new(ClaudeAdapter::new())],
        sink.clone(),
    )
    .unwrap();
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(sink.len(), 11, "{:?}", labels(&sink.events()[11..]));

    // ...and a new line belongs to the same session.
    append(&real.join(path.strip_prefix(&link).unwrap()), &lines[0]);
    sink.wait_for(12, WAIT).expect("event for the new line");
    runner.stop();
    let last = sink.events().pop().unwrap();
    assert_eq!(common::label(&last), "state:Working");
    assert_eq!(session_of(&last), Some(session));
}
