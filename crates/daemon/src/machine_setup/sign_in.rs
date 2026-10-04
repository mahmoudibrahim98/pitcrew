//! Sign-in terminals: an agent CLI's **own** login command, run in a terminal of this machine's
//! terminal runtime (tmux or pitcrew-ptyd, as the runner's sessions), which the person drives
//! through the terminals route (`GET /v1/sessions/{id}/terminal`).
//!
//! - **What runs** is fixed per CLI ([`login_args`]): `claude auth login`, `codex login` (or
//!   `codex login --device-auth`), `opencode auth login`, found on the daemon's `PATH`, in the
//!   person's home folder, with nothing added to its environment (no PitCrew variable, no token).
//!   Never `claude setup-token`, which prints a long-lived token to the screen.
//! - **PitCrew never reads the login.** The terminal relays the CLI's screen and the person's keys
//!   as any terminal does; nothing here parses, logs or stores them. What the login stores is the
//!   CLI's own business, in its own files, which PitCrew never opens.
//! - **Not a session.** Its id is a fresh session-shaped id that only the terminals route knows
//!   ([`SignInTerminals`]): no event is appended, no session listed, no transcript watched.
//! - **One per CLI at a time.** Asking again while one runs gives that one.
//! - **Short-lived.** Once the login has ended, its terminal stays [`LINGER`] for its last screen to
//!   be read, then it is removed (its output with it). One still running after [`MAX_AGE`] is
//!   stopped and removed. A daemon that starts removes any sign-in terminal an earlier one left in
//!   its runtime (by its window name).

use super::accounts::{label, program};
use super::tools::Tools;
use pitcrew_api::{Attachment, RuntimeTerminals, TerminalError, Terminals};
use pitcrew_interfaces::runtime::{Runtime, RuntimeError, StartSpec};
use pitcrew_protocol::ids::{SessionId, TerminalId};
use pitcrew_protocol::machine_setup::{SignIn, SignInMethod};
use pitcrew_protocol::model::{Engine, TimestampMs};
use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// How long an ended login's terminal stays readable.
pub const LINGER: Duration = Duration::from_secs(5 * 60);
/// How long a login may run before it is stopped.
pub const MAX_AGE: Duration = Duration::from_secs(30 * 60);
/// How often expired sign-ins are looked for.
const SWEEP_EVERY: Duration = Duration::from_secs(15);
/// The start of every sign-in terminal's name, by which a later daemon finds leftovers.
const NAME_PREFIX: &str = "pitcrew sign-in: ";
/// The terminal's size until the person's view resizes it.
const COLS: u16 = 100;
const ROWS: u16 = 30;

/// Why a sign-in could not start.
#[derive(Debug)]
pub enum StartError {
    /// The CLI is not on `PATH` (`409`).
    NotInstalled(String),
    /// The method does not apply to this CLI (`400`).
    Method(String),
    /// No terminal runtime, or it did not answer (`503`).
    Unavailable(String),
    /// Anything else (`500`).
    Failed(String),
}

/// The CLI's own login: its arguments, for `method`; `None` when the method does not apply.
pub fn login_args(engine: Engine, method: SignInMethod) -> Option<&'static [&'static str]> {
    match (engine, method) {
        (Engine::Claude, SignInMethod::Browser) => Some(&["auth", "login"]),
        (Engine::Codex, SignInMethod::Browser) => Some(&["login"]),
        (Engine::Codex, SignInMethod::DeviceCode) => Some(&["login", "--device-auth"]),
        (Engine::OpenCode, SignInMethod::Browser) => Some(&["auth", "login"]),
        _ => None,
    }
}

/// The sign-in terminals of this daemon.
pub struct SignIns {
    runtime: Arc<dyn Runtime>,
    terminals: RuntimeTerminals<dyn Runtime>,
    records: Mutex<HashMap<Engine, Record>>,
    tools: Tools,
    /// Where logins run: the person's home folder.
    cwd: PathBuf,
    linger: Duration,
    max_age: Duration,
}

impl fmt::Debug for SignIns {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SignIns")
            .field("running", &self.lock().len())
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug)]
struct Record {
    id: SessionId,
    terminal: TerminalId,
    command: Vec<String>,
    started: TimestampMs,
    since: Instant,
    /// When it was first seen ended.
    ended: Option<Instant>,
}

impl SignIns {
    /// Sign-ins in `runtime`'s terminals, running the CLIs `tools` finds, in `cwd`.
    pub fn new(runtime: Arc<dyn Runtime>, tools: Tools, cwd: PathBuf) -> Self {
        Self {
            terminals: RuntimeTerminals::new(Arc::clone(&runtime)),
            runtime,
            records: Mutex::new(HashMap::new()),
            tools,
            cwd,
            linger: LINGER,
            max_age: MAX_AGE,
        }
    }

    /// As [`SignIns::new`], with other lifetimes (tests).
    #[cfg(test)]
    #[cfg(unix)]
    fn with_times(mut self, linger: Duration, max_age: Duration) -> Self {
        self.linger = linger;
        self.max_age = max_age;
        self
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<Engine, Record>> {
        self.records.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Removes sign-in terminals an earlier daemon left in the runtime, then looks for expired
    /// ones every [`SWEEP_EVERY`] while `this` lives. Blocking work runs on the blocking pool.
    pub fn start_sweeping(this: &Arc<Self>) {
        let weak: Weak<Self> = Arc::downgrade(this);
        drop(tokio::spawn(async move {
            if let Some(this) = weak.upgrade() {
                let _ = tokio::task::spawn_blocking(move || this.remove_leftovers()).await;
            }
            let mut tick = tokio::time::interval(SWEEP_EVERY);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tick.tick().await;
                let Some(this) = weak.upgrade() else { return };
                let _ = tokio::task::spawn_blocking(move || this.sweep()).await;
            }
        }));
    }

    /// Stops and removes sign-in terminals that no record of this daemon's names.
    fn remove_leftovers(&self) {
        let Ok(listed) = self.runtime.list() else {
            return;
        };
        let ours: Vec<TerminalId> = self.lock().values().map(|r| r.terminal).collect();
        for terminal in listed {
            if terminal.name.starts_with(NAME_PREFIX) && !ours.contains(&terminal.id) {
                tracing::info!(terminal = %terminal.id, "removing a sign-in terminal an earlier pitcrewd left");
                let _ = self.runtime.kill(terminal.id);
            }
        }
    }

    /// Whether the login in `terminal` still runs, as far as the runtime can tell.
    fn running(&self, terminal: TerminalId) -> bool {
        match self.runtime.info(terminal) {
            Ok(info) => info.alive,
            Err(RuntimeError::NotFound(_)) => false,
            // It cannot be asked: count it as running, so it is not removed meanwhile.
            Err(_) => true,
        }
    }

    /// Notes which logins have ended, and removes those past their time. Blocking.
    pub fn sweep(&self) {
        let records: Vec<(Engine, Record)> = self
            .lock()
            .iter()
            .map(|(engine, record)| (*engine, record.clone()))
            .collect();
        for (engine, record) in records {
            let running = record.ended.is_none() && self.running(record.terminal);
            let expired = match record.ended {
                Some(ended) => ended.elapsed() >= self.linger,
                None if !running => false,
                None => record.since.elapsed() >= self.max_age,
            };
            if !running && record.ended.is_none() {
                if let Some(r) = self.lock().get_mut(&engine)
                    && r.id == record.id
                {
                    r.ended = Some(Instant::now());
                }
                continue;
            }
            if expired {
                self.forget(engine, &record);
            }
        }
    }

    /// Stops `record`'s terminal and forgets it.
    fn forget(&self, engine: Engine, record: &Record) {
        let mut records = self.lock();
        if records.get(&engine).is_some_and(|r| r.id == record.id) {
            records.remove(&engine);
        }
        drop(records);
        self.terminals.unlink(record.id);
        if let Err(e) = self.runtime.kill(record.terminal) {
            tracing::debug!(error = %e, "a sign-in terminal could not be removed");
        }
        tracing::info!(engine = ?engine, terminal = %record.terminal, "a sign-in terminal was removed");
    }

    /// The sign-in of `engine`, if there is one. Blocking (asks the runtime whether it runs).
    pub fn get(&self, engine: Engine) -> Option<SignIn> {
        let record = self.lock().get(&engine).cloned()?;
        let running = record.ended.is_none() && self.running(record.terminal);
        Some(view(engine, &record, running))
    }

    /// Starts `engine`'s login with `method`, or gives the one running (`false`: not new).
    /// Blocking: the runtime starts the terminal.
    ///
    /// # Errors
    /// See [`StartError`].
    pub fn start(
        &self,
        engine: Engine,
        method: SignInMethod,
    ) -> Result<(SignIn, bool), StartError> {
        let name = program(engine)
            .ok_or_else(|| StartError::Method("PitCrew does not run this CLI.".to_owned()))?;
        let args = login_args(engine, method).ok_or_else(|| {
            StartError::Method(format!(
                "{} has no {} sign-in; its login shows what to do.",
                label(engine),
                match method {
                    SignInMethod::Browser => "browser",
                    SignInMethod::DeviceCode => "device-code",
                }
            ))
        })?;
        if let Some(running) = self.get(engine).filter(|s| s.running) {
            return Ok((running, false));
        }
        if self.tools.find(name).is_none() {
            return Err(StartError::NotInstalled(format!(
                "{} ({name}) is not on this machine's PATH: install it first.",
                label(engine)
            )));
        }
        let cwd = self
            .cwd
            .to_str()
            .ok_or_else(|| StartError::Failed("The home folder's path is not UTF-8.".to_owned()))?
            .to_owned();
        // By name, as the runner starts the CLIs for sessions: the runtime finds it on the same
        // `PATH` (and on Windows, pitcrew-ptyd its `.exe` or `.cmd`).
        let spec = StartSpec {
            program: name.to_owned(),
            args: args.iter().map(|a| (*a).to_owned()).collect(),
            cwd,
            env: Vec::new(),
            name: format!("{NAME_PREFIX}{name}"),
            cols: COLS,
            rows: ROWS,
        };
        let info = self.runtime.start(&spec).map_err(|e| match e {
            RuntimeError::Unavailable(why) => StartError::Unavailable(format!(
                "This machine has no terminal for the login: {why}"
            )),
            other => StartError::Failed(format!("The login could not be started: {other}")),
        })?;
        let record = Record {
            id: SessionId::new(),
            terminal: info.id,
            command: std::iter::once(name.to_owned())
                .chain(args.iter().map(|a| (*a).to_owned()))
                .collect(),
            started: now_ms(),
            since: Instant::now(),
            ended: None,
        };
        self.terminals.link(record.id, record.terminal);
        let previous = self.lock().insert(engine, record.clone());
        if let Some(previous) = previous {
            // An ended one, replaced: its terminal goes now.
            self.terminals.unlink(previous.id);
            let _ = self.runtime.kill(previous.terminal);
        }
        tracing::info!(engine = name, terminal = %record.terminal, "a sign-in terminal started");
        Ok((view(engine, &record, true), true))
    }

    /// The terminal of sign-in `id`, if it is one of these.
    fn attach(&self, id: SessionId) -> Option<Result<Arc<dyn Attachment>, TerminalError>> {
        let known = self.lock().values().any(|r| r.id == id);
        known.then(|| self.terminals.attach(id))
    }
}

fn view(engine: Engine, record: &Record, running: bool) -> SignIn {
    SignIn {
        engine,
        terminal: record.id,
        command: record.command.clone(),
        running,
        started: record.started,
    }
}

fn now_ms() -> TimestampMs {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or_default()
}

/// The terminals route's [`Terminals`]: sign-in terminals by their id, every other id the
/// sessions' (`inner`).
#[derive(Debug)]
pub struct SignInTerminals<T> {
    sign_ins: Arc<SignIns>,
    inner: T,
}

impl<T: Terminals> SignInTerminals<T> {
    /// Sign-ins first, then `inner`.
    pub fn new(sign_ins: Arc<SignIns>, inner: T) -> Self {
        Self { sign_ins, inner }
    }
}

impl<T: Terminals> Terminals for SignInTerminals<T> {
    fn attach(&self, session: SessionId) -> Result<Arc<dyn Attachment>, TerminalError> {
        match self.sign_ins.attach(session) {
            Some(found) => found,
            None => self.inner.attach(session),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pitcrew_interfaces::runtime::{OutputChunk, RuntimeKind, Screen, TerminalInfo};
    use pitcrew_protocol::runner::Key;

    /// A runtime that records what it was asked to start, and whose terminals end when told.
    #[derive(Debug, Default)]
    struct Fake {
        started: Mutex<Vec<(TerminalId, StartSpec)>>,
        ended: Mutex<Vec<TerminalId>>,
        killed: Mutex<Vec<TerminalId>>,
        leftovers: Vec<TerminalInfo>,
    }

    impl Fake {
        fn info_of(&self, id: TerminalId) -> Result<TerminalInfo, RuntimeError> {
            let started = self.started.lock().unwrap();
            let (_, spec) = started
                .iter()
                .find(|(t, _)| *t == id)
                .ok_or(RuntimeError::NotFound(id))?;
            if self.killed.lock().unwrap().contains(&id) {
                return Err(RuntimeError::NotFound(id));
            }
            Ok(TerminalInfo {
                id,
                name: spec.name.clone(),
                pid: None,
                alive: !self.ended.lock().unwrap().contains(&id),
                native_target: None,
            })
        }
    }

    impl Runtime for Fake {
        fn kind(&self) -> RuntimeKind {
            RuntimeKind::Pty
        }
        fn start(&self, spec: &StartSpec) -> Result<TerminalInfo, RuntimeError> {
            let id = TerminalId::new();
            self.started.lock().unwrap().push((id, spec.clone()));
            self.info_of(id)
        }
        fn write(&self, _: TerminalId, _: &[u8]) -> Result<(), RuntimeError> {
            Ok(())
        }
        fn send_keys(&self, _: TerminalId, _: &[Key]) -> Result<(), RuntimeError> {
            Ok(())
        }
        fn resize(&self, _: TerminalId, _: u16, _: u16) -> Result<(), RuntimeError> {
            Ok(())
        }
        fn screen(&self, _: TerminalId) -> Result<Screen, RuntimeError> {
            Ok(Screen::default())
        }
        fn read_output(
            &self,
            id: TerminalId,
            from: u64,
            _: usize,
        ) -> Result<OutputChunk, RuntimeError> {
            self.info_of(id)?;
            Ok(OutputChunk {
                offset: from,
                data: Vec::new(),
                end: from,
                truncated: false,
            })
        }
        fn info(&self, id: TerminalId) -> Result<TerminalInfo, RuntimeError> {
            self.info_of(id)
        }
        fn list(&self) -> Result<Vec<TerminalInfo>, RuntimeError> {
            Ok(self.leftovers.clone())
        }
        fn kill(&self, id: TerminalId) -> Result<(), RuntimeError> {
            self.killed.lock().unwrap().push(id);
            Ok(())
        }
    }

    #[cfg(unix)]
    #[derive(Debug)]
    struct NoSessions;
    #[cfg(unix)]
    impl Terminals for NoSessions {
        fn attach(&self, session: SessionId) -> Result<Arc<dyn Attachment>, TerminalError> {
            Err(TerminalError::NotFound(format!("No session {session}.")))
        }
    }

    /// `bin` with a stand-in for each CLI.
    #[cfg(unix)]
    fn tools(bin: &std::path::Path) -> Tools {
        for name in ["claude", "codex", "opencode"] {
            let path = bin.join(name);
            std::fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
        Tools::with_path(std::ffi::OsString::from(bin))
    }

    fn rig(fake: Arc<Fake>, tools: Tools) -> SignIns {
        let runtime: Arc<dyn Runtime> = fake;
        SignIns::new(runtime, tools, std::env::temp_dir())
    }

    #[test]
    fn each_cli_runs_its_own_login_and_nothing_else() {
        assert_eq!(
            login_args(Engine::Claude, SignInMethod::Browser),
            Some(&["auth", "login"][..])
        );
        assert_eq!(
            login_args(Engine::Codex, SignInMethod::Browser),
            Some(&["login"][..])
        );
        assert_eq!(
            login_args(Engine::Codex, SignInMethod::DeviceCode),
            Some(&["login", "--device-auth"][..])
        );
        assert_eq!(
            login_args(Engine::OpenCode, SignInMethod::Browser),
            Some(&["auth", "login"][..])
        );
        assert_eq!(login_args(Engine::Claude, SignInMethod::DeviceCode), None);
        assert_eq!(login_args(Engine::OpenCode, SignInMethod::DeviceCode), None);
    }

    #[cfg(unix)]
    #[test]
    fn a_sign_in_is_a_terminal_only_the_terminals_route_knows() {
        let tmp = tempfile::tempdir().unwrap();
        let fake = Arc::new(Fake::default());
        let sign_ins = Arc::new(rig(Arc::clone(&fake), tools(tmp.path())));
        let routes = SignInTerminals::new(Arc::clone(&sign_ins), NoSessions);

        let (first, new) = sign_ins
            .start(Engine::Claude, SignInMethod::Browser)
            .unwrap();
        assert!(new);
        assert!(first.running);
        assert_eq!(first.command, ["claude", "auth", "login"]);
        let started = fake.started.lock().unwrap().clone();
        assert_eq!(started.len(), 1);
        let (terminal, spec) = &started[0];
        assert_eq!(spec.program, "claude");
        assert_eq!(spec.args, ["auth", "login"]);
        assert!(
            spec.env.is_empty(),
            "nothing is added to a login's environment"
        );
        assert_eq!(spec.name, "pitcrew sign-in: claude");

        // Asking again while it runs gives the same one.
        let (again, new) = sign_ins
            .start(Engine::Claude, SignInMethod::Browser)
            .unwrap();
        assert!(!new);
        assert_eq!(again.terminal, first.terminal);
        assert_eq!(fake.started.lock().unwrap().len(), 1);

        // The terminals route finds it by its id; any other id is the sessions'.
        assert!(routes.attach(first.terminal).is_ok());
        assert!(matches!(
            routes.attach(SessionId::new()),
            Err(TerminalError::NotFound(m)) if m.starts_with("No session")
        ));

        // Once the login ends it stays readable, then goes.
        fake.ended.lock().unwrap().push(*terminal);
        assert!(!sign_ins.get(Engine::Claude).unwrap().running);
        sign_ins.sweep();
        assert!(sign_ins.get(Engine::Claude).is_some(), "it lingers");
        assert!(routes.attach(first.terminal).is_ok());

        let short =
            Arc::new(rig(Arc::clone(&fake), tools(tmp.path())).with_times(Duration::ZERO, MAX_AGE));
        let (ended, _) = short
            .start(Engine::Codex, SignInMethod::DeviceCode)
            .unwrap();
        assert_eq!(ended.command, ["codex", "login", "--device-auth"]);
        let codex = fake.started.lock().unwrap().last().unwrap().0;
        fake.ended.lock().unwrap().push(codex);
        short.sweep();
        short.sweep();
        assert!(
            short.get(Engine::Codex).is_none(),
            "removed after its linger"
        );
        assert!(fake.killed.lock().unwrap().contains(&codex));
        let short_routes = SignInTerminals::new(Arc::clone(&short), NoSessions);
        assert!(short_routes.attach(ended.terminal).is_err());

        // A login that runs too long is stopped.
        let old = rig(Arc::clone(&fake), tools(tmp.path())).with_times(LINGER, Duration::ZERO);
        let (open, _) = old.start(Engine::OpenCode, SignInMethod::Browser).unwrap();
        old.sweep();
        assert!(old.get(Engine::OpenCode).is_none());
        let opencode = fake.started.lock().unwrap().last().unwrap().0;
        assert!(fake.killed.lock().unwrap().contains(&opencode));
        assert!(!open.terminal.to_string().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn what_cannot_start_says_why() {
        let tmp = tempfile::tempdir().unwrap();
        let empty = Tools::with_path(std::ffi::OsString::from(tmp.path()));
        let fake = Arc::new(Fake::default());
        let sign_ins = rig(Arc::clone(&fake), empty);
        assert!(matches!(
            sign_ins.start(Engine::Claude, SignInMethod::Browser),
            Err(StartError::NotInstalled(m)) if m.contains("Claude Code (claude) is not on this machine's PATH")
        ));
        assert!(matches!(
            sign_ins.start(Engine::Claude, SignInMethod::DeviceCode),
            Err(StartError::Method(_))
        ));
        let none: Arc<dyn Runtime> = Arc::new(crate::terminals::NoRuntime);
        let without = SignIns::new(none, tools(tmp.path()), std::env::temp_dir());
        assert!(matches!(
            without.start(Engine::Codex, SignInMethod::Browser),
            Err(StartError::Unavailable(_))
        ));
        assert!(fake.started.lock().unwrap().is_empty());
    }

    #[test]
    fn leftovers_of_an_earlier_daemon_are_removed() {
        let mine = TerminalInfo {
            id: TerminalId::new(),
            name: "pitcrew sign-in: claude".into(),
            pid: None,
            alive: true,
            native_target: None,
        };
        let session = TerminalInfo {
            id: TerminalId::new(),
            name: "work".into(),
            pid: None,
            alive: true,
            native_target: None,
        };
        let fake = Arc::new(Fake {
            leftovers: vec![mine.clone(), session.clone()],
            ..Fake::default()
        });
        let sign_ins = rig(Arc::clone(&fake), Tools::default());
        sign_ins.remove_leftovers();
        assert_eq!(*fake.killed.lock().unwrap(), vec![mine.id]);
    }
}
