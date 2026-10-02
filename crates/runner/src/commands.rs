//! Hub commands, in-process form (work package 5): [`RunnerCommands::run`].
//!
//! - **Idempotent by `CommandId`:** an outcome is saved in the runner's index before `run`
//!   returns, and a command id seen again (also after a restart, for a week) returns that outcome
//!   without running anything. The same id sent twice at once runs once; the second call waits.
//! - **Starting a session** starts its CLI in a new terminal. The runner learns the session's id
//!   only when its transcript appears; the terminal is then linked to it, and its
//!   `session_discovered` names the terminal. Claude is started with a session id chosen here, so
//!   the match is exact; other CLIs are matched by folder and start time.
//! - Text, keys, interrupts and ends go to the session's terminal. Ending a session reports it
//!   ended (`session_state_changed` and `session_ended`) at once.

use crate::config::EngineHome;
use crate::derive::Reported;
use crate::plain;
use crate::store::TerminalRow;
use crate::terminals::RunnerTerminals;
use crate::watch::{Origin, Shared, Signal, Target};
use pitcrew_api::terminal::TerminalError;
use pitcrew_interfaces::runtime::StartSpec;
use pitcrew_protocol::ids::{CommandId, SessionId, TerminalId};
use pitcrew_protocol::model::{Engine, PermissionMode, SessionState};
use pitcrew_protocol::runner::{CommandOutcome, EndMode, Key, RunnerCommand};
use std::collections::HashSet;
use std::fmt;
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// Tuning and policy for [`RunnerCommands`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandOptions {
    /// Whether `bypass_permissions` may be used. Off by default; each use is logged as a warning.
    pub allow_bypass_permissions: bool,
    /// How long a graceful end waits for the CLI to exit.
    pub graceful_wait: Duration,
    /// Size of new terminals, in columns.
    pub cols: u16,
    /// Size of new terminals, in rows.
    pub rows: u16,
}

impl Default for CommandOptions {
    fn default() -> Self {
        Self {
            allow_bypass_permissions: false,
            graceful_wait: Duration::from_secs(10),
            cols: 120,
            rows: 40,
        }
    }
}

/// Runs hub commands on this machine. Cheap to clone. Get it from
/// [`RunnerHandle::commands`](crate::RunnerHandle::commands).
#[derive(Clone)]
pub struct RunnerCommands {
    inner: Arc<Inner>,
}

struct Inner {
    terminals: RunnerTerminals,
    shared: Arc<Shared>,
    homes: Vec<EngineHome>,
    options: CommandOptions,
    running: Mutex<HashSet<CommandId>>,
    done: Condvar,
}

impl fmt::Debug for RunnerCommands {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RunnerCommands")
            .field("options", &self.inner.options)
            .finish_non_exhaustive()
    }
}

impl RunnerCommands {
    pub(crate) fn new(
        terminals: RunnerTerminals,
        shared: Arc<Shared>,
        homes: Vec<EngineHome>,
        options: CommandOptions,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                terminals,
                shared,
                homes,
                options,
                running: Mutex::new(HashSet::new()),
                done: Condvar::new(),
            }),
        }
    }

    /// Runs a command once per id, and answers its outcome. Blocking: starting a program or
    /// ending one gracefully can take seconds.
    pub fn run(&self, id: CommandId, command: &RunnerCommand) -> CommandOutcome {
        let inner = &*self.inner;
        {
            let mut running = inner.running.lock().unwrap_or_else(PoisonError::into_inner);
            while running.contains(&id) {
                running = inner
                    .done
                    .wait(running)
                    .unwrap_or_else(PoisonError::into_inner);
            }
            match inner.terminals.store().outcome(id) {
                Ok(Some(outcome)) => return outcome,
                Ok(None) => {}
                Err(e) => return failed(format!("cannot read earlier outcomes: {e}")),
            }
            running.insert(id);
        }
        let _running = Running { inner, id };
        let outcome = self.execute(command);
        if let Err(e) = inner
            .terminals
            .store()
            .save_outcome(id, &outcome, crate::now_ms())
        {
            tracing::error!(command = %id, error = %e, "cannot save a command's outcome; it may run again if sent again");
        }
        outcome
    }

    fn execute(&self, command: &RunnerCommand) -> CommandOutcome {
        match command {
            RunnerCommand::StartSession {
                engine,
                cwd,
                name,
                brief,
                persona: _,
                model,
                account,
                permission_mode,
            } => self.start(&Launch {
                engine: *engine,
                cwd,
                name,
                brief: brief.as_deref(),
                model: model.as_deref(),
                account: account.as_deref(),
                mode: *permission_mode,
                resume: None,
            }),
            RunnerCommand::ResumeSession {
                engine,
                native_id,
                cwd,
                name,
            } => self.start(&Launch {
                engine: *engine,
                cwd,
                name,
                brief: None,
                model: None,
                account: None,
                mode: PermissionMode::Default,
                resume: Some(native_id),
            }),
            RunnerCommand::SendText { session, text } => self.on_terminal(*session, |t, id| {
                t.write(id, text.as_bytes().to_vec())?;
                t.send_keys(id, vec![Key::Enter])
            }),
            RunnerCommand::SendKeys { session, keys } => {
                self.on_terminal(*session, |t, id| t.send_keys(id, keys.clone()))
            }
            RunnerCommand::Interrupt { session } => {
                self.on_terminal(*session, |t, id| t.send_keys(id, vec![Key::Escape]))
            }
            RunnerCommand::EndSession { session, mode } => self.end(*session, *mode),
            RunnerCommand::ResizeTerminal {
                terminal,
                cols,
                rows,
            } => done(self.inner.terminals.resize(*terminal, *cols, *rows)),
            RunnerCommand::ReadTerminal { .. } => {
                rejected("In process, terminal output is read through the terminal API.".into())
            }
            RunnerCommand::Scan { roots } if roots.is_empty() => {
                self.inner.shared.rescan();
                CommandOutcome::Ok { detail: None }
            }
            RunnerCommand::Scan { .. } => rejected(
                "Scanning roots other than the configured homes is not supported yet.".into(),
            ),
            other => rejected(format!("This runner does not know the command {other:?}.")),
        }
    }

    fn start(&self, launch: &Launch<'_>) -> CommandOutcome {
        let inner = &*self.inner;
        if launch.mode == PermissionMode::BypassPermissions
            && !inner.options.allow_bypass_permissions
        {
            return rejected("bypass_permissions is not allowed on this runner.".into());
        }
        if !std::path::Path::new(launch.cwd).is_dir() {
            return rejected(format!("{} is not a folder on this machine.", launch.cwd));
        }
        let env = match self.account_env(launch.engine, launch.account) {
            Ok(env) => env,
            Err(reason) => return rejected(reason),
        };
        // Claude takes the session id it should use; others are matched by folder.
        let native_id = match (launch.resume, launch.engine) {
            (Some(id), _) => Some(id.to_owned()),
            (None, Engine::Claude) => Some(new_uuid()),
            (None, _) => None,
        };
        let spec = match start_spec(launch, native_id.as_deref(), env, &inner.options) {
            Ok(spec) => spec,
            Err(reason) => return rejected(reason),
        };
        let started_at = crate::now_ms();
        let info = match inner.terminals.start(spec) {
            Ok(info) => info,
            Err(e) => return failed(e.to_string()),
        };
        if launch.mode == PermissionMode::BypassPermissions {
            tracing::warn!(terminal = %info.id, cwd = launch.cwd, engine = ?launch.engine, "started a session with permission prompts bypassed");
        }
        // A resumed session may be indexed already: link it now.
        let session = match launch.resume {
            Some(id) => inner
                .terminals
                .store()
                .session_by_native(launch.engine, id)
                .unwrap_or_else(|e| {
                    tracing::warn!(error = %e, "cannot look up the resumed session");
                    None
                }),
            None => None,
        };
        let saved = inner.terminals.store().put_terminal(&TerminalRow {
            terminal: info.id,
            native_target: info.native_target.clone(),
            session,
            engine: Some(launch.engine),
            native_id: native_id.clone(),
            cwd: launch.cwd.to_owned(),
            started_at,
        });
        if let Err(e) = saved {
            return failed(format!(
                "started terminal {} but cannot record it: {e}",
                info.id
            ));
        }
        CommandOutcome::Ok {
            detail: Some(serde_json::json!({
                "terminal": info.id,
                "native_target": info.native_target,
                "native_id": native_id,
                "session": session,
            })),
        }
    }

    /// The environment that selects an account home: one of the configured homes for the CLI.
    fn account_env(
        &self,
        engine: Engine,
        account: Option<&str>,
    ) -> Result<Vec<(String, String)>, String> {
        let Some(account) = account else {
            return Ok(Vec::new());
        };
        let known = self
            .inner
            .homes
            .iter()
            .any(|h| h.engine == engine && h.path.to_str() == Some(account));
        if !known {
            return Err(format!(
                "{account:?} is not a {engine:?} home on this runner."
            ));
        }
        let var = match engine {
            Engine::Claude => "CLAUDE_CONFIG_DIR",
            Engine::Codex => "CODEX_HOME",
            _ => return Err(format!("Accounts are not supported for {engine:?} yet.")),
        };
        Ok(vec![(var.to_owned(), account.to_owned())])
    }

    fn on_terminal(
        &self,
        session: SessionId,
        f: impl FnOnce(&RunnerTerminals, TerminalId) -> Result<(), TerminalError>,
    ) -> CommandOutcome {
        match self.terminal(session) {
            Ok(id) => done(f(&self.inner.terminals, id)),
            Err(outcome) => outcome,
        }
    }

    fn terminal(&self, session: SessionId) -> Result<TerminalId, CommandOutcome> {
        match self.inner.terminals.terminal_of(session) {
            Ok(Some(id)) => Ok(id),
            Ok(None) => Err(failed(format!(
                "Session {session} has no terminal on this runner."
            ))),
            Err(e) => Err(failed(e.to_string())),
        }
    }

    fn end(&self, session: SessionId, mode: EndMode) -> CommandOutcome {
        let id = match self.terminal(session) {
            Ok(id) => id,
            Err(outcome) => return outcome,
        };
        let t = &self.inner.terminals;
        let ended = match mode {
            EndMode::Kill => t.kill(id).map(|()| true),
            // Two interrupts end each supported CLI, mid-turn or not.
            EndMode::Graceful => t
                .send_keys(id, vec![Key::CtrlC, Key::CtrlC])
                .and_then(|()| self.wait_for_exit(id)),
        };
        match ended {
            Ok(true) => {
                self.inner.shared.signal(Signal {
                    target: Target::Session(session),
                    report: Reported {
                        at: crate::now_ms(),
                        to: SessionState::Ended,
                        status_line: None,
                    },
                    // Not a hook: the runner itself ended the session.
                    origin: Origin::Runner,
                });
                CommandOutcome::Ok { detail: None }
            }
            Ok(false) => failed(format!(
                "The session is still running after {:?}; end it with kill.",
                self.inner.options.graceful_wait
            )),
            Err(e) => failed(e.to_string()),
        }
    }

    fn wait_for_exit(&self, id: TerminalId) -> Result<bool, TerminalError> {
        let deadline = Instant::now() + self.inner.options.graceful_wait;
        loop {
            match self.inner.terminals.info(id) {
                Ok(info) if !info.alive => return Ok(true),
                Err(TerminalError::NotFound(_)) => return Ok(true),
                Err(e) => return Err(e),
                Ok(_) => {}
            }
            if Instant::now() >= deadline {
                return Ok(false);
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

/// Marks a command finished, even if it panicked.
struct Running<'a> {
    inner: &'a Inner,
    id: CommandId,
}

impl Drop for Running<'_> {
    fn drop(&mut self) {
        self.inner
            .running
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.id);
        self.inner.done.notify_all();
    }
}

/// What to start.
#[derive(Debug)]
pub(crate) struct Launch<'a> {
    pub engine: Engine,
    pub cwd: &'a str,
    pub name: &'a str,
    pub brief: Option<&'a str>,
    pub model: Option<&'a str>,
    pub account: Option<&'a str>,
    pub mode: PermissionMode,
    /// Resume this CLI session instead of starting a new one.
    pub resume: Option<&'a str>,
}

/// The program and arguments for a launch. `new_id` is the session id to give a new Claude
/// session.
///
/// No value can be read as an option:
/// - session ids and models must be plain (see `plain`), or the launch is refused and the
///   refusal logged: a resumed session's id comes from its transcript;
/// - every option takes its value in the same argument (`--model=<m>`);
/// - free text (the brief) and positional ids come after `--`.
pub(crate) fn start_spec(
    launch: &Launch<'_>,
    new_id: Option<&str>,
    env: Vec<(String, String)>,
    options: &CommandOptions,
) -> Result<StartSpec, String> {
    let resume = launch
        .resume
        .map(|id| checked("session id", id, plain::is_id))
        .transpose()?;
    let new_id = new_id
        .map(|id| checked("session id", id, plain::is_id))
        .transpose()?;
    let model = launch
        .model
        .map(|m| checked("model", m, plain::is_model))
        .transpose()?;
    let mut args: Vec<String> = Vec::new();
    let mode = launch.mode;
    let program = match launch.engine {
        Engine::Claude => {
            match (resume, new_id) {
                (Some(id), _) => args.push(format!("--resume={id}")),
                (None, Some(id)) => args.push(format!("--session-id={id}")),
                (None, None) => {}
            }
            if let Some(m) = model {
                args.push(format!("--model={m}"));
            }
            let flag = match mode {
                PermissionMode::Default => None,
                PermissionMode::AcceptEdits => Some("acceptEdits"),
                PermissionMode::Plan => Some("plan"),
                PermissionMode::BypassPermissions => Some("bypassPermissions"),
            };
            if let Some(f) = flag {
                args.push(format!("--permission-mode={f}"));
            }
            if let Some(b) = launch.brief {
                args.extend(["--".to_owned(), b.to_owned()]);
            }
            "claude"
        }
        Engine::Codex => {
            if resume.is_some() {
                args.push("resume".to_owned());
            }
            if let Some(m) = model {
                args.push(format!("--model={m}"));
            }
            match mode {
                PermissionMode::Default => {}
                PermissionMode::AcceptEdits => args.push("--full-auto".to_owned()),
                PermissionMode::Plan => return Err("Codex has no plan mode.".into()),
                PermissionMode::BypassPermissions => {
                    args.push("--dangerously-bypass-approvals-and-sandbox".to_owned());
                }
            }
            // The session to resume and the brief are positional.
            let positional: Vec<&str> = resume.into_iter().chain(launch.brief).collect();
            if !positional.is_empty() {
                args.push("--".to_owned());
                args.extend(positional.into_iter().map(str::to_owned));
            }
            "codex"
        }
        Engine::OpenCode => {
            if mode != PermissionMode::Default {
                return Err("OpenCode takes its permissions from its own settings.".into());
            }
            if let Some(id) = resume {
                args.push(format!("--session={id}"));
            }
            if let Some(m) = model {
                args.push(format!("--model={m}"));
            }
            if let Some(b) = launch.brief {
                args.push(format!("--prompt={b}"));
            }
            "opencode"
        }
        other => return Err(format!("This runner cannot start {other:?} sessions.")),
    };
    Ok(StartSpec {
        program: program.to_owned(),
        args,
        cwd: launch.cwd.to_owned(),
        env,
        name: launch.name.to_owned(),
        cols: options.cols,
        rows: options.rows,
    })
}

/// `value` if `is_plain` accepts it; otherwise why the launch is refused, logged as a warning.
fn checked<'a>(what: &str, value: &'a str, is_plain: fn(&str) -> bool) -> Result<&'a str, String> {
    if is_plain(value) {
        return Ok(value);
    }
    let shown: String = value.chars().take(64).collect();
    tracing::warn!(what, value = ?shown, "refused to start a session: a value is not plain and could be read as an option");
    Err(format!(
        "The {what} {shown:?} is not allowed: it must start with a letter or digit and hold \
         at most {} plain characters.",
        plain::MAX_LEN
    ))
}

/// A random UUID (version 4), as Claude's `--session-id` wants.
fn new_uuid() -> String {
    // A ULID's random part has 80 bits; two of them fill 128.
    let bits = (ulid::Ulid::generate().random() << 48) ^ ulid::Ulid::generate().random();
    let mut b = bits.to_be_bytes();
    // Version 4, variant 10xx.
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    )
}

fn done(r: Result<(), TerminalError>) -> CommandOutcome {
    match r {
        Ok(()) => CommandOutcome::Ok { detail: None },
        Err(e) => failed(e.to_string()),
    }
}

fn failed(error: String) -> CommandOutcome {
    CommandOutcome::Failed { error }
}

fn rejected(reason: String) -> CommandOutcome {
    CommandOutcome::Rejected { reason }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn launch(engine: Engine, mode: PermissionMode) -> Launch<'static> {
        Launch {
            engine,
            cwd: "/w",
            name: "writer",
            brief: Some("-v means verbose"),
            model: Some("m1"),
            account: None,
            mode,
            resume: None,
        }
    }

    fn args(l: &Launch<'_>, id: Option<&str>) -> Result<(String, Vec<String>), String> {
        start_spec(l, id, Vec::new(), &CommandOptions::default()).map(|s| (s.program, s.args))
    }

    fn strings<const N: usize>(a: [&str; N]) -> Vec<String> {
        a.map(String::from).to_vec()
    }

    #[test]
    fn launches_per_cli() {
        let claude = args(
            &launch(Engine::Claude, PermissionMode::AcceptEdits),
            Some("u1"),
        );
        assert_eq!(
            claude,
            Ok((
                "claude".into(),
                strings([
                    "--session-id=u1",
                    "--model=m1",
                    "--permission-mode=acceptEdits",
                    "--",
                    "-v means verbose"
                ])
            ))
        );
        let mut resume = launch(Engine::Claude, PermissionMode::Default);
        resume.brief = None;
        resume.model = None;
        resume.resume = Some("old");
        assert_eq!(
            args(&resume, Some("old")).map(|a| a.1),
            Ok(strings(["--resume=old"]))
        );
        resume.engine = Engine::Codex;
        assert_eq!(
            args(&resume, None).map(|a| a.1),
            Ok(strings(["resume", "--", "old"]))
        );
        assert_eq!(
            args(&launch(Engine::Codex, PermissionMode::AcceptEdits), None).map(|a| a.1),
            Ok(strings([
                "--model=m1",
                "--full-auto",
                "--",
                "-v means verbose"
            ]))
        );
        assert!(args(&launch(Engine::Codex, PermissionMode::Plan), None).is_err());
        assert_eq!(
            args(
                &launch(Engine::Codex, PermissionMode::BypassPermissions),
                None
            )
            .map(|a| a
                .1
                .contains(&"--dangerously-bypass-approvals-and-sandbox".into())),
            Ok(true)
        );
        assert!(args(&launch(Engine::OpenCode, PermissionMode::AcceptEdits), None).is_err());
        let mut opencode = launch(Engine::OpenCode, PermissionMode::Default);
        opencode.model = Some("anthropic/claude-sonnet-4");
        opencode.resume = Some("ses_01");
        assert_eq!(
            args(&opencode, None),
            Ok((
                "opencode".into(),
                strings([
                    "--session=ses_01",
                    "--model=anthropic/claude-sonnet-4",
                    "--prompt=-v means verbose"
                ])
            ))
        );
    }

    const ENGINES: [Engine; 3] = [Engine::Claude, Engine::Codex, Engine::OpenCode];

    /// The mode each engine accepts with no option of its own.
    fn quiet(engine: Engine) -> Launch<'static> {
        launch(engine, PermissionMode::Default)
    }

    #[test]
    fn ids_and_models_that_could_be_options_are_refused() {
        let flags = [
            "--dangerously-skip-permissions",
            "--dangerously-bypass-approvals-and-sandbox",
            "-p",
            "-",
            "",
            "a b",
            "a\n--x",
            "a=b",
        ];
        for engine in ENGINES {
            for bad in flags {
                let mut resumed = quiet(engine);
                resumed.resume = Some(bad);
                assert!(args(&resumed, None).is_err(), "{engine:?} resume {bad:?}");
                let mut model = quiet(engine);
                model.model = Some(bad);
                assert!(args(&model, None).is_err(), "{engine:?} model {bad:?}");
            }
            let mut long = quiet(engine);
            let id = "a".repeat(plain::MAX_LEN + 1);
            long.resume = Some(&id);
            assert!(args(&long, None).is_err(), "{engine:?} long id");
        }
        // Claude's new session id is checked too.
        assert!(args(&quiet(Engine::Claude), Some("--print")).is_err());
    }

    #[test]
    fn before_the_end_of_options_every_argument_is_an_option_with_its_value_attached() {
        let modes = [
            PermissionMode::Default,
            PermissionMode::AcceptEdits,
            PermissionMode::Plan,
            PermissionMode::BypassPermissions,
        ];
        let mut checked = 0;
        for engine in ENGINES {
            for mode in modes {
                for resume in [None, Some("2b6f1a8e-4c1d-4f5e-9a37-0c8d1e2f3a4b")] {
                    let mut l = launch(engine, mode);
                    l.resume = resume;
                    l.model = Some("provider/model-1");
                    let Ok((_, a)) = args(&l, Some("u-1")) else {
                        continue;
                    };
                    let end = a.iter().position(|x| x == "--").unwrap_or(a.len());
                    for x in &a[..end] {
                        // A subcommand, or `--name` / `--name=value` whose value is plain, or
                        // is the brief (OpenCode's `--prompt=`, free text in one argument).
                        let option = x.strip_prefix("--").is_some_and(|o| {
                            let (name, value) = o.split_once('=').unwrap_or((o, "x"));
                            !name.is_empty()
                                && !name.starts_with('-')
                                && (value.starts_with(|c: char| c.is_ascii_alphanumeric())
                                    || (name == "prompt" && Some(value) == l.brief))
                        });
                        assert!(option || x == "resume", "{engine:?} {mode:?}: {a:?}");
                    }
                    // After `--`, only the resumed id and the brief.
                    let rest: Vec<&str> = a.iter().skip(end + 1).map(String::as_str).collect();
                    let allowed: Vec<&str> = resume.into_iter().chain(l.brief).collect();
                    assert!(
                        rest.iter().all(|r| allowed.contains(r)),
                        "{engine:?} {mode:?}: {a:?}"
                    );
                    checked += 1;
                }
            }
        }
        assert!(checked >= 10, "{checked}");
    }

    #[test]
    fn uuids_are_version_4() {
        let u = new_uuid();
        assert_eq!(u.len(), 36);
        assert_eq!(u.as_bytes()[14], b'4');
        assert!(matches!(u.as_bytes()[19], b'8' | b'9' | b'a' | b'b'), "{u}");
        assert_ne!(u, new_uuid());
    }
}
