//! Sessions the hub named (`StartSession`'s `session`, a dispatch's): the CLI's transcript is
//! reported under that id, once, for each engine (the real adapters, `FakeRuntime`, transcripts
//! written as each CLI writes them); its sub-agents keep ids of their own with it as their parent;
//! the CLI gets what the `SessionEnv` gives; and a second start in a folder where a CLI matched by
//! folder still waits is refused.

#![allow(clippy::unwrap_used)]

mod common;

use common::{CollectSink, fixture_lines_as, labels, place};
use pitcrew_ingest::claude::ClaudeAdapter;
use pitcrew_ingest::codex::CodexAdapter;
use pitcrew_ingest::opencode::OpenCodeAdapter;
use pitcrew_interfaces::fake::FakeRuntime;
use pitcrew_interfaces::runtime::{
    OutputChunk, Runtime, RuntimeError, RuntimeKind, Screen, StartSpec, TerminalInfo,
};
use pitcrew_interfaces::source::SourceAdapter;
use pitcrew_protocol::events::{Event, EventBody};
use pitcrew_protocol::ids::{CommandId, MachineId, MemberId, SessionId, TerminalId, WorkspaceId};
use pitcrew_protocol::model::{Engine, PermissionMode, Session};
use pitcrew_protocol::runner::{CommandOutcome, EndMode, Key, RunnerCommand};
use pitcrew_runner::{
    RunnerCommands, RunnerConfig, RunnerHandle, RunnerTerminals, SessionEnv, Started, Timing,
};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Longest wait for the watcher thread: a ceiling, not a delay.
const CEILING: Duration = Duration::from_secs(60);

/// `FakeRuntime` that keeps what it was asked to start.
#[derive(Debug, Default)]
struct Recording {
    inner: FakeRuntime,
    started: Mutex<Vec<StartSpec>>,
}

impl Runtime for Recording {
    fn kind(&self) -> RuntimeKind {
        self.inner.kind()
    }
    fn start(&self, spec: &StartSpec) -> Result<TerminalInfo, RuntimeError> {
        self.started.lock().unwrap().push(spec.clone());
        self.inner.start(spec)
    }
    fn write(&self, id: TerminalId, bytes: &[u8]) -> Result<(), RuntimeError> {
        self.inner.write(id, bytes)
    }
    fn send_keys(&self, id: TerminalId, keys: &[Key]) -> Result<(), RuntimeError> {
        self.inner.send_keys(id, keys)
    }
    fn resize(&self, id: TerminalId, cols: u16, rows: u16) -> Result<(), RuntimeError> {
        self.inner.resize(id, cols, rows)
    }
    fn screen(&self, id: TerminalId) -> Result<Screen, RuntimeError> {
        self.inner.screen(id)
    }
    fn read_output(
        &self,
        id: TerminalId,
        from: u64,
        max: usize,
    ) -> Result<OutputChunk, RuntimeError> {
        self.inner.read_output(id, from, max)
    }
    fn info(&self, id: TerminalId) -> Result<TerminalInfo, RuntimeError> {
        self.inner.info(id)
    }
    fn list(&self) -> Result<Vec<TerminalInfo>, RuntimeError> {
        self.inner.list()
    }
    fn kill(&self, id: TerminalId) -> Result<(), RuntimeError> {
        self.inner.kill(id)
    }
}

/// Gives every named session's CLI a token file's path (never a token), or refuses.
#[derive(Debug, Default)]
struct TokenFiles {
    refuse: bool,
    asked: Mutex<Vec<SessionId>>,
}

impl SessionEnv for TokenFiles {
    fn env_for(&self, session: SessionId) -> Result<Vec<(String, String)>, String> {
        self.asked.lock().unwrap().push(session);
        if self.refuse {
            return Err("the session's agent has no owner".into());
        }
        Ok(vec![(
            "PITCREW_TOKEN_FILE".into(),
            format!("/state/agents/{session}.token"),
        )])
    }
}

struct Rig {
    runner: RunnerHandle,
    sink: Arc<CollectSink>,
    terminals: RunnerTerminals,
    commands: RunnerCommands,
    runtime: Arc<Recording>,
    env: Arc<TokenFiles>,
}

impl Rig {
    fn new(engine: Engine, home: &Path, state: &Path, env: TokenFiles) -> Self {
        let adapter: Arc<dyn SourceAdapter> = match engine {
            Engine::Claude => Arc::new(ClaudeAdapter::new()),
            Engine::Codex => Arc::new(CodexAdapter::new()),
            _ => Arc::new(OpenCodeAdapter::new()),
        };
        let env = Arc::new(env);
        let mut config =
            RunnerConfig::new(WorkspaceId::new(), MachineId::new(), MemberId::new(), state)
                .with_home(engine, home)
                .with_session_env(env.clone());
        config.timing = Timing {
            cold_interval: Duration::from_secs(600),
            rediscover_interval: Duration::from_secs(600),
            ..Timing::default()
        };
        let sink = Arc::new(CollectSink::default());
        let runner = pitcrew_runner::start(config, vec![adapter], sink.clone()).unwrap();
        let runtime = Arc::new(Recording::default());
        let terminals = runner.terminals(runtime.clone()).unwrap();
        let commands = runner.commands(&terminals);
        Self {
            runner,
            sink,
            terminals,
            commands,
            runtime,
            env,
        }
    }

    fn run(&self, command: &RunnerCommand) -> CommandOutcome {
        self.commands.run(CommandId::new(), command)
    }

    /// Starts `engine` in `cwd`, for `session` when given: the terminal it runs in, and the CLI id
    /// it was started with (Claude's).
    fn start(
        &self,
        engine: Engine,
        cwd: &Path,
        session: Option<SessionId>,
    ) -> (TerminalId, Option<String>) {
        let outcome = self.run(&start(engine, cwd, session));
        let CommandOutcome::Ok {
            detail: Some(detail),
        } = &outcome
        else {
            panic!("not started: {outcome:?}");
        };
        if let Some(session) = session {
            assert_eq!(detail["session"], serde_json::json!(session));
        }
        (
            serde_json::from_value(detail["terminal"].clone()).unwrap(),
            detail["native_id"].as_str().map(str::to_owned),
        )
    }

    /// Waits until the session whose CLI id is `native` is discovered, and returns it.
    fn discovered(&self, native: &str) -> Session {
        assert!(
            common::eventually(CEILING, || {
                self.runner.rescan();
                discovered_as(&self.sink.events(), native).is_some()
            }),
            "never discovered: {:?}",
            labels(&self.sink.events())
        );
        discovered_as(&self.sink.events(), native).unwrap()
    }
}

fn start(engine: Engine, cwd: &Path, session: Option<SessionId>) -> RunnerCommand {
    RunnerCommand::StartSession {
        engine,
        cwd: cwd.to_str().unwrap().to_owned(),
        name: "PAP-5 Seed runs".into(),
        brief: Some("Submit the seeds".into()),
        persona: None,
        model: None,
        account: None,
        permission_mode: PermissionMode::Default,
        session,
    }
}

fn discovered_as(events: &[Event], native: &str) -> Option<Session> {
    events.iter().find_map(|e| match &e.body {
        EventBody::SessionDiscovered { session } if session.native_id == native => {
            Some(session.clone())
        }
        _ => None,
    })
}

/// Every session id the events name.
fn sessions_named(events: &[Event]) -> Vec<SessionId> {
    let mut ids: Vec<SessionId> = events.iter().filter_map(common::session_of).collect();
    ids.sort_unstable();
    ids.dedup();
    ids
}

fn discoveries(events: &[Event]) -> usize {
    events
        .iter()
        .filter(|e| matches!(e.body, EventBody::SessionDiscovered { .. }))
        .count()
}

fn rejected(outcome: &CommandOutcome, says: &str) -> bool {
    matches!(outcome, CommandOutcome::Rejected { reason } if reason.contains(says))
}

/// Now, as RFC 3339 with milliseconds, as Codex writes it.
fn rfc3339_now() -> String {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis();
    let ms = i64::try_from(ms).unwrap();
    let (days, rest) = (ms.div_euclid(86_400_000), ms.rem_euclid(86_400_000));
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        rest / 3_600_000,
        rest / 60_000 % 60,
        rest / 1000 % 60,
        rest % 1000
    )
}

/// A Codex rollout as Codex writes it when started with a prompt: its session, in `cwd`, started
/// now. Placed whole under `home/sessions`.
fn place_codex(home: &Path, native: &str, cwd: &Path) -> PathBuf {
    let now = rfc3339_now();
    let dir = home.join("sessions").join("2026").join("10").join("03");
    std::fs::create_dir_all(&dir).unwrap();
    let meta = serde_json::json!({
        "timestamp": now,
        "type": "session_meta",
        "payload": {
            "id": native,
            "timestamp": now,
            "cwd": cwd.to_str().unwrap(),
            "originator": "codex_cli_rs",
            "cli_version": "0.50.0",
        },
    });
    let prompt = serde_json::json!({
        "timestamp": now,
        "type": "response_item",
        "payload": {
            "type": "message",
            "role": "user",
            "content": [{ "type": "input_text", "text": "Submit the seeds" }],
        },
    });
    let path = dir.join(format!("rollout-{native}.jsonl"));
    place(&path, format!("{meta}\n{prompt}\n").as_bytes());
    path
}

/// OpenCode's store, as OpenCode keeps it, with one session in `cwd` started now.
fn place_opencode(home: &Path, native: &str, cwd: &Path) {
    let schema = std::fs::read_to_string(
        pitcrew_fixtures::data_dir().join("transcripts/opencode/schema.sql"),
    )
    .unwrap();
    let part = home.join("opencode.part");
    let db = rusqlite::Connection::open(&part).unwrap();
    db.execute_batch(&schema).unwrap();
    let now = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap();
    db.execute(
        "INSERT INTO session VALUES (?1, 'prj_synthetic', NULL, ?2, 'Seed runs', '1.0.0', ?3, ?3)",
        rusqlite::params![native, cwd.to_str().unwrap(), now],
    )
    .unwrap();
    db.execute(
        "INSERT INTO message VALUES ('msg_1', ?1, ?2, ?2, '{\"role\":\"user\"}')",
        rusqlite::params![native, now],
    )
    .unwrap();
    db.execute(
        "INSERT INTO part VALUES ('prt_1', 'msg_1', ?1, ?2, ?2,
           '{\"type\":\"text\",\"text\":\"Submit the seeds\"}')",
        rusqlite::params![native, now],
    )
    .unwrap();
    drop(db);
    std::fs::rename(&part, home.join("opencode.db")).unwrap();
}

/// Claude, started for a named session: the terminal is the session's at once, its CLI gets the
/// token file's path, and its transcript is reported under the name, once. A sub-agent found
/// before it keeps an id of its own, with the named session as its parent.
#[test]
fn a_named_claude_session_is_reported_once_under_its_name() {
    let home = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let r = Rig::new(
        Engine::Claude,
        home.path(),
        state.path(),
        TokenFiles::default(),
    );
    let named = SessionId::new();

    assert_eq!(r.commands.started(named), Started::Gone);
    let (terminal, native) = r.start(Engine::Claude, work.path(), Some(named));
    let native = native.unwrap();
    assert_eq!(r.terminals.terminal_of(named).unwrap(), Some(terminal));
    assert_eq!(r.terminals.session_of(terminal).unwrap(), Some(named));
    assert_eq!(r.commands.started(named), Started::Running);
    // Commands reach it before its transcript exists.
    let send = RunnerCommand::SendText {
        session: named,
        text: "go".into(),
    };
    assert_eq!(r.run(&send), CommandOutcome::Ok { detail: None });
    // The CLI was given the token file's path for its session, and no token.
    assert_eq!(*r.env.asked.lock().unwrap(), [named]);
    let spec = r.runtime.started.lock().unwrap()[0].clone();
    assert!(
        spec.env.contains(&(
            "PITCREW_TOKEN_FILE".into(),
            format!("/state/agents/{named}.token")
        )),
        "{:?}",
        spec.env
    );
    assert!(spec.env.iter().all(|(k, _)| k != "PITCREW_TOKEN"));

    // The CLI's transcript and a sub-agent's appear together, the sub-agent's newer, so it is
    // indexed first: its parent, not indexed yet, is the named session already. (Made aside and
    // moved into the home at once, so no discovery sees one without the other.)
    let staging = tempfile::tempdir().unwrap();
    let project = staging.path().join("-w-paper");
    let main = project.join(format!("{native}.jsonl"));
    let sub_dir = project.join(&native).join("subagents");
    std::fs::create_dir_all(&sub_dir).unwrap();
    std::fs::write(&main, fixture_lines_as(&native)[..3].concat()).unwrap();
    common::age(&main, Duration::from_secs(60));
    let sub = String::from_utf8(fixture_lines_as(&native)[..3].concat())
        .unwrap()
        .replace(
            r#""isSidechain":false"#,
            r#""isSidechain":true,"agentId":"agent-a1""#,
        );
    std::fs::write(sub_dir.join("agent-a1.jsonl"), sub).unwrap();
    let projects = home.path().join("projects");
    std::fs::create_dir_all(&projects).unwrap();
    std::fs::rename(&project, projects.join("-w-paper")).unwrap();
    let child = r.discovered("agent-a1");
    assert_eq!(child.parent, Some(named));
    assert_ne!(child.id, named);

    // The CLI's own transcript is the named session.
    let session = r.discovered(&native);
    assert_eq!(session.id, named);
    assert_eq!(session.terminal, Some(terminal));
    assert_eq!(session.agent, None, "re-statements name no agent");
    assert_eq!(r.commands.started(named), Started::Reported);
    std::thread::sleep(Duration::from_millis(300));
    let events = r.sink.events();
    assert_eq!(discoveries(&events), 2, "{:?}", labels(&events));
    assert_eq!(sessions_named(&events), {
        let mut ids = vec![named, child.id];
        ids.sort_unstable();
        ids
    });

    // It is not started again.
    assert!(rejected(
        &r.run(&start(Engine::Claude, work.path(), Some(named))),
        "already known"
    ));
    assert_eq!(r.runtime.list().unwrap().len(), 1);

    // After a restart it keeps the name.
    let Rig {
        runner,
        terminals,
        commands,
        ..
    } = r;
    runner.stop();
    drop((terminals, commands));
    let r = Rig::new(
        Engine::Claude,
        home.path(),
        state.path(),
        TokenFiles::default(),
    );
    assert_eq!(r.commands.started(named), Started::Reported);
    r.runner.stop();
}

/// Codex, started for a named session: found by folder and start time, its transcript adopts the
/// name. While it waits, a second start in its folder is refused (Codex sessions are told apart
/// only by folder), named or not; once it is found, the folder is free again.
#[test]
fn a_named_codex_session_adopts_its_name_by_folder() {
    let home = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    let r = Rig::new(
        Engine::Codex,
        home.path(),
        state.path(),
        TokenFiles::default(),
    );
    let named = SessionId::new();
    let (terminal, _) = r.start(Engine::Codex, work.path(), Some(named));
    assert_eq!(r.terminals.terminal_of(named).unwrap(), Some(terminal));

    // Waiting: another dispatch there, or a plain start there, could be taken for it.
    for other in [Some(SessionId::new()), None] {
        let refused = r.run(&start(Engine::Codex, work.path(), other));
        assert!(rejected(&refused, "folder"), "{refused:?}");
    }
    // Another folder is fine, and so is Claude, which is matched by its id.
    r.start(Engine::Codex, elsewhere.path(), None);
    r.start(Engine::Claude, work.path(), Some(SessionId::new()));
    assert_eq!(r.runtime.list().unwrap().len(), 3);

    let native = "7c1e9d2a-0b3f-4e6a-8d5c-1f2e3a4b5c6e";
    place_codex(home.path(), native, work.path());
    let session = r.discovered(native);
    assert_eq!(session.id, named);
    assert_eq!(session.terminal, Some(terminal));
    assert_eq!(r.commands.started(named), Started::Reported);
    std::thread::sleep(Duration::from_millis(300));
    let events = r.sink.events();
    assert_eq!(discoveries(&events), 1, "{:?}", labels(&events));
    assert_eq!(sessions_named(&events), [named]);

    // Found: the folder is free for the next dispatch.
    r.start(Engine::Codex, work.path(), Some(SessionId::new()));
    // Two plain starts in one folder are left to the folder match, as before.
    r.start(Engine::Codex, elsewhere.path(), None);
    r.runner.stop();
}

/// OpenCode, started for a named session: found by folder and start time in OpenCode's store,
/// its session adopts the name.
#[test]
fn a_named_opencode_session_adopts_its_name_by_folder() {
    let home = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let r = Rig::new(
        Engine::OpenCode,
        home.path(),
        state.path(),
        TokenFiles::default(),
    );
    let named = SessionId::new();
    let (terminal, _) = r.start(Engine::OpenCode, work.path(), Some(named));
    assert!(rejected(
        &r.run(&start(Engine::OpenCode, work.path(), None)),
        "folder"
    ));

    let native = "ses_01jbsynthetic000000000001";
    place_opencode(home.path(), native, work.path());
    let session = r.discovered(native);
    assert_eq!(session.id, named);
    assert_eq!(session.terminal, Some(terminal));
    std::thread::sleep(Duration::from_millis(300));
    let events = r.sink.events();
    assert_eq!(discoveries(&events), 1, "{:?}", labels(&events));
    assert_eq!(sessions_named(&events), [named]);
    r.runner.stop();
}

/// A named session whose CLI ends before its transcript appears is gone; a start the session's
/// environment cannot be made for is refused, and nothing runs.
#[test]
fn a_named_start_that_never_ran_is_gone_and_one_without_its_environment_is_refused() {
    let home = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let r = Rig::new(
        Engine::Codex,
        home.path(),
        state.path(),
        TokenFiles::default(),
    );
    let named = SessionId::new();
    r.start(Engine::Codex, work.path(), Some(named));
    assert_eq!(r.commands.started(named), Started::Running);
    let kill = RunnerCommand::EndSession {
        session: named,
        mode: EndMode::Kill,
    };
    assert_eq!(r.run(&kill), CommandOutcome::Ok { detail: None });
    assert_eq!(r.commands.started(named), Started::Gone);
    // Its program ended: the folder is free.
    r.start(Engine::Codex, work.path(), Some(SessionId::new()));
    r.runner.stop();

    let state = tempfile::tempdir().unwrap();
    let r = Rig::new(
        Engine::Claude,
        home.path(),
        state.path(),
        TokenFiles {
            refuse: true,
            ..TokenFiles::default()
        },
    );
    let refused = r.run(&start(Engine::Claude, work.path(), Some(SessionId::new())));
    assert!(rejected(&refused, "no owner"), "{refused:?}");
    assert!(r.runtime.list().unwrap().is_empty());
    // A start for no named session asks for no environment.
    r.start(Engine::Claude, work.path(), None);
    assert_eq!(r.env.asked.lock().unwrap().len(), 1);
    assert!(
        r.runtime.started.lock().unwrap()[0]
            .env
            .iter()
            .all(|(k, _)| !k.starts_with("PITCREW_"))
    );
    r.runner.stop();
}

/// A runtime whose program writes its transcript at once and is found before `start` returns, so
/// the runner records the terminal only after the transcript was discovered.
#[derive(Debug)]
struct Eager {
    inner: Recording,
    home: PathBuf,
    sink: Arc<CollectSink>,
}

impl Runtime for Eager {
    fn kind(&self) -> RuntimeKind {
        self.inner.kind()
    }
    fn start(&self, spec: &StartSpec) -> Result<TerminalInfo, RuntimeError> {
        let info = self.inner.start(spec)?;
        if let Some(native) = spec
            .args
            .iter()
            .find_map(|a| a.strip_prefix("--session-id="))
        {
            let dir = self.home.join("projects").join("-eager");
            std::fs::create_dir_all(&dir).unwrap();
            place(
                &dir.join(format!("{native}.jsonl")),
                &fixture_lines_as(native)[..3].concat(),
            );
            assert!(
                common::eventually(CEILING, || discovered_as(&self.sink.events(), native)
                    .is_some()),
                "the transcript was not found while the start was under way"
            );
        }
        Ok(info)
    }
    fn write(&self, id: TerminalId, bytes: &[u8]) -> Result<(), RuntimeError> {
        self.inner.write(id, bytes)
    }
    fn send_keys(&self, id: TerminalId, keys: &[Key]) -> Result<(), RuntimeError> {
        self.inner.send_keys(id, keys)
    }
    fn resize(&self, id: TerminalId, cols: u16, rows: u16) -> Result<(), RuntimeError> {
        self.inner.resize(id, cols, rows)
    }
    fn screen(&self, id: TerminalId) -> Result<Screen, RuntimeError> {
        self.inner.screen(id)
    }
    fn read_output(
        &self,
        id: TerminalId,
        from: u64,
        max: usize,
    ) -> Result<OutputChunk, RuntimeError> {
        self.inner.read_output(id, from, max)
    }
    fn info(&self, id: TerminalId) -> Result<TerminalInfo, RuntimeError> {
        self.inner.info(id)
    }
    fn list(&self) -> Result<Vec<TerminalInfo>, RuntimeError> {
        self.inner.list()
    }
    fn kill(&self, id: TerminalId) -> Result<(), RuntimeError> {
        self.inner.kill(id)
    }
}

/// A CLI whose transcript is found before the runner has recorded its terminal (a start still
/// under way) still takes the session the hub named, and its terminal is linked once recorded.
#[test]
fn a_transcript_found_while_its_start_is_under_way_takes_the_name() {
    let home = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join("projects")).unwrap();
    let mut config = RunnerConfig::new(
        WorkspaceId::new(),
        MachineId::new(),
        MemberId::new(),
        state.path(),
    )
    .with_home(Engine::Claude, home.path());
    // Discovery looks often, so the transcript is found while `start` waits for it.
    config.timing = Timing {
        rediscover_interval: Duration::from_millis(100),
        ..Timing::default()
    };
    let sink = Arc::new(CollectSink::default());
    let runner =
        pitcrew_runner::start(config, vec![Arc::new(ClaudeAdapter::new())], sink.clone()).unwrap();
    let runtime = Arc::new(Eager {
        inner: Recording::default(),
        home: home.path().to_path_buf(),
        sink: sink.clone(),
    });
    let terminals = runner.terminals(runtime).unwrap();
    let commands = runner.commands(&terminals);
    let named = SessionId::new();
    let outcome = commands.run(
        CommandId::new(),
        &start(Engine::Claude, work.path(), Some(named)),
    );
    let CommandOutcome::Ok {
        detail: Some(detail),
    } = &outcome
    else {
        panic!("not started: {outcome:?}");
    };
    let native = detail["native_id"].as_str().unwrap();
    let terminal: TerminalId = serde_json::from_value(detail["terminal"].clone()).unwrap();
    let session = discovered_as(&sink.events(), native).unwrap();
    assert_eq!(session.id, named, "{:?}", labels(&sink.events()));
    assert_eq!(
        session.terminal, None,
        "found before its terminal was recorded"
    );
    assert_eq!(terminals.terminal_of(named).unwrap(), Some(terminal));
    assert_eq!(commands.started(named), Started::Reported);
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(sessions_named(&sink.events()), [named]);
    drop((terminals, commands));
    runner.stop();
}
