//! Sign-in terminals: an agent CLI's **own** login command, run in a terminal of this machine's
//! terminal runtime (tmux or pitcrew-ptyd, as the runner's sessions), which the person drives
//! through the terminals route (`GET /v1/sessions/{id}/terminal`).
//!
//! - **What runs** is fixed per CLI ([`login_args`]): `claude auth login`, `codex login` (or
//!   `codex login --device-auth`), `opencode auth login`, found on the daemon's `PATH`, in the
//!   person's home folder, with nothing added to its environment (no PitCrew variable, no token).
//!   Never `claude setup-token`, which prints a long-lived token to the screen.
//! - **Only a CLI that answers its status command.** A login starts only once the CLI's own status
//!   command (`claude auth status`, …; [`accounts`](super::accounts)) has said, in a way PitCrew
//!   understands, whether it is signed in. An older Claude Code without `auth` reads
//!   `auth login` as a prompt and would start an agent in the home folder instead: that is `409`,
//!   "update it first".
//! - **Who.** Only the member who set the hub up uses these routes (`super::routes`), and a
//!   sign-in's terminal opens only for the member who started it ([`SignInTerminals::routes`]).
//! - **PitCrew never reads the login.** The terminal relays the CLI's screen and the person's keys
//!   as any terminal does; nothing here parses, logs or stores them. What the login stores is the
//!   CLI's own business, in its own files, which PitCrew never opens.
//! - **Not a session.** Its id is a fresh session-shaped id that only the terminals route knows
//!   ([`SignInTerminals`]): no event is appended, no session listed, no transcript watched.
//! - **One per CLI at a time.** Asking again while one runs gives that one; two asks at once wait
//!   for each other (one start per CLI at a time), so both get the same terminal.
//! - **Short-lived.** Once the login has ended, its terminal stays [`LINGER`] for its last screen to
//!   be read, then it is removed (its output with it). One still running after [`MAX_AGE`] is
//!   stopped and removed, and so is one the person leaves (`DELETE …/sign-in`). The daemon stops
//!   every sign-in terminal when it stops ([`SignIns::stop_all`]), and keeps their ids in its
//!   state directory ([`LEDGER`]), so one that a crash left is removed when it next starts. Only
//!   ids listed there are removed: a session's terminal is never taken for a sign-in's by its name.

use super::accounts::{self, label, program};
use super::tools::Tools;
use axum::Router;
use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use pitcrew_api::terminal::TerminalConfig;
use pitcrew_api::{Attachment, RuntimeTerminals, TerminalError, Terminals};
use pitcrew_auth::ErrorResponse;
use pitcrew_interfaces::runtime::{Runtime, RuntimeError, StartSpec};
use pitcrew_protocol::api::Caller;
use pitcrew_protocol::ids::{MemberId, SessionId, TerminalId};
use pitcrew_protocol::machine_setup::{SignIn, SignInMethod};
use pitcrew_protocol::model::{Engine, TimestampMs};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// How long an ended login's terminal stays readable.
pub const LINGER: Duration = Duration::from_secs(5 * 60);
/// How long a login may run before it is stopped.
pub const MAX_AGE: Duration = Duration::from_secs(30 * 60);
/// How often expired sign-ins are looked for.
const SWEEP_EVERY: Duration = Duration::from_secs(15);
/// The start of every sign-in terminal's name (for people: a tmux window's name).
const NAME_PREFIX: &str = "pitcrew sign-in: ";
/// The file in the state directory that lists the sign-in terminals this daemon has open, for the
/// next daemon to remove any that a crash left.
pub const LEDGER: &str = "sign-in-terminals.json";
/// The terminal's size until the person's view resizes it.
const COLS: u16 = 100;
const ROWS: u16 = 30;

/// Why a sign-in could not start.
#[derive(Debug)]
pub enum StartError {
    /// The CLI is not on `PATH` (`409`).
    NotInstalled(String),
    /// The CLI did not answer its status command in a way PitCrew understands, so it may be too
    /// old to have the login command (`409`).
    Outdated(String),
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
    /// One start per CLI at a time: whether one runs, its status command, and starting it.
    starting: Mutex<HashMap<Engine, Arc<tokio::sync::Mutex<()>>>>,
    /// An earlier daemon's sign-in terminals (or this one's) that could not be removed yet.
    leftovers: Mutex<Vec<TerminalId>>,
    /// Where the open sign-in terminals' ids are kept ([`LEDGER`]); `None` keeps them nowhere.
    ledger: Option<PathBuf>,
    /// One write of the ledger at a time.
    saving: Mutex<()>,
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
    /// Who started it: the only member whose terminal view it opens in.
    member: MemberId,
}

/// The ledger's shape on disk.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Ledger {
    terminals: Vec<TerminalId>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl SignIns {
    /// Sign-ins in `runtime`'s terminals, running the CLIs `tools` finds, in `cwd`, their ids
    /// kept in `ledger` (`<state>/`[`LEDGER`]).
    pub fn new(
        runtime: Arc<dyn Runtime>,
        tools: Tools,
        cwd: PathBuf,
        ledger: Option<PathBuf>,
    ) -> Self {
        Self {
            terminals: RuntimeTerminals::new(Arc::clone(&runtime)),
            runtime,
            records: Mutex::new(HashMap::new()),
            starting: Mutex::new(HashMap::new()),
            leftovers: Mutex::new(Vec::new()),
            ledger,
            saving: Mutex::new(()),
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
        lock(&self.records)
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

    /// Stops and removes the sign-in terminals the ledger lists that no record of this daemon's
    /// names: an earlier daemon's. A terminal the ledger does not list is never touched, whatever
    /// its name. Those that cannot be removed now stay listed for the next time.
    fn remove_leftovers(&self) {
        let Some(path) = &self.ledger else {
            return;
        };
        let earlier = read_ledger(path);
        if earlier.is_empty() {
            return;
        }
        let Ok(listed) = self.runtime.list() else {
            // The runtime did not answer: try again when the daemon next starts.
            lock(&self.leftovers).extend(earlier);
            return;
        };
        let ours: Vec<TerminalId> = self.lock().values().map(|r| r.terminal).collect();
        let mut kept = Vec::new();
        for terminal in listed {
            if !earlier.contains(&terminal.id) || ours.contains(&terminal.id) {
                continue;
            }
            tracing::info!(terminal = %terminal.id, "removing a sign-in terminal an earlier pitcrewd left");
            if let Err(e) = self.runtime.kill(terminal.id) {
                tracing::debug!(error = %e, "a leftover sign-in terminal could not be removed");
                kept.push(terminal.id);
            }
        }
        *lock(&self.leftovers) = kept;
        self.save_or_warn();
    }

    /// Writes the ledger: this daemon's sign-in terminals, and the leftovers not removed yet.
    fn save(&self) -> std::io::Result<()> {
        let Some(path) = &self.ledger else {
            return Ok(());
        };
        let _saving = lock(&self.saving);
        let mut terminals: Vec<TerminalId> = self.lock().values().map(|r| r.terminal).collect();
        terminals.extend(lock(&self.leftovers).iter().copied());
        terminals.sort_unstable();
        terminals.dedup();
        let body = serde_json::to_vec(&Ledger { terminals }).map_err(std::io::Error::other)?;
        let mut temporary = path.as_os_str().to_owned();
        temporary.push(".tmp");
        let temporary = PathBuf::from(temporary);
        std::fs::write(&temporary, body)?;
        std::fs::rename(&temporary, path)
    }

    fn save_or_warn(&self) {
        if let Err(e) = self.save() {
            tracing::warn!(error = %e, "the list of sign-in terminals could not be written");
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
        self.kill(record.terminal);
        self.save_or_warn();
        tracing::info!(engine = ?engine, terminal = %record.terminal, "a sign-in terminal was removed");
    }

    /// Stops `terminal`; one that cannot be stopped now is kept for the next daemon.
    fn kill(&self, terminal: TerminalId) {
        match self.runtime.kill(terminal) {
            Ok(()) | Err(RuntimeError::NotFound(_)) => {}
            Err(e) => {
                tracing::debug!(error = %e, "a sign-in terminal could not be removed");
                lock(&self.leftovers).push(terminal);
            }
        }
    }

    /// The sign-in of `engine`, if there is one. Blocking (asks the runtime whether it runs).
    pub fn get(&self, engine: Engine) -> Option<SignIn> {
        let record = self.lock().get(&engine).cloned()?;
        let running = record.ended.is_none() && self.running(record.terminal);
        Some(view(engine, &record, running))
    }

    /// Stops and removes the sign-in of `engine`, running or ended: whether there was one.
    /// Blocking.
    pub fn stop(&self, engine: Engine) -> bool {
        let Some(record) = self.lock().get(&engine).cloned() else {
            return false;
        };
        self.forget(engine, &record);
        true
    }

    /// Stops and removes every sign-in terminal: when the daemon stops, before its runtime is let
    /// go of, so no login (and no Codex callback listener) outlives it. Blocking.
    pub fn stop_all(&self) {
        let records: Vec<(Engine, Record)> = self.lock().drain().collect();
        for (engine, record) in &records {
            self.terminals.unlink(record.id);
            self.kill(record.terminal);
            tracing::info!(engine = ?engine, terminal = %record.terminal, "a sign-in terminal was stopped with the daemon");
        }
        self.save_or_warn();
    }

    /// Who started sign-in `id`, if it is one of these.
    fn starter(&self, id: SessionId) -> Option<MemberId> {
        self.lock().values().find(|r| r.id == id).map(|r| r.member)
    }

    fn gate(&self, engine: Engine) -> Arc<tokio::sync::Mutex<()>> {
        Arc::clone(lock(&self.starting).entry(engine).or_default())
    }

    /// Starts `engine`'s login with `method` for `member`, or gives the one running (`false`: not
    /// new). One start per CLI at a time: a second ask meanwhile waits, then gets the same one.
    ///
    /// # Errors
    /// See [`StartError`].
    pub async fn start(
        this: &Arc<Self>,
        engine: Engine,
        method: SignInMethod,
        member: MemberId,
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
        let gate = this.gate(engine);
        let _starting = gate.lock().await;
        let running = blocking(this, move |s| s.get(engine))
            .await?
            .filter(|s| s.running);
        if let Some(running) = running {
            return Ok((running, false));
        }
        // The CLI's own status first: only one that answers it is asked to log in.
        let account = accounts::account(&this.tools, engine).await;
        if !account.installed {
            return Err(StartError::NotInstalled(format!(
                "{} ({name}) is not on this machine's PATH: install it first.",
                label(engine)
            )));
        }
        if account.signed_in.is_none() {
            return Err(StartError::Outdated(format!(
                "Update {label} first: `{status}` gave no answer PitCrew understands, so this \
                 {label} may not have `{name} {login}`.",
                label = label(engine),
                status = accounts::status_command(engine).unwrap_or_default(),
                login = args.join(" "),
            )));
        }
        blocking(this, move |s| s.launch(engine, name, args, member)).await?
    }

    /// Starts the login's terminal. Blocking: the runtime starts it.
    fn launch(
        &self,
        engine: Engine,
        name: &'static str,
        args: &'static [&'static str],
        member: MemberId,
    ) -> Result<(SignIn, bool), StartError> {
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
            member,
        };
        self.terminals.link(record.id, record.terminal);
        let previous = self.lock().insert(engine, record.clone());
        if let Some(previous) = previous {
            // An ended one, replaced: its terminal goes now.
            self.terminals.unlink(previous.id);
            self.kill(previous.terminal);
        }
        if let Err(e) = self.save() {
            // Unlisted, a crash would leave it for good: it does not run.
            tracing::warn!(error = %e, "the list of sign-in terminals could not be written");
            self.forget(engine, &record);
            return Err(StartError::Failed(
                "The sign-in could not be noted in PitCrew's state folder, so it was stopped."
                    .to_owned(),
            ));
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

/// Runs `f` on `this` on the blocking pool.
async fn blocking<T: Send + 'static>(
    this: &Arc<SignIns>,
    f: impl FnOnce(&SignIns) -> T + Send + 'static,
) -> Result<T, StartError> {
    let this = Arc::clone(this);
    tokio::task::spawn_blocking(move || f(&this))
        .await
        .map_err(|_| StartError::Failed("The sign-in could not start.".to_owned()))
}

/// The terminal ids `path` lists; none when it is missing or unreadable.
fn read_ledger(path: &Path) -> Vec<TerminalId> {
    match std::fs::read(path) {
        Ok(bytes) => match serde_json::from_slice::<Ledger>(&bytes) {
            Ok(ledger) => ledger.terminals,
            Err(e) => {
                tracing::warn!(error = %e, "the list of sign-in terminals is malformed; ignoring it");
                Vec::new()
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => {
            tracing::warn!(error = %e, "the list of sign-in terminals could not be read");
            Vec::new()
        }
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

    /// The terminals route (`pitcrew_api::terminal::routes`) over these, where a sign-in's
    /// terminal opens only for the member who started it: another member's device token gets
    /// `403`. ([`Terminals::attach`] is not told who asks, so the check is a layer of the route.)
    /// Mount it as a **device** route.
    pub fn routes(self, config: TerminalConfig) -> Router {
        let sign_ins = Arc::clone(&self.sign_ins);
        pitcrew_api::terminal::routes(Arc::new(self), config).route_layer(
            axum::middleware::from_fn_with_state(sign_ins, only_its_starter),
        )
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

/// `GET /v1/sessions/{id}/terminal` for a sign-in's id: only for the member who started it.
async fn only_its_starter(
    State(sign_ins): State<Arc<SignIns>>,
    request: Request,
    next: Next,
) -> Response {
    let starter = request
        .uri()
        .path()
        .strip_prefix("/v1/sessions/")
        .and_then(|rest| rest.strip_suffix("/terminal"))
        .and_then(|id| id.parse::<SessionId>().ok())
        .and_then(|id| sign_ins.starter(id));
    if let Some(starter) = starter {
        let caller = request.extensions().get::<Caller>().map(|c| c.member);
        if caller != Some(starter) {
            return ErrorResponse::forbidden(
                "Only the person who started this sign-in can open its terminal.",
            )
            .into_response();
        }
    }
    next.run(request).await
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

        /// The terminals started and not killed.
        #[cfg(unix)]
        fn open(&self) -> Vec<TerminalId> {
            let killed = self.killed.lock().unwrap().clone();
            self.started
                .lock()
                .unwrap()
                .iter()
                .map(|(id, _)| *id)
                .filter(|id| !killed.contains(id))
                .collect()
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

    /// A stand-in CLI in `bin`: it answers its status command (not signed in) after a short
    /// pause, as a Node CLI takes a moment to start, and its login is never run here.
    #[cfg(unix)]
    fn stand_in(bin: &Path, name: &str, status: &str) {
        use std::os::unix::fs::PermissionsExt as _;
        let path = bin.join(name);
        std::fs::write(&path, format!("#!/bin/sh\nsleep 0.2\n{status}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// `bin` with a stand-in for each CLI, each answering its own status command. The rest of
    /// `PATH` is the system's folders, for `sleep`.
    #[cfg(unix)]
    fn tools(bin: &Path) -> Tools {
        stand_in(
            bin,
            "claude",
            r#"[ "$1 $2" = "auth status" ] && { echo '{"loggedIn":false}'; exit 1; }; exit 9"#,
        );
        stand_in(
            bin,
            "codex",
            r#"[ "$1 $2" = "login status" ] && { echo 'Not logged in' >&2; exit 1; }; exit 9"#,
        );
        stand_in(
            bin,
            "opencode",
            r#"[ "$1 $2" = "auth list" ] && { echo '0 credentials'; exit 0; }; exit 9"#,
        );
        let mut path = bin.as_os_str().to_owned();
        path.push(":/usr/bin:/bin");
        Tools::with_path(path)
    }

    fn rig(fake: Arc<Fake>, tools: Tools, ledger: Option<PathBuf>) -> SignIns {
        let runtime: Arc<dyn Runtime> = fake;
        SignIns::new(runtime, tools, std::env::temp_dir(), ledger)
    }

    #[cfg(unix)]
    fn person() -> MemberId {
        MemberId::new()
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
    #[tokio::test]
    async fn a_sign_in_is_a_terminal_only_the_terminals_route_knows() {
        let tmp = tempfile::tempdir().unwrap();
        let ledger = tmp.path().join(LEDGER);
        let fake = Arc::new(Fake::default());
        let sign_ins = Arc::new(rig(
            Arc::clone(&fake),
            tools(tmp.path()),
            Some(ledger.clone()),
        ));
        let routes = SignInTerminals::new(Arc::clone(&sign_ins), NoSessions);
        let sam = person();

        let (first, new) = SignIns::start(&sign_ins, Engine::Claude, SignInMethod::Browser, sam)
            .await
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
        assert_eq!(sign_ins.starter(first.terminal), Some(sam));
        // Its id is noted for a later daemon.
        assert_eq!(read_ledger(&ledger), [*terminal]);

        // Asking again while it runs gives the same one.
        let (again, new) = SignIns::start(&sign_ins, Engine::Claude, SignInMethod::Browser, sam)
            .await
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

        let short = Arc::new(
            rig(Arc::clone(&fake), tools(tmp.path()), None).with_times(Duration::ZERO, MAX_AGE),
        );
        let (ended, _) = SignIns::start(&short, Engine::Codex, SignInMethod::DeviceCode, sam)
            .await
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
        let old = Arc::new(
            rig(Arc::clone(&fake), tools(tmp.path()), None).with_times(LINGER, Duration::ZERO),
        );
        let (open, _) = SignIns::start(&old, Engine::OpenCode, SignInMethod::Browser, sam)
            .await
            .unwrap();
        old.sweep();
        assert!(old.get(Engine::OpenCode).is_none());
        let opencode = fake.started.lock().unwrap().last().unwrap().0;
        assert!(fake.killed.lock().unwrap().contains(&opencode));
        assert!(!open.terminal.to_string().is_empty());
    }

    /// Two asks for one CLI's sign-in at once start one terminal, and both get it: neither is
    /// left holding a terminal that the other's start replaced.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn two_starts_at_once_share_one_terminal() {
        let tmp = tempfile::tempdir().unwrap();
        let fake = Arc::new(Fake::default());
        let sign_ins = Arc::new(rig(Arc::clone(&fake), tools(tmp.path()), None));
        let sam = person();
        let (a, b) = tokio::join!(
            SignIns::start(&sign_ins, Engine::Claude, SignInMethod::Browser, sam),
            SignIns::start(&sign_ins, Engine::Claude, SignInMethod::Browser, sam),
        );
        let (a, a_new) = a.unwrap();
        let (b, b_new) = b.unwrap();
        assert_eq!(a.terminal, b.terminal, "both have the same terminal");
        assert!(a_new != b_new, "one started it, the other was given it");
        assert_eq!(fake.started.lock().unwrap().len(), 1, "one terminal");
        assert!(fake.killed.lock().unwrap().is_empty(), "none replaced");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn what_cannot_start_says_why() {
        let tmp = tempfile::tempdir().unwrap();
        let empty = Tools::with_path(std::ffi::OsString::from(tmp.path()));
        let fake = Arc::new(Fake::default());
        let sign_ins = Arc::new(rig(Arc::clone(&fake), empty, None));
        let sam = person();
        assert!(matches!(
            SignIns::start(&sign_ins, Engine::Claude, SignInMethod::Browser, sam).await,
            Err(StartError::NotInstalled(m)) if m.contains("Claude Code (claude) is not on this machine's PATH")
        ));
        assert!(matches!(
            SignIns::start(&sign_ins, Engine::Claude, SignInMethod::DeviceCode, sam).await,
            Err(StartError::Method(_))
        ));
        let bin = tmp.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let none: Arc<dyn Runtime> = Arc::new(crate::terminals::NoRuntime);
        let without = Arc::new(SignIns::new(none, tools(&bin), std::env::temp_dir(), None));
        assert!(matches!(
            SignIns::start(&without, Engine::Codex, SignInMethod::Browser, sam).await,
            Err(StartError::Unavailable(_))
        ));
        assert!(fake.started.lock().unwrap().is_empty());
    }

    /// An older Claude Code has no `auth` command: it reads `auth status` (and so `auth login`)
    /// as a prompt for an agent. Its login is never started; the answer says to update it.
    #[cfg(unix)]
    #[tokio::test]
    async fn an_older_claude_code_without_auth_is_not_started() {
        let tmp = tempfile::tempdir().unwrap();
        let tools = tools(tmp.path());
        // What an old Claude Code does with `auth status` and no terminal: it tries to run an
        // agent on the prompt "auth status", and fails for want of a TTY.
        stand_in(
            tmp.path(),
            "claude",
            "echo 'Error: Raw mode is not supported on the current process.stdin' >&2; exit 1",
        );
        let fake = Arc::new(Fake::default());
        let sign_ins = Arc::new(rig(Arc::clone(&fake), tools, None));
        let refused = SignIns::start(&sign_ins, Engine::Claude, SignInMethod::Browser, person())
            .await
            .unwrap_err();
        let StartError::Outdated(message) = refused else {
            panic!("not refused as outdated: {refused:?}");
        };
        assert_eq!(
            message,
            "Update Claude Code first: `claude auth status` gave no answer PitCrew understands, \
             so this Claude Code may not have `claude auth login`."
        );
        assert!(fake.started.lock().unwrap().is_empty(), "nothing started");
        assert!(sign_ins.get(Engine::Claude).is_none());
    }

    /// `DELETE …/sign-in` and the daemon's stop remove sign-in terminals, running or not, and
    /// the ledger with them.
    #[cfg(unix)]
    #[tokio::test]
    async fn sign_ins_stop_when_asked_and_with_the_daemon() {
        let tmp = tempfile::tempdir().unwrap();
        let ledger = tmp.path().join(LEDGER);
        let fake = Arc::new(Fake::default());
        let sign_ins = Arc::new(rig(
            Arc::clone(&fake),
            tools(tmp.path()),
            Some(ledger.clone()),
        ));
        let routes = SignInTerminals::new(Arc::clone(&sign_ins), NoSessions);
        let sam = person();
        let (codex, _) = SignIns::start(&sign_ins, Engine::Codex, SignInMethod::Browser, sam)
            .await
            .unwrap();
        assert_eq!(fake.open().len(), 1);
        assert!(sign_ins.stop(Engine::Codex), "stopped");
        assert!(fake.open().is_empty(), "its terminal is gone");
        assert!(sign_ins.get(Engine::Codex).is_none());
        assert!(routes.attach(codex.terminal).is_err());
        assert!(!sign_ins.stop(Engine::Codex), "nothing left to stop");
        assert!(read_ledger(&ledger).is_empty());

        for engine in [Engine::Claude, Engine::Codex, Engine::OpenCode] {
            SignIns::start(&sign_ins, engine, SignInMethod::Browser, sam)
                .await
                .unwrap();
        }
        assert_eq!(fake.open().len(), 3);
        assert_eq!(read_ledger(&ledger).len(), 3);
        sign_ins.stop_all();
        assert!(
            fake.open().is_empty(),
            "every sign-in stops with the daemon"
        );
        assert!(sign_ins.get(Engine::Claude).is_none());
        assert!(read_ledger(&ledger).is_empty());
    }

    /// At start, only the terminals the ledger lists are removed: a session's terminal whose name
    /// looks like a sign-in's is left alone.
    #[test]
    fn leftovers_of_an_earlier_daemon_are_removed() {
        let tmp = tempfile::tempdir().unwrap();
        let ledger = tmp.path().join(LEDGER);
        let info = |name: &str| TerminalInfo {
            id: TerminalId::new(),
            name: name.into(),
            pid: None,
            alive: true,
            native_target: None,
        };
        let earlier = info("pitcrew sign-in: claude");
        let session = info("work");
        let named_like_one = info("pitcrew sign-in: x");
        let gone = TerminalId::new();
        std::fs::write(
            &ledger,
            serde_json::to_vec(&Ledger {
                terminals: vec![earlier.id, gone],
            })
            .unwrap(),
        )
        .unwrap();
        let fake = Arc::new(Fake {
            leftovers: vec![earlier.clone(), session, named_like_one],
            ..Fake::default()
        });
        let sign_ins = rig(Arc::clone(&fake), Tools::default(), Some(ledger.clone()));
        sign_ins.remove_leftovers();
        assert_eq!(*fake.killed.lock().unwrap(), vec![earlier.id]);
        assert!(read_ledger(&ledger).is_empty(), "nothing is left to remove");

        // Without a ledger, nothing is removed at all.
        let fake = Arc::new(Fake {
            leftovers: vec![info("pitcrew sign-in: codex")],
            ..Fake::default()
        });
        rig(Arc::clone(&fake), Tools::default(), None).remove_leftovers();
        assert!(fake.killed.lock().unwrap().is_empty());
    }
}
