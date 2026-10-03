//! Hub commands, in-process form (work package 5): [`RunnerCommands::run`].
//!
//! - **Idempotent by `CommandId`:** an outcome is saved in the runner's index before `run`
//!   returns, and a command id seen again (also after a restart, for a week) returns that outcome
//!   without running anything. The same id sent twice at once runs once; the second call waits.
//! - **Starting a session** starts its CLI in a new terminal. The runner learns the CLI's own id
//!   only when its transcript appears; the terminal is then linked to it, and its
//!   `session_discovered` names the terminal. Claude is started with a session id chosen here, so
//!   the match is exact; other CLIs are matched by folder and start time.
//! - **A session the hub named** (`StartSession`'s `session`, a dispatch's): the terminal is that
//!   session's from the start (text, keys and ends reach it at once), and the CLI's transcript
//!   **adopts** the id instead of getting one of its own (see `watch`): Claude's by the id it was
//!   started with, the others' by folder and start time. The CLI gets the environment the
//!   [`SessionEnv`] gives it (an agent token's file). A session this runner already knows is not
//!   started again. Two CLIs matched by folder that wait for their transcripts in one folder
//!   cannot be told apart, so a start there is refused while one waits (inside the claim window,
//!   its program still running) when either of them is for a named session. A terminal there
//!   whose program ended before its transcript appeared is retired at such a start (forgotten:
//!   it will write no transcript, and none is taken for it), and the watcher's folder match skips
//!   it meanwhile.
//! - **Where a named session stands** ([`RunnerCommands::started`]), for the hub's
//!   reconciliation: a start still under way (from the moment its command is run until it
//!   returns) is running, never gone; and [`RunnerCommands::retire`] forgets the terminal of one
//!   the hub gave up on, once its program has ended.
//! - Text, keys, interrupts and ends go to the session's terminal. Ending a session reports it
//!   ended (`session_state_changed` and `session_ended`) at once.

use crate::config::EngineHome;
use crate::derive::Reported;
use crate::plain;
use crate::session_env::SessionEnv;
use crate::store::{CLAIM_WINDOW_MS, StoreError, TerminalRow};
use crate::terminals::RunnerTerminals;
use crate::watch::{Origin, Pending, Shared, Signal, Target};
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
    session_env: Option<Arc<dyn SessionEnv>>,
    options: CommandOptions,
    running: Mutex<HashSet<CommandId>>,
    done: Condvar,
    /// Held by a start of a CLI matched by folder, from its check of the folder until its
    /// terminal is recorded, so two such starts cannot both find the folder free.
    by_folder: Mutex<()>,
}

/// Where a session the hub named for a start stands on this runner ([`RunnerCommands::started`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Started {
    /// Its transcript is indexed under it: the runner reports it.
    Reported,
    /// Its start is under way (its command has not returned), or its terminal's program runs and
    /// its transcript has not been found yet.
    Running,
    /// Its terminal's program runs, but it is matched by folder (Codex, OpenCode) and its
    /// transcript did not appear within the claim window (15 minutes) of its start: no transcript
    /// can be taken for it any more.
    TooLate,
    /// Neither: this runner never started it (or forgot it), or its program ended before its
    /// transcript was found.
    Gone,
    /// Cannot tell now: the index or the runtime did not answer.
    Unknown(String),
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
        session_env: Option<Arc<dyn SessionEnv>>,
        options: CommandOptions,
    ) -> Self {
        shared.set_terminals(&terminals);
        Self {
            inner: Arc::new(Inner {
                terminals,
                shared,
                homes,
                session_env,
                options,
                running: Mutex::new(HashSet::new()),
                done: Condvar::new(),
                by_folder: Mutex::new(()),
            }),
        }
    }

    /// Where `session`, one the hub named for a start, stands here: reported (its transcript is
    /// indexed under it), running (its start is under way, or its terminal's program runs and the
    /// transcript is not found yet), too late (matched by folder, and past the claim window),
    /// gone, or unknown. For the hub's reconciliation of the sessions it stored before their CLI
    /// started. Blocking: it may ask the runtime about the terminal.
    pub fn started(&self, session: SessionId) -> Started {
        // First: a start that returns meanwhile has recorded its terminal by then.
        if self.inner.shared.is_under_way(session) {
            return Started::Running;
        }
        let terminals = &self.inner.terminals;
        let known = {
            let store = terminals.store();
            store
                .has_session(session)
                .and_then(|reported| Ok((reported, store.terminal_of(session)?)))
        };
        let row = match known {
            Ok((true, _)) => return Started::Reported,
            Ok((false, None)) => return Started::Gone,
            Ok((false, Some(t))) => t,
            Err(e) => return Started::Unknown(e.to_string()),
        };
        match terminals.info(row.terminal) {
            Ok(info) if info.alive => {
                let window_over = crate::now_ms().saturating_sub(row.started_at) > CLAIM_WINDOW_MS;
                if row.native_id.is_none() && window_over {
                    Started::TooLate
                } else {
                    Started::Running
                }
            }
            Ok(_) | Err(TerminalError::NotFound(_)) => Started::Gone,
            Err(e) => Started::Unknown(e.to_string()),
        }
    }

    /// Forgets the terminal of `session`, one the hub named and has given up on, if its
    /// transcript was never found and its program has ended: no transcript is taken for it
    /// afterwards, by its CLI's id or by folder. A terminal whose program still runs is kept (it
    /// is the session's still, for a person to end). True if one was forgotten.
    ///
    /// # Errors
    ///
    /// The runner's index cannot be read or written.
    pub fn retire(&self, session: SessionId) -> Result<bool, StoreError> {
        let terminals = &self.inner.terminals;
        let row = {
            let store = terminals.store();
            if store.has_session(session)? {
                return Ok(false);
            }
            store.terminal_of(session)?
        };
        let Some(row) = row else {
            return Ok(false);
        };
        if !terminals.has_ended(row.terminal) {
            return Ok(false);
        }
        terminals.store().forget_terminal(row.terminal)?;
        tracing::debug!(%session, terminal = %row.terminal, "retired the terminal of a session whose CLI ended before its transcript appeared");
        Ok(true)
    }

    /// Runs a command once per id, and answers its outcome. Blocking: starting a program or
    /// ending one gracefully can take seconds.
    pub fn run(&self, id: CommandId, command: &RunnerCommand) -> CommandOutcome {
        let inner = &*self.inner;
        // A start for a session the hub named is under way from now until this returns.
        let _under_way = match command {
            RunnerCommand::StartSession {
                session: Some(session),
                ..
            } => Some(inner.shared.under_way(*session)),
            _ => None,
        };
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
                session,
            } => self.start(&Launch {
                engine: *engine,
                cwd,
                name,
                brief: brief.as_deref(),
                model: model.as_deref(),
                account: account.as_deref(),
                mode: *permission_mode,
                resume: None,
                session: *session,
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
                session: None,
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
        if let Some(session) = launch.session {
            match self.knows(session) {
                Ok(false) => {}
                Ok(true) => {
                    return rejected(format!(
                        "Session {session} is already known to this runner; it is not started \
                         again."
                    ));
                }
                Err(e) => return failed(format!("cannot look up session {session}: {e}")),
            }
        }
        let mut env = match self.account_env(launch.engine, launch.account) {
            Ok(env) => env,
            Err(reason) => return rejected(reason),
        };
        if let (Some(session), Some(provider)) = (launch.session, &inner.session_env) {
            match provider.env_for(session) {
                Ok(more) => env.extend(more),
                Err(reason) => return rejected(reason),
            }
        }
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
        // A CLI matched by folder: no other start may take the folder until this one's terminal
        // is recorded.
        let _by_folder = native_id.is_none().then(|| {
            inner
                .by_folder
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
        });
        if native_id.is_none()
            && let Some(reason) = self.ambiguous(launch)
        {
            return rejected(reason);
        }
        let started_at = crate::now_ms();
        // Its CLI may write its transcript before the terminal is recorded below: the watcher
        // gives it the session the hub named meanwhile too.
        let _starting = launch.session.map(|session| {
            inner.shared.starting(Pending {
                session,
                engine: launch.engine,
                native_id: native_id.clone(),
                cwd: launch.cwd.to_owned(),
                started_at,
            })
        });
        let info = match inner.terminals.start(spec) {
            Ok(info) => info,
            Err(e) => return failed(e.to_string()),
        };
        if launch.mode == PermissionMode::BypassPermissions {
            tracing::warn!(terminal = %info.id, cwd = launch.cwd, engine = ?launch.engine, "started a session with permission prompts bypassed");
        }
        // A resumed session may be indexed already: link it now. A named one is the terminal's
        // from the start.
        let session = match (launch.session, launch.resume) {
            (Some(named), _) => Some(named),
            (None, Some(id)) => inner
                .terminals
                .store()
                .session_by_native(launch.engine, id)
                .unwrap_or_else(|e| {
                    tracing::warn!(error = %e, "cannot look up the resumed session");
                    None
                }),
            (None, None) => None,
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

    /// Whether this runner knows `session` already: a transcript indexed under it, or a terminal.
    fn knows(&self, session: SessionId) -> Result<bool, StoreError> {
        let store = self.inner.terminals.store();
        Ok(store.has_session(session)? || store.terminal_of(session)?.is_some())
    }

    /// Why a start of a CLI matched by folder (not Claude) could be taken for another, or the
    /// other for it: a start in the same folder still waits for its transcript (inside the claim
    /// window, its program running), and one of the two is for a session the hub named. Two
    /// starts for no named session are left to the folder match, as before.
    ///
    /// A start there whose program has ended is retired on the way: it will write no transcript,
    /// so none may be taken for it (the one this start's CLI writes least of all).
    fn ambiguous(&self, launch: &Launch<'_>) -> Option<String> {
        let terminals = &self.inner.terminals;
        let now = crate::now_ms();
        let waiting = match terminals.store().waiting_in(launch.engine, launch.cwd, now) {
            Ok(waiting) => waiting,
            Err(e) => {
                return Some(format!(
                    "cannot look at the starts waiting in {}: {e}",
                    launch.cwd
                ));
            }
        };
        let mut blocking = None;
        for t in waiting {
            match terminals.info(t.terminal) {
                Ok(info) if info.alive => {
                    if blocking.is_none() && (launch.session.is_some() || t.session.is_some()) {
                        blocking = Some(t);
                    }
                }
                Ok(_) | Err(TerminalError::NotFound(_)) => {
                    if let Err(e) = terminals.store().forget_terminal(t.terminal) {
                        tracing::warn!(terminal = %t.terminal, error = %e, "cannot retire a terminal whose program ended");
                    } else {
                        tracing::debug!(terminal = %t.terminal, session = ?t.session, "retired a terminal whose program ended before its transcript appeared");
                    }
                }
                // Cannot tell: neither blocks nor is retired.
                Err(_) => {}
            }
        }
        let blocking = blocking?;
        let secs = (now.saturating_sub(blocking.started_at) / 1000).max(0);
        Some(format!(
            "Another {:?} session started in {} {secs} s ago has not written its transcript yet, \
             and {:?} sessions are told apart only by folder and start time, so a second one \
             started there now could be taken for it. Start this one once that one appears, or \
             in another folder.",
            launch.engine, launch.cwd, launch.engine
        ))
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
    /// The session the hub named for it.
    pub session: Option<SessionId>,
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

    /// A start matched by folder (Codex, OpenCode) whose transcript has not appeared within the
    /// claim window is too late: no transcript can be taken for it any more. One matched by its
    /// id (Claude), or still inside the window, runs on.
    #[test]
    fn a_start_matched_by_folder_is_too_late_past_the_claim_window() {
        use crate::store::Store;
        use crate::terminals::TerminalOptions;
        use pitcrew_interfaces::fake::FakeRuntime;
        use pitcrew_interfaces::runtime::Runtime as _;

        let dir = tempfile::tempdir().expect("tempdir");
        let store = Arc::new(Mutex::new(Store::open(dir.path()).expect("open")));
        let runtime = Arc::new(FakeRuntime::default());
        let terminals = RunnerTerminals::new(
            Arc::clone(&runtime) as Arc<dyn pitcrew_interfaces::runtime::Runtime>,
            Arc::clone(&store),
            TerminalOptions::default(),
        )
        .expect("terminals");
        let commands = RunnerCommands::new(
            terminals,
            Arc::new(Shared::default()),
            Vec::new(),
            None,
            CommandOptions::default(),
        );
        let now = crate::now_ms();
        let named = |engine: Engine, native_id: Option<&str>, started_at| {
            let info = runtime
                .start(&StartSpec {
                    program: "codex".into(),
                    args: Vec::new(),
                    cwd: "/w".into(),
                    env: Vec::new(),
                    name: "w".into(),
                    cols: 80,
                    rows: 24,
                })
                .expect("start");
            let session = SessionId::new();
            store
                .lock()
                .expect("store")
                .put_terminal(&TerminalRow {
                    terminal: info.id,
                    native_target: None,
                    session: Some(session),
                    engine: Some(engine),
                    native_id: native_id.map(Into::into),
                    cwd: "/w".into(),
                    started_at,
                })
                .expect("put");
            session
        };
        let past = now - CLAIM_WINDOW_MS - 60_000;
        let late = named(Engine::Codex, None, past);
        let fresh = named(Engine::OpenCode, None, now);
        let by_id = named(Engine::Claude, Some("abc"), past);
        assert_eq!(commands.started(late), Started::TooLate);
        assert_eq!(commands.started(fresh), Started::Running);
        assert_eq!(commands.started(by_id), Started::Running);
        // Still running: not retired.
        assert!(!commands.retire(late).expect("retire"));
    }

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
            session: None,
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
