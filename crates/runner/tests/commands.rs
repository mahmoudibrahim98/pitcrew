//! Hub commands in process (`RunnerCommands`) over `FakeRuntime`, with the real Claude adapter:
//! idempotent by command id (also across a restart), a started session linked to its terminal,
//! text, keys, interrupts and ends.

#![allow(clippy::unwrap_used)]

mod common;

use common::{CollectSink, claude_file, config, discovered, fixture_lines_as, labels, place};
use pitcrew_api::Terminals as _;
use pitcrew_ingest::claude::ClaudeAdapter;
use pitcrew_interfaces::fake::FakeRuntime;
use pitcrew_interfaces::runtime::{
    OutputChunk, Runtime, RuntimeError, RuntimeKind, Screen, StartSpec, TerminalInfo,
};
use pitcrew_protocol::ids::{CommandId, SessionId, TerminalId};
use pitcrew_protocol::model::{Engine, PermissionMode};
use pitcrew_protocol::runner::{CommandOutcome, EndMode, Key, RunnerCommand};
use pitcrew_runner::{CommandOptions, RunnerCommands, RunnerHandle, RunnerTerminals};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Longest wait for the watcher thread. Waits end as soon as what they wait for happens; this is
/// only a ceiling, generous so a loaded machine is slow, not a failure.
const CEILING: Duration = Duration::from_secs(60);

/// `FakeRuntime` that records what it started, and whose programs exit on Ctrl-C when asked to.
#[derive(Debug, Default)]
struct Recording {
    inner: FakeRuntime,
    started: Mutex<Vec<StartSpec>>,
    exits_on_ctrl_c: bool,
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
        self.inner.send_keys(id, keys)?;
        if self.exits_on_ctrl_c && keys.contains(&Key::CtrlC) {
            self.inner.kill(id)?;
        }
        Ok(())
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

struct Rig {
    runner: RunnerHandle,
    sink: Arc<CollectSink>,
    terminals: RunnerTerminals,
    commands: RunnerCommands,
}

fn rig(home: &Path, state: &Path, runtime: Arc<Recording>, options: CommandOptions) -> Rig {
    let sink = Arc::new(CollectSink::default());
    let runner = pitcrew_runner::start(
        config(home, state),
        vec![Arc::new(ClaudeAdapter::new())],
        sink.clone(),
    )
    .unwrap();
    let terminals = runner.terminals(runtime).unwrap();
    let commands = runner.commands_with(&terminals, options);
    Rig {
        runner,
        sink,
        terminals,
        commands,
    }
}

fn start_claude(cwd: &Path) -> RunnerCommand {
    RunnerCommand::StartSession {
        engine: Engine::Claude,
        cwd: cwd.to_str().unwrap().to_owned(),
        name: "writer".into(),
        brief: Some("Draft section 3".into()),
        persona: None,
        model: None,
        account: None,
        permission_mode: PermissionMode::Default,
    }
}

fn detail(outcome: &CommandOutcome) -> serde_json::Value {
    match outcome {
        CommandOutcome::Ok {
            detail: Some(detail),
        } => detail.clone(),
        other => panic!("not ok: {other:?}"),
    }
}

fn ok() -> CommandOutcome {
    CommandOutcome::Ok { detail: None }
}

#[test]
fn a_started_session_is_linked_to_its_terminal_and_driven_through_it() {
    let home = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let runtime = Arc::new(Recording::default());
    let r = rig(
        home.path(),
        state.path(),
        runtime.clone(),
        CommandOptions::default(),
    );

    // Started once, however often the command is sent.
    let id = CommandId::new();
    let start = start_claude(work.path());
    let first = r.commands.run(id, &start);
    assert_eq!(r.commands.run(id, &start), first);
    assert_eq!(runtime.list().unwrap().len(), 1);
    let detail = detail(&first);
    let terminal: TerminalId = serde_json::from_value(detail["terminal"].clone()).unwrap();
    let native = detail["native_id"].as_str().unwrap().to_owned();
    let spec = runtime.started.lock().unwrap()[0].clone();
    assert_eq!(spec.program, "claude");
    assert_eq!(
        spec.args,
        [
            format!("--session-id={native}").as_str(),
            "--",
            "Draft section 3"
        ]
    );

    // The CLI writes its transcript: the session is discovered in its terminal. Its folder is
    // new, so the folder watches may or may not see it in time; a rescan finds it either way,
    // and the file appears whole, so whichever finds it reads all of it.
    place(
        &claude_file(home.path(), &native),
        &fixture_lines_as(&native)[..3].concat(),
    );
    r.runner.rescan();
    r.sink.wait_for(2, CEILING).expect("discovery");
    let session = discovered(&r.sink.events());
    assert_eq!(session.native_id, native);
    assert_eq!(session.terminal, Some(terminal));
    assert_eq!(r.terminals.terminal_of(session.id).unwrap(), Some(terminal));
    assert!(r.terminals.attach(session.id).is_ok());

    // Text, keys and interrupts reach the terminal.
    let s = session.id;
    let send = |command| r.commands.run(CommandId::new(), &command);
    assert_eq!(
        send(RunnerCommand::SendText {
            session: s,
            text: "compare both".into()
        }),
        ok()
    );
    assert_eq!(
        send(RunnerCommand::SendKeys {
            session: s,
            keys: vec![Key::Down, Key::Enter]
        }),
        ok()
    );
    assert_eq!(send(RunnerCommand::Interrupt { session: s }), ok());
    assert_eq!(
        runtime.inner.read_output(terminal, 0, 100).unwrap().data,
        b"compare both"
    );
    assert_eq!(
        runtime.inner.keys(terminal),
        [Key::Enter, Key::Down, Key::Enter, Key::Escape]
    );

    // Killing it ends the session at once.
    let end = CommandId::new();
    let kill = RunnerCommand::EndSession {
        session: s,
        mode: EndMode::Kill,
    };
    assert_eq!(r.commands.run(end, &kill), ok());
    assert!(!runtime.info(terminal).unwrap().alive);
    assert!(common::eventually(CEILING, || labels(&r.sink.events())
        .contains(&"ended".to_owned())));
    std::thread::sleep(Duration::from_millis(200));
    let events = r.sink.events();
    let tail = labels(&events[events.len() - 2..]);
    assert_eq!(tail, ["state:Ended", "ended"]);

    // After a restart the same command ids answer the same, and nothing runs again.
    let Rig {
        runner,
        terminals,
        commands,
        ..
    } = r;
    runner.stop();
    drop((terminals, commands));
    let r = rig(
        home.path(),
        state.path(),
        runtime.clone(),
        CommandOptions::default(),
    );
    assert_eq!(r.commands.run(id, &start), first);
    assert_eq!(r.commands.run(end, &kill), ok());
    assert_eq!(runtime.list().unwrap().len(), 1);
    assert_eq!(runtime.started.lock().unwrap().len(), 1);
    r.runner.stop();
}

#[test]
fn the_same_command_sent_twice_at_once_runs_once() {
    let home = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let runtime = Arc::new(Recording::default());
    let r = rig(
        home.path(),
        state.path(),
        runtime.clone(),
        CommandOptions::default(),
    );
    let id = CommandId::new();
    let start = start_claude(work.path());
    let outcomes: Vec<CommandOutcome> = std::thread::scope(|scope| {
        let runs: Vec<_> = (0..8)
            .map(|_| scope.spawn(|| r.commands.run(id, &start)))
            .collect();
        runs.into_iter().map(|h| h.join().unwrap()).collect()
    });
    assert!(outcomes.iter().all(|o| *o == outcomes[0]));
    assert_eq!(runtime.started.lock().unwrap().len(), 1);
    r.runner.stop();
}

#[test]
fn a_graceful_end_waits_for_the_cli_to_exit() {
    let home = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let options = CommandOptions {
        graceful_wait: Duration::from_millis(300),
        ..CommandOptions::default()
    };

    for exits in [true, false] {
        let state = state.path().join(exits.to_string());
        let runtime = Arc::new(Recording {
            exits_on_ctrl_c: exits,
            ..Recording::default()
        });
        let r = rig(home.path(), &state, runtime.clone(), options.clone());
        let started = r.commands.run(CommandId::new(), &start_claude(work.path()));
        let terminal: TerminalId =
            serde_json::from_value(detail(&started)["terminal"].clone()).unwrap();
        let session = SessionId::new();
        r.terminals.link(session, terminal).unwrap();
        let end = r.commands.run(
            CommandId::new(),
            &RunnerCommand::EndSession {
                session,
                mode: EndMode::Graceful,
            },
        );
        assert_eq!(runtime.inner.keys(terminal), [Key::CtrlC, Key::CtrlC]);
        if exits {
            assert_eq!(end, ok());
        } else {
            assert!(
                matches!(&end, CommandOutcome::Failed { error } if error.contains("kill")),
                "{end:?}"
            );
            assert!(runtime.info(terminal).unwrap().alive);
        }
        r.runner.stop();
    }
}

#[test]
fn refusals_and_failures() {
    let home = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let runtime = Arc::new(Recording::default());
    let r = rig(
        home.path(),
        state.path(),
        runtime.clone(),
        CommandOptions::default(),
    );
    let run = |command: RunnerCommand| r.commands.run(CommandId::new(), &command);
    let rejected = |o: &CommandOutcome| matches!(o, CommandOutcome::Rejected { .. });

    let mut bypass = start_claude(work.path());
    if let RunnerCommand::StartSession {
        permission_mode, ..
    } = &mut bypass
    {
        *permission_mode = PermissionMode::BypassPermissions;
    }
    assert!(rejected(&run(bypass)), "bypass needs an opt-in");

    // A transcript can name any session id: one that reads as an option is refused, for every
    // CLI, and so is such a model. Nothing starts.
    for engine in [Engine::Claude, Engine::Codex, Engine::OpenCode] {
        for flag in ["--dangerously-skip-permissions", "-p"] {
            let resume = RunnerCommand::ResumeSession {
                engine,
                native_id: flag.into(),
                cwd: work.path().to_str().unwrap().to_owned(),
                name: "writer".into(),
            };
            assert!(rejected(&run(resume)), "{engine:?} resume {flag}");
            let mut model = start_claude(work.path());
            if let RunnerCommand::StartSession {
                engine: e,
                model: m,
                ..
            } = &mut model
            {
                *e = engine;
                *m = Some(flag.into());
            }
            assert!(rejected(&run(model)), "{engine:?} model {flag}");
        }
    }
    assert!(runtime.started.lock().unwrap().is_empty());

    let missing = start_claude(&work.path().join("missing"));
    assert!(rejected(&run(missing)), "the folder must exist");

    let mut account = start_claude(work.path());
    if let RunnerCommand::StartSession { account: a, .. } = &mut account {
        *a = Some("/not/a/home".into());
    }
    assert!(rejected(&run(account)), "only configured homes");

    // The configured home is an account.
    let mut known = start_claude(work.path());
    if let RunnerCommand::StartSession { account: a, .. } = &mut known {
        *a = Some(home.path().to_str().unwrap().to_owned());
    }
    assert!(matches!(run(known), CommandOutcome::Ok { .. }));
    let env = runtime.started.lock().unwrap()[0].env.clone();
    assert_eq!(env[0].0, "CLAUDE_CONFIG_DIR");

    let none = run(RunnerCommand::Interrupt {
        session: SessionId::new(),
    });
    assert!(matches!(none, CommandOutcome::Failed { .. }), "{none:?}");
    assert!(rejected(&run(RunnerCommand::ReadTerminal {
        terminal: TerminalId::new(),
        from_offset: 0,
        max_bytes: 10
    })));
    assert_eq!(run(RunnerCommand::Scan { roots: vec![] }), ok());
    r.runner.stop();
}
