//! Sub-agents name their parents, in every engine's own way, and what a session says about itself
//! (its model, its account home) reaches the hub. The real adapters on synthetic transcripts in
//! temporary homes.

#![allow(clippy::unwrap_used)]

mod common;

use common::{CollectSink, age, eventually};
use pitcrew_ingest::claude::ClaudeAdapter;
use pitcrew_ingest::codex::CodexAdapter;
use pitcrew_ingest::opencode::OpenCodeAdapter;
use pitcrew_interfaces::source::SourceAdapter;
use pitcrew_protocol::events::{Event, EventBody};
use pitcrew_protocol::ids::{MachineId, MemberId, WorkspaceId};
use pitcrew_protocol::model::{Engine, Session};
use pitcrew_runner::RunnerConfig;
use serde_json::json;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

const WAIT: Duration = Duration::from_secs(10);

fn runner_config(state: &Path, homes: &[(Engine, &Path)]) -> RunnerConfig {
    let mut c = RunnerConfig::new(WorkspaceId::new(), MachineId::new(), MemberId::new(), state);
    for (engine, home) in homes {
        c = c.with_home(*engine, home);
    }
    c.notification_window = Duration::from_millis(175);
    c
}

fn discovered(events: &[Event]) -> Vec<Session> {
    events
        .iter()
        .filter_map(|e| match &e.body {
            EventBody::SessionDiscovered { session } => Some(session.clone()),
            _ => None,
        })
        .collect()
}

fn by_native<'a>(sessions: &'a [Session], native: &str) -> &'a Session {
    sessions
        .iter()
        .find(|s| s.native_id == native)
        .unwrap_or_else(|| panic!("no session {native} in {sessions:?}"))
}

fn lines(values: &[serde_json::Value]) -> String {
    values.iter().map(|v| format!("{v}\n")).collect()
}

/// A Claude user prompt and an assistant reply, with the given extra fields on each record.
fn claude_turn(extra: &serde_json::Value, cwd: &str, model: &str) -> String {
    let mut user = json!({"type": "user", "cwd": cwd, "gitBranch": "main",
        "timestamp": "2026-10-01T09:00:00Z", "message": {"role": "user", "content": "Synthetic prompt"}});
    let mut reply = json!({"type": "assistant", "cwd": cwd, "timestamp": "2026-10-01T09:00:05Z",
        "message": {"role": "assistant", "model": model, "content": [{"type": "text", "text": "Done."}],
                    "stop_reason": "end_turn"}});
    for record in [&mut user, &mut reply] {
        for (k, v) in extra.as_object().unwrap() {
            record[k] = v.clone();
        }
    }
    lines(&[user, reply])
}

/// An older Claude sub-agent: a top-level file whose records say `isSidechain`, naming its parent
/// only by the session id they carry. It is newer than its parent, so it is read first.
#[test]
fn an_older_sidechain_file_names_its_parent_by_session_id() {
    let home = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let project = home.path().join("projects").join("-w-atlas");
    std::fs::create_dir_all(&project).unwrap();
    let parent = project.join("sess-parent.jsonl");
    std::fs::write(
        &parent,
        claude_turn(
            &json!({"sessionId": "sess-parent"}),
            "/w/atlas",
            "synthetic-model-1",
        ),
    )
    .unwrap();
    age(&parent, Duration::from_secs(60));
    std::fs::write(
        project.join("agent-old1.jsonl"),
        claude_turn(
            &json!({"sessionId": "sess-parent", "isSidechain": true, "agentId": "old1"}),
            "/w/atlas",
            "synthetic-model-2",
        ),
    )
    .unwrap();

    let sink = Arc::new(CollectSink::default());
    let runner = pitcrew_runner::start(
        runner_config(state.path(), &[(Engine::Claude, home.path())]),
        vec![Arc::new(ClaudeAdapter::new())],
        sink.clone(),
    )
    .unwrap();
    assert!(eventually(WAIT, || discovered(&sink.events()).len() == 2));
    runner.stop();
    let sessions = discovered(&sink.events());
    let (p, c) = (
        by_native(&sessions, "sess-parent"),
        by_native(&sessions, "old1"),
    );
    assert_eq!(c.parent, Some(p.id));
    assert_eq!(p.parent, None);
    // What the transcript records: the model, and the account home it is in.
    let model = |s: &Session| s.recorded.as_ref().and_then(|r| r.model.clone());
    assert_eq!(model(p).as_deref(), Some("synthetic-model-1"));
    assert_eq!(model(c).as_deref(), Some("synthetic-model-2"));
    assert!(
        p.recorded.as_ref().is_some_and(|r| r.account.is_some()),
        "{p:?}"
    );
}

/// A sidechain transcript naming a parent outside its folder (`../x`) names none.
#[test]
fn a_parent_id_that_is_not_a_plain_name_is_no_parent() {
    let home = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let project = home.path().join("projects").join("-w-atlas");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(
        home.path().join("projects").join("escape.jsonl"),
        claude_turn(&json!({"sessionId": "escape"}), "/w", "m"),
    )
    .unwrap();
    std::fs::write(
        project.join("agent-x.jsonl"),
        claude_turn(
            &json!({"sessionId": "../escape", "isSidechain": true, "agentId": "x"}),
            "/w/atlas",
            "m",
        ),
    )
    .unwrap();
    let sink = Arc::new(CollectSink::default());
    let runner = pitcrew_runner::start(
        runner_config(state.path(), &[(Engine::Claude, home.path())]),
        vec![Arc::new(ClaudeAdapter::new())],
        sink.clone(),
    )
    .unwrap();
    assert!(eventually(WAIT, || !discovered(&sink.events()).is_empty()));
    runner.stop();
    let sessions = discovered(&sink.events());
    assert_eq!(by_native(&sessions, "x").parent, None);
}

/// The model a transcript records after the session was first read reaches the hub as
/// `session_updated`.
#[test]
fn a_model_recorded_later_is_a_session_update() {
    let home = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let project = home.path().join("projects").join("-w-atlas");
    std::fs::create_dir_all(&project).unwrap();
    let path = project.join("sess-live.jsonl");
    let turn = claude_turn(
        &json!({"sessionId": "sess-live"}),
        "/w/atlas",
        "synthetic-model-9",
    );
    let (prompt, reply) = turn.split_at(turn.find('\n').unwrap() + 1);
    std::fs::write(&path, prompt).unwrap();

    let sink = Arc::new(CollectSink::default());
    let runner = pitcrew_runner::start(
        runner_config(state.path(), &[(Engine::Claude, home.path())]),
        vec![Arc::new(ClaudeAdapter::new())],
        sink.clone(),
    )
    .unwrap();
    assert!(eventually(WAIT, || discovered(&sink.events()).len() == 1));
    assert_eq!(
        discovered(&sink.events())[0]
            .recorded
            .as_ref()
            .and_then(|r| r.model.clone()),
        None
    );
    common::append(&path, reply.as_bytes());
    let updated = || {
        sink.events().iter().find_map(|e| match &e.body {
            EventBody::SessionUpdated { model, title, .. } => Some((model.clone(), title.clone())),
            _ => None,
        })
    };
    assert!(eventually(WAIT, || updated().is_some()));
    // Claude's `<synthetic>` stand-in (a reply the CLI made up, as for an error) is no model: it
    // neither replaces the model nor brings it back after.
    let synthetic = json!({"type": "assistant", "cwd": "/w/atlas", "timestamp": "2026-10-01T09:00:07Z",
        "message": {"role": "assistant", "model": "<synthetic>", "content": [{"type": "text", "text": "No response."}]}});
    common::append(&path, lines(&[synthetic]).as_bytes());
    common::append(&path, reply.as_bytes());
    let turns = || {
        sink.events()
            .iter()
            .filter(|e| matches!(e.body, EventBody::TurnEnded { .. }))
            .count()
    };
    assert!(eventually(WAIT, || turns() >= 2));
    runner.stop();
    assert_eq!(
        updated(),
        Some((Some("synthetic-model-9".into()), None)),
        "only what changed"
    );
    let updates = sink
        .events()
        .iter()
        .filter(|e| matches!(e.body, EventBody::SessionUpdated { .. }))
        .count();
    assert_eq!(updates, 1, "{:?}", sink.events());
}

fn codex_meta(id: &str, source: &serde_json::Value, ts: &str) -> String {
    lines(&[
        json!({"timestamp": ts, "type": "session_meta",
               "payload": {"id": id, "timestamp": ts, "cwd": "/w/atlas", "source": source,
                           "git": {"branch": "main"}}}),
        json!({"timestamp": ts, "type": "turn_context", "payload": {"model": "synthetic-codex", "cwd": "/w/atlas"}}),
    ])
}

/// A Codex sub-agent names its parent by thread id. Here it is newer than its parent, so it is
/// read before the parent is: the parent is still found, in its day folder. A review sub-agent
/// names none, and an exec run is a session like any other.
#[test]
fn a_codex_sub_agent_names_its_parent_thread() {
    let home = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let day = home
        .path()
        .join("sessions")
        .join("2026")
        .join("10")
        .join("01");
    std::fs::create_dir_all(&day).unwrap();
    let parent_id = "0199a000-0000-7000-8000-00000000000a";
    let child_id = "0199a000-0000-7000-8000-00000000000b";
    let review_id = "0199a000-0000-7000-8000-00000000000c";
    let exec_id = "0199a000-0000-7000-8000-00000000000d";
    let parent = day.join(format!("rollout-2026-10-01T09-00-00-{parent_id}.jsonl"));
    std::fs::write(
        &parent,
        codex_meta(parent_id, &json!("cli"), "2026-10-01T09:00:00Z"),
    )
    .unwrap();
    age(&parent, Duration::from_secs(120));
    let spawn = json!({"subagent": {"thread_spawn": {"parent_thread_id": parent_id, "depth": 1}}});
    std::fs::write(
        day.join(format!("rollout-2026-10-01T09-01-00-{child_id}.jsonl")),
        codex_meta(child_id, &spawn, "2026-10-01T09:01:00Z"),
    )
    .unwrap();
    std::fs::write(
        day.join(format!("rollout-2026-10-01T09-02-00-{review_id}.jsonl")),
        codex_meta(
            review_id,
            &json!({"subagent": "review"}),
            "2026-10-01T09:02:00Z",
        ),
    )
    .unwrap();
    std::fs::write(
        day.join(format!("rollout-2026-10-01T09-03-00-{exec_id}.jsonl")),
        codex_meta(exec_id, &json!("exec"), "2026-10-01T09:03:00Z"),
    )
    .unwrap();

    let sink = Arc::new(CollectSink::default());
    let runner = pitcrew_runner::start(
        runner_config(state.path(), &[(Engine::Codex, home.path())]),
        vec![Arc::new(CodexAdapter::new())],
        sink.clone(),
    )
    .unwrap();
    assert!(eventually(WAIT, || discovered(&sink.events()).len() == 4));
    runner.stop();
    let sessions = discovered(&sink.events());
    let p = by_native(&sessions, parent_id);
    assert_eq!(by_native(&sessions, child_id).parent, Some(p.id));
    assert_eq!(by_native(&sessions, review_id).parent, None);
    assert_eq!(by_native(&sessions, exec_id).parent, None);
    assert_eq!(p.parent, None);
    assert_eq!(
        p.recorded.as_ref().and_then(|r| r.model.as_deref()),
        Some("synthetic-codex")
    );
}

/// A Codex adapter as a runner read it before parents were kept: the parent a transcript names
/// is left out, and it reads no lineage.
struct Unnamed(CodexAdapter);

impl SourceAdapter for Unnamed {
    fn engine(&self) -> Engine {
        Engine::Codex
    }

    fn discover(
        &self,
        home: &Path,
    ) -> Result<
        Vec<pitcrew_interfaces::source::TranscriptRef>,
        pitcrew_interfaces::source::SourceError,
    > {
        self.0.discover(home)
    }

    fn read_from(
        &self,
        t: &pitcrew_interfaces::source::TranscriptRef,
        cursor: &pitcrew_interfaces::source::Cursor,
    ) -> Result<pitcrew_interfaces::source::ParseChunk, pitcrew_interfaces::source::SourceError>
    {
        let mut chunk = self.0.read_from(t, cursor)?;
        if let Some(meta) = &mut chunk.meta {
            meta.parent = None;
        }
        Ok(chunk)
    }

    fn read_page(
        &self,
        t: &pitcrew_interfaces::source::TranscriptRef,
        before: Option<u64>,
        limit: usize,
    ) -> Result<pitcrew_interfaces::source::TranscriptPage, pitcrew_interfaces::source::SourceError>
    {
        self.0.read_page(t, before, limit)
    }
}

fn pending(state: &Path) -> i64 {
    let db = rusqlite::Connection::open(state.join("runner.sqlite3")).unwrap();
    db.query_row("SELECT count(*) FROM lineage_pending", [], |r| r.get(0))
        .unwrap()
}

/// A sub-agent a runner indexed before it kept the parents transcripts name was stated with none.
/// After the upgrade, it is looked up once at start, from its head, and stated again with its
/// parent (an event of its own); the next starts leave it be.
#[test]
fn an_upgraded_runner_states_the_parents_of_sub_agents_it_indexed_once() {
    let home = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let day = home
        .path()
        .join("sessions")
        .join("2026")
        .join("10")
        .join("02");
    std::fs::create_dir_all(&day).unwrap();
    let parent_id = "0199a000-0000-7000-8000-00000000001a";
    let child_id = "0199a000-0000-7000-8000-00000000001b";
    let parent = day.join(format!("rollout-2026-10-02T09-00-00-{parent_id}.jsonl"));
    std::fs::write(
        &parent,
        codex_meta(parent_id, &json!("cli"), "2026-10-02T09:00:00Z"),
    )
    .unwrap();
    age(&parent, Duration::from_secs(120));
    let spawn = json!({"subagent": {"thread_spawn": {"parent_thread_id": parent_id, "depth": 1}}});
    std::fs::write(
        day.join(format!("rollout-2026-10-02T09-01-00-{child_id}.jsonl")),
        codex_meta(child_id, &spawn, "2026-10-02T09:01:00Z"),
    )
    .unwrap();
    let config = || runner_config(state.path(), &[(Engine::Codex, home.path())]);

    // Before: the sub-agent is stated with no parent.
    let before = Arc::new(CollectSink::default());
    let runner = pitcrew_runner::start(
        config(),
        vec![Arc::new(Unnamed(CodexAdapter::new()))],
        before.clone(),
    )
    .unwrap();
    assert!(eventually(WAIT, || discovered(&before.events()).len() == 2));
    runner.stop();
    let sessions = discovered(&before.events());
    let (p, c) = (
        by_native(&sessions, parent_id),
        by_native(&sessions, child_id),
    );
    assert_eq!(c.parent, None);
    // As that runner's index was: no list of sub-agents to look up.
    let db = rusqlite::Connection::open(state.path().join("runner.sqlite3")).unwrap();
    db.execute_batch("DROP TABLE lineage_pending; PRAGMA user_version = 5;")
        .unwrap();
    drop(db);

    // The upgrade: stated again, with its parent, by an event of its own.
    let after = Arc::new(CollectSink::default());
    let runner =
        pitcrew_runner::start(config(), vec![Arc::new(CodexAdapter::new())], after.clone())
            .unwrap();
    let restated = || {
        after.events().into_iter().find(
            |e| matches!(&e.body, EventBody::SessionDiscovered { session } if session.id == c.id),
        )
    };
    assert!(eventually(WAIT, || restated().is_some()));
    runner.stop();
    let event = restated().unwrap();
    let EventBody::SessionDiscovered { session } = &event.body else {
        unreachable!()
    };
    assert_eq!(session.parent, Some(p.id));
    assert_eq!(session.native_id, child_id);
    let first = before
        .events()
        .into_iter()
        .find(|e| matches!(&e.body, EventBody::SessionDiscovered { session } if session.id == c.id))
        .unwrap();
    assert_ne!(event.id, first.id);
    assert_eq!(
        discovered(&after.events()).len(),
        1,
        "only the sub-agent is stated again"
    );

    // Settled: the next start states nothing again, and the list is empty.
    let again = Arc::new(CollectSink::default());
    let runner =
        pitcrew_runner::start(config(), vec![Arc::new(CodexAdapter::new())], again.clone())
            .unwrap();
    assert!(eventually(WAIT, || pending(state.path()) == 0));
    runner.stop();
    assert!(discovered(&again.events()).is_empty());
}

/// An OpenCode child session names its parent by `parent_id`, in the same store.
#[test]
fn an_opencode_child_session_names_its_parent() {
    let home = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let sql = pitcrew_fixtures::data_dir()
        .join("transcripts")
        .join("opencode");
    let db = rusqlite::Connection::open(home.path().join("opencode.db")).unwrap();
    for file in ["schema.sql", "seed.sql"] {
        db.execute_batch(&std::fs::read_to_string(sql.join(file)).unwrap())
            .unwrap();
    }
    // A child session of the fixture's, as OpenCode writes one for a sub-agent: newer, so read
    // before its parent.
    db.execute(
        "INSERT INTO session (id, project_id, parent_id, directory, title, version, time_created, time_updated)
         SELECT 'ses_01jb9demo00000000000000009', project_id, id, directory, 'Child session - x',
                version, time_created + 1000, time_updated + 1000
         FROM session WHERE parent_id IS NULL LIMIT 1",
        [],
    )
    .unwrap();
    // And one whose parent is not in the store (deleted): it has none, and no row is made for it.
    db.execute(
        "INSERT INTO session (id, project_id, parent_id, directory, title, version, time_created, time_updated)
         SELECT 'ses_01jb9demo00000000000000010', project_id, 'ses_01jb9demo0000000000000gone',
                directory, 'Child session - y', version, time_created + 2000, time_updated + 2000
         FROM session WHERE parent_id IS NULL LIMIT 1",
        [],
    )
    .unwrap();
    let children: Vec<(String, String)> = db
        .prepare(
            "SELECT id, parent_id FROM session WHERE parent_id IS NOT NULL
             AND parent_id IN (SELECT id FROM session)",
        )
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    drop(db);
    assert!(!children.is_empty(), "the fixture has a child session");
    let total = OpenCodeAdapter::new().discover(home.path()).unwrap().len();

    let sink = Arc::new(CollectSink::default());
    let runner = pitcrew_runner::start(
        runner_config(state.path(), &[(Engine::OpenCode, home.path())]),
        vec![Arc::new(OpenCodeAdapter::new())],
        sink.clone(),
    )
    .unwrap();
    assert!(eventually(WAIT, || discovered(&sink.events()).len() == total));
    runner.stop();
    let sessions = discovered(&sink.events());
    for (child, parent) in &children {
        assert_eq!(
            by_native(&sessions, child).parent,
            Some(by_native(&sessions, parent).id),
            "{child}"
        );
    }
    assert_eq!(
        by_native(&sessions, "ses_01jb9demo00000000000000010").parent,
        None
    );
}
