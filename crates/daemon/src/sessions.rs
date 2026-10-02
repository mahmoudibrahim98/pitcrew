//! api-v1's session commands, run by the runner in this process (`RunnerCommands`) in the
//! terminals of the runtime chosen at start (`crate::runtime`). Device routes:
//!
//! | Route | Command |
//! |---|---|
//! | `POST /v1/sessions` (`StartSession` → `Session`, 202) | starts the CLI in a new terminal |
//! | `POST /v1/sessions/{id}/send` (`{ "text" }` → 204) | types the text, then Enter |
//! | `POST /v1/sessions/{id}/keys` (`{ "keys" }` → 204) | sends the keys |
//! | `POST /v1/sessions/{id}/interrupt` (→ 204) | Escape |
//! | `POST /v1/sessions/{id}/end` (`{ "mode" }` → 204) | two Ctrl-C and a wait (`graceful`), or kills it (`kill`) |
//!
//! **Starting.** The runner learns a session's id only when the CLI writes its transcript (Claude
//! is started with a session id of the runner's choosing, so the match is exact; other CLIs are
//! matched by folder and start time). So `POST /v1/sessions` starts the CLI, then waits up to
//! [`DISCOVERY`] for the runner to link the session to that terminal, and answers it as the hub
//! stores it. If it does not appear in time (Claude writes its transcript at its first prompt, so
//! a start without a brief may wait for a person), it answers `503 unavailable`: the CLI keeps
//! running in its terminal, and its session appears on the stream once its transcript does.
//!
//! - `agent` and `task` are not supported yet (`503`): a session started for an agent must be
//!   known under the agent before its CLI starts, which needs the runner to adopt a session id
//!   (stream D's dispatch work). `persona` is passed on; the runner does not use it yet.
//! - `machine` must be a machine of the workspace (`400` otherwise) and the runner's (`503` for
//!   another).
//! - `cwd` ([`checked_cwd`]): absolute, at most [`MAX_CWD`] bytes, an existing folder, resolved
//!   once here (links and `..`; on Windows `.` and `..` in its text, as Windows itself resolves
//!   them) and passed on resolved. On Unix it and every folder above it must belong to root or
//!   this user, and none may be writable by every user (o+w), except that a folder above it may be
//!   if it is sticky (as `/tmp`): anyone who can write there could plant files the CLI reads as its
//!   project's (settings, hooks), or swap the folder. Group-writable folders (a shared project)
//!   are allowed, and a start in one is logged at info, naming them.
//!
//! **Who may.** A person (device token) may command only a session with no agent, or one whose
//! agent they own: the runner's rule for hooks ([`may_command`], over the hub's sessions and
//! members). A session whose agent is not known is refused (`403`).
//!
//! **Bounds.** A body is at most `MAX_BODY` (1 MiB; `400` past it); `text` at most [`MAX_TEXT`]
//! bytes, `keys` at most [`MAX_KEYS`], `brief` at most [`MAX_BRIEF`] bytes. Commands run on the
//! blocking pool, at most [`MAX_COMMANDS`] at once, and starts (their wait for the session
//! included) at most [`MAX_STARTS`] at once (`503` past either); a command's permit is held until
//! the command returns, even after the request gave up on it ([`COMMAND_TIMEOUT`]). Every lookup
//! is bounded by [`LOOKUP_TIMEOUT`] (`503` past it).
//!
//! **Answers.** An unknown session is `404`; one on another machine, or any without a runner,
//! `503`; one the caller may not command `403`; an ended one, or one without a terminal here
//! (PitCrew did not start it), `409`. A command the runner refuses (a folder that does not exist,
//! a permission mode it does not allow, a value that could be read as an option) is `400`; one
//! that fails (the runtime cannot start or reach the terminal, or did not answer in time) is
//! `503`.

use crate::agents::HubAgents;
use crate::runner::{Attached, Parts};
use axum::body::Body;
use axum::extract::rejection::PathRejection;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use pitcrew_auth::{Authenticated, ErrorResponse};
use pitcrew_hub_work::WorkService;
use pitcrew_hub_work::routes::MAX_BODY;
use pitcrew_protocol::api::{Caller, ErrorCode, TokenScope};
use pitcrew_protocol::ids::{
    CommandId, MachineId, MemberId, PersonaId, SessionId, TaskId, TerminalId,
};
use pitcrew_protocol::model::{Engine, PermissionMode, Session, SessionState};
use pitcrew_protocol::runner::{CommandOutcome, EndMode, Key, RunnerCommand};
use pitcrew_runner::{SessionAgent, SessionAgents as _};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;

/// How long `POST /v1/sessions` waits for the runner to discover the session it started.
pub const DISCOVERY: Duration = Duration::from_secs(30);
/// Commands running at once; past it a command answers `503` at once.
pub const MAX_COMMANDS: usize = 16;
/// Starts under way at once, each from its command to the end of its wait for the session.
pub const MAX_STARTS: usize = 4;
/// The longest a command may take here: the runner bounds each terminal call (5 s), starting a
/// program (30 s) and a graceful end (10 s); this is a backstop over those.
pub const COMMAND_TIMEOUT: Duration = Duration::from_secs(45);
/// The longest a lookup (a session, its agent, its terminal, a folder) may take.
pub const LOOKUP_TIMEOUT: Duration = Duration::from_secs(5);
/// The most text `send` types, in bytes.
pub const MAX_TEXT: usize = 64 * 1024;
/// The most keys `keys` sends at once.
pub const MAX_KEYS: usize = 64;
/// The longest first prompt a start takes, in bytes.
pub const MAX_BRIEF: usize = 64 * 1024;
/// The longest `cwd`, in bytes.
pub const MAX_CWD: usize = 4096;
/// How often the runner's index is looked at while waiting for a started session.
const POLL: Duration = Duration::from_millis(200);
/// When, after a start, the runner is asked to look for new transcripts again, in case its folder
/// watches missed the new one.
const RESCANS: [Duration; 4] = [
    Duration::from_secs(1),
    Duration::from_secs(3),
    Duration::from_secs(7),
    Duration::from_secs(15),
];

/// The routes' state.
#[derive(Debug)]
pub struct Sessions {
    work: Arc<WorkService>,
    /// The runner, once it runs: its machine, terminals and commands.
    runner: Arc<Attached>,
    /// Who runs each session, as the hub knows it (the runner's hook rule asks the same).
    agents: Arc<HubAgents>,
    commands: Arc<Semaphore>,
    starts: Arc<Semaphore>,
}

impl Sessions {
    /// Commands for the sessions `work` knows, run by the runner on its machine once it runs.
    #[must_use]
    pub fn new(work: Arc<WorkService>, runner: Arc<Attached>) -> Self {
        Self {
            agents: Arc::new(HubAgents::new(Arc::clone(&work))),
            work,
            runner,
            commands: Arc::new(Semaphore::new(MAX_COMMANDS)),
            starts: Arc::new(Semaphore::new(MAX_STARTS)),
        }
    }

    /// The routes. Mount them as **device** routes (`RouterParts::device`).
    pub fn routes(self) -> Router {
        Router::new()
            .route("/v1/sessions", post(start))
            .route("/v1/sessions/{id}/send", post(send))
            .route("/v1/sessions/{id}/keys", post(keys))
            .route("/v1/sessions/{id}/interrupt", post(interrupt))
            .route("/v1/sessions/{id}/end", post(end))
            .with_state(Arc::new(self))
    }

    /// The runner, or `503`.
    fn runner(&self) -> Result<Parts, ErrorResponse> {
        self.runner.get().cloned().ok_or_else(|| {
            unavailable("No runner is attached to this hub, so it cannot run sessions.")
        })
    }

    /// Runs `command` on the blocking pool, bounded, and answers its outcome. Its permit is
    /// released only when the command returns, even if this gives up on it first.
    async fn run(&self, runner: &Parts, command: RunnerCommand) -> Result<Done, ErrorResponse> {
        let Ok(permit) = Arc::clone(&self.commands).try_acquire_owned() else {
            return Err(unavailable(
                "Too many session commands are running; try again in a moment.",
            ));
        };
        let commands = runner.commands.clone();
        let running = tokio::task::spawn_blocking(move || {
            let outcome = commands.run(CommandId::new(), &command);
            drop(permit);
            outcome
        });
        match tokio::time::timeout(COMMAND_TIMEOUT, running).await {
            Ok(Ok(CommandOutcome::Ok { detail })) => Ok(Done { detail }),
            Ok(Ok(CommandOutcome::Rejected { reason })) => Err(invalid(reason)),
            Ok(Ok(CommandOutcome::Failed { error })) => Err(unavailable(error)),
            Ok(Err(e)) => {
                tracing::error!(error = %e, "a session command failed");
                Err(ErrorResponse::new(
                    ErrorCode::Internal,
                    "The command could not be run.",
                ))
            }
            Err(_) => {
                tracing::warn!(
                    seconds = COMMAND_TIMEOUT.as_secs(),
                    "a session command has not returned; it is left running"
                );
                Err(unavailable("The command took too long."))
            }
        }
    }

    /// The hub's session `id` on the runner's machine, which `caller` may command, with a
    /// terminal here; or why not.
    async fn running(&self, caller: Caller, id: SessionId) -> Result<Parts, ErrorResponse> {
        let work = Arc::clone(&self.work);
        let session = match bounded(move || work.session(&id)).await? {
            Ok(session) => session,
            Err(e) if e.code() == ErrorCode::NotFound => return Err(not_found(&id)),
            Err(e) => {
                tracing::error!(error = %e, session = %id, "cannot look up a session for a command");
                return Err(ErrorResponse::new(
                    ErrorCode::Internal,
                    "The session could not be looked up.",
                ));
            }
        };
        let runner = self.runner()?;
        if session.machine != runner.machine {
            return Err(unavailable(format!(
                "Session {id} runs on another machine, which this hub cannot reach yet."
            )));
        }
        let agents = Arc::clone(&self.agents);
        let agent = bounded(move || agents.agent_of(id)).await?;
        if !may_command(caller, agent) {
            return Err(ErrorResponse::new(
                ErrorCode::Forbidden,
                format!(
                    "Session {id} runs as an agent you do not own, or one this hub does not know."
                ),
            ));
        }
        if session.state == SessionState::Ended {
            return Err(conflict(format!("Session {id} has ended.")));
        }
        let terminals = runner.terminals.clone();
        match bounded(move || terminals.terminal_of(id)).await? {
            Ok(Some(_)) => Ok(runner),
            Ok(None) => Err(conflict(format!(
                "Session {id} has no terminal here: PitCrew did not start it."
            ))),
            Err(e) => {
                tracing::error!(error = %e, session = %id, "cannot look up a session's terminal");
                Err(ErrorResponse::new(
                    ErrorCode::Internal,
                    "The session's terminal could not be looked up.",
                ))
            }
        }
    }

    /// Runs the command `make` builds for session `id`, once `caller` may and it is ready.
    async fn command(
        &self,
        caller: Caller,
        id: SessionId,
        make: impl FnOnce(SessionId) -> RunnerCommand,
    ) -> Result<StatusCode, ErrorResponse> {
        let runner = self.running(caller, id).await?;
        self.run(&runner, make(id)).await?;
        Ok(StatusCode::NO_CONTENT)
    }

    /// Waits up to [`DISCOVERY`] for the runner to link a session to `terminal`, and for the hub
    /// to hold it.
    async fn discovered(
        &self,
        runner: &Parts,
        terminal: TerminalId,
    ) -> Result<Option<Session>, ErrorResponse> {
        let started = Instant::now();
        let mut rescans = RESCANS.iter().peekable();
        loop {
            let terminals = runner.terminals.clone();
            match bounded(move || terminals.session_of(terminal)).await? {
                Ok(Some(id)) => {
                    let work = Arc::clone(&self.work);
                    match bounded(move || work.session(&id)).await? {
                        Ok(session) => return Ok(Some(session)),
                        // The runner's report of it is on its way to the store.
                        Err(e) if e.code() == ErrorCode::NotFound => {}
                        Err(e) => {
                            tracing::error!(error = %e, session = %id, "cannot look up a started session");
                            return Err(ErrorResponse::new(
                                ErrorCode::Internal,
                                "The started session could not be looked up.",
                            ));
                        }
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    tracing::error!(error = %e, %terminal, "cannot look up a terminal's session");
                    return Err(ErrorResponse::new(
                        ErrorCode::Internal,
                        "The started session could not be looked up.",
                    ));
                }
            }
            let waited = started.elapsed();
            if waited >= DISCOVERY {
                return Ok(None);
            }
            if rescans.next_if(|at| waited >= **at).is_some() {
                self.rescan(runner);
            }
            tokio::time::sleep(POLL).await;
        }
    }

    /// Asks the runner to look for new transcripts now (discovery runs on its watcher thread),
    /// unless every command permit is taken.
    fn rescan(&self, runner: &Parts) {
        let Ok(permit) = Arc::clone(&self.commands).try_acquire_owned() else {
            return;
        };
        let commands = runner.commands.clone();
        drop(tokio::task::spawn_blocking(move || {
            commands.run(CommandId::new(), &RunnerCommand::Scan { roots: Vec::new() });
            drop(permit);
        }));
    }
}

/// Whether `caller` may command a session that runs as `agent`: the runner's rule for hooks. A
/// person may command a session with no agent, or with an agent they own; an agent, only its own
/// sessions; nobody one whose agent is not known.
fn may_command(caller: Caller, agent: SessionAgent) -> bool {
    match (caller.scope, agent) {
        (_, SessionAgent::Unknown) => false,
        (TokenScope::Device, SessionAgent::NoAgent) => true,
        (TokenScope::Device, SessionAgent::Agent { owner, .. }) => owner == Some(caller.member),
        (TokenScope::Agent, SessionAgent::NoAgent) => false,
        (TokenScope::Agent, SessionAgent::Agent { agent, .. }) => agent == caller.member,
    }
}

/// A command that went well, with the runner's detail.
struct Done {
    detail: Option<serde_json::Value>,
}

/// `StartSession` (api-v1, "Sessions").
#[derive(Debug, Deserialize)]
struct StartSession {
    machine: MachineId,
    engine: Engine,
    cwd: String,
    #[serde(default)]
    agent: Option<MemberId>,
    #[serde(default)]
    task: Option<TaskId>,
    #[serde(default)]
    brief: Option<String>,
    #[serde(default)]
    persona: Option<PersonaId>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    permission_mode: Option<PermissionMode>,
}

impl StartSession {
    /// The bounds a start must keep before anything is looked up.
    fn check(&self) -> Result<(), ErrorResponse> {
        if self.cwd.len() > MAX_CWD {
            return Err(invalid(format!("cwd must be at most {MAX_CWD} bytes.")));
        }
        if self.brief.as_ref().is_some_and(|b| b.len() > MAX_BRIEF) {
            return Err(invalid(format!("brief must be at most {MAX_BRIEF} bytes.")));
        }
        if self.agent.is_some() || self.task.is_some() {
            return Err(unavailable(
                "Starting a session for an agent or a task is not supported here yet; start it \
                 without `agent` and `task`.",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
struct SendText {
    text: String,
}

impl SendText {
    fn check(&self) -> Result<(), ErrorResponse> {
        if self.text.len() > MAX_TEXT {
            return Err(invalid(format!("text must be at most {MAX_TEXT} bytes.")));
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
struct SendKeys {
    keys: Vec<Key>,
}

impl SendKeys {
    fn check(&self) -> Result<(), ErrorResponse> {
        if self.keys.is_empty() {
            return Err(invalid("keys must list at least one key."));
        }
        if self.keys.len() > MAX_KEYS {
            return Err(invalid(format!("keys must list at most {MAX_KEYS} keys.")));
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
struct End {
    mode: EndMode,
}

async fn start(
    State(sessions): State<Arc<Sessions>>,
    Authenticated(caller): Authenticated,
    body: Body,
) -> Result<(StatusCode, Json<Session>), ErrorResponse> {
    let start: StartSession = parse(&read(body).await?, "a StartSession")?;
    start.check()?;
    if caller.scope != TokenScope::Device {
        return Err(ErrorResponse::forbidden(
            "Only a person may start a session.",
        ));
    }
    // Held to the end of the wait for the session.
    let Ok(_starting) = Arc::clone(&sessions.starts).try_acquire_owned() else {
        return Err(unavailable(
            "Too many sessions are starting; try again in a moment.",
        ));
    };
    let runner = sessions.runner()?;
    let work = Arc::clone(&sessions.work);
    let machines = match bounded(move || work.machines()).await? {
        Ok(machines) => machines,
        Err(e) => {
            tracing::error!(error = %e, "cannot list the machines to start a session");
            return Err(ErrorResponse::new(
                ErrorCode::Internal,
                "The machines could not be listed.",
            ));
        }
    };
    if !machines.iter().any(|m| m.id == start.machine) {
        return Err(invalid(format!("machine: no machine {}.", start.machine)));
    }
    if start.machine != runner.machine {
        return Err(unavailable(format!(
            "Machine {} is not this hub's, and this hub cannot reach other machines yet.",
            start.machine
        )));
    }
    let given = start.cwd;
    let folder = bounded(move || checked_cwd(&given))
        .await?
        .map_err(invalid)?;
    if !folder.group_writable.is_empty() {
        tracing::info!(
            cwd = %folder.path,
            group_writable = ?folder.group_writable,
            "a session starts in a folder that members of its group can change"
        );
    }
    let name = window_name(start.engine, &folder.path);
    let done = sessions
        .run(
            &runner,
            RunnerCommand::StartSession {
                engine: start.engine,
                cwd: folder.path,
                name,
                brief: start.brief,
                persona: start.persona,
                model: start.model,
                account: None,
                permission_mode: start.permission_mode.unwrap_or_default(),
            },
        )
        .await?;
    let detail = done.detail.unwrap_or_default();
    let Some(terminal) = detail
        .get("terminal")
        .and_then(|t| serde_json::from_value::<TerminalId>(t.clone()).ok())
    else {
        tracing::error!(
            ?detail,
            "the runner started a session but named no terminal"
        );
        return Err(ErrorResponse::new(
            ErrorCode::Internal,
            "The session's terminal is not known.",
        ));
    };
    let target = detail
        .get("native_target")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    tracing::info!(%terminal, target = ?target, "started a session's CLI in a terminal");
    match sessions.discovered(&runner, terminal).await? {
        Some(session) => Ok((StatusCode::ACCEPTED, Json(session))),
        None => {
            let attach = target.map_or_else(String::new, |t| {
                format!(" A person can attach to it by hand (tmux target {t}).")
            });
            Err(unavailable(format!(
                "The CLI started in terminal {terminal}, but its session did not appear within \
                 {} seconds (Claude writes its transcript at its first prompt). It keeps \
                 running, and its session appears on the stream once its transcript does.{attach}",
                DISCOVERY.as_secs()
            )))
        }
    }
}

/// A folder a CLI may start in ([`checked_cwd`]).
#[derive(Debug, PartialEq, Eq)]
struct Folder {
    /// Resolved: links and `..` followed.
    path: String,
    /// It, or folders above it, that members of their group can change (logged at info).
    group_writable: Vec<String>,
}

/// `cwd` resolved (links and `..`; on Windows, in its text), if it is a folder PitCrew may start
/// a CLI in: absolute and existing; on Unix, it and every folder above it belong to root or this
/// user, and none is writable by every user (o+w) except a sticky folder above it. Group-writable
/// folders pass, and are named so the start can say so. Otherwise why not. Blocking.
fn checked_cwd(cwd: &str) -> Result<Folder, String> {
    let given = std::path::Path::new(cwd);
    if !given.is_absolute() {
        return Err("cwd must be an absolute folder.".to_owned());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let real = std::fs::canonicalize(given)
            .map_err(|e| format!("cwd {cwd:?} cannot be resolved: {e}."))?;
        if !real.is_dir() {
            return Err(format!("cwd {} is not a folder.", real.display()));
        }
        let me = pitcrew_auth::euid();
        let mut group_writable = Vec::new();
        for (i, folder) in real.ancestors().enumerate() {
            let meta = std::fs::metadata(folder)
                .map_err(|e| format!("cannot inspect {}: {e}.", folder.display()))?;
            if meta.uid() != 0 && meta.uid() != me {
                return Err(format!(
                    "{} belongs to another user (uid {}), so a session may not start under it.",
                    folder.display(),
                    meta.uid()
                ));
            }
            let mode = meta.mode();
            // A sticky folder above it (as /tmp) lets no one replace what is under it.
            let sticky_above = i > 0 && mode & 0o1000 != 0;
            if mode & 0o002 != 0 && !sticky_above {
                return Err(format!(
                    "{} can be changed by every user (mode {:03o}), so a session may not start \
                     under it.",
                    folder.display(),
                    mode & 0o7777
                ));
            }
            if mode & 0o020 != 0 && !sticky_above {
                group_writable.push(folder.display().to_string());
            }
        }
        let path = real
            .into_os_string()
            .into_string()
            .map_err(|_| "cwd must be UTF-8.".to_owned())?;
        Ok(Folder {
            path,
            group_writable,
        })
    }
    #[cfg(not(unix))]
    {
        // Windows resolves `.` and `..` in a path's text before it looks at any folder, so
        // `C:\w\none\..\work` is `C:\w\work` even where `none` does not exist. Resolved the same
        // way here, the runner gets the folder the CLI really starts in, with `\` separators.
        let real = std::path::absolute(given)
            .map_err(|e| format!("cwd {cwd:?} cannot be resolved: {e}."))?;
        if !real.is_dir() {
            return Err(format!("cwd {} is not a folder.", real.display()));
        }
        let path = real
            .into_os_string()
            .into_string()
            .map_err(|_| "cwd must be UTF-8.".to_owned())?;
        Ok(Folder {
            path,
            group_writable: Vec::new(),
        })
    }
}

/// The tmux window's name: the CLI and the folder's name.
fn window_name(engine: Engine, cwd: &str) -> String {
    let cli = serde_json::to_value(engine)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_else(|| "agent".to_owned());
    match std::path::Path::new(cwd)
        .file_name()
        .and_then(|n| n.to_str())
    {
        Some(folder) => format!("{cli} {folder}"),
        None => cli,
    }
}

async fn send(
    State(sessions): State<Arc<Sessions>>,
    Authenticated(caller): Authenticated,
    id: Result<Path<String>, PathRejection>,
    body: Body,
) -> Result<StatusCode, ErrorResponse> {
    let id = session_id(id)?;
    let send: SendText = parse(&read(body).await?, "{ \"text\": String }")?;
    send.check()?;
    let text = send.text;
    sessions
        .command(caller, id, |session| RunnerCommand::SendText {
            session,
            text,
        })
        .await
}

async fn keys(
    State(sessions): State<Arc<Sessions>>,
    Authenticated(caller): Authenticated,
    id: Result<Path<String>, PathRejection>,
    body: Body,
) -> Result<StatusCode, ErrorResponse> {
    let id = session_id(id)?;
    let send: SendKeys = parse(&read(body).await?, "{ \"keys\": Key[] }")?;
    send.check()?;
    let keys = send.keys;
    sessions
        .command(caller, id, |session| RunnerCommand::SendKeys {
            session,
            keys,
        })
        .await
}

async fn interrupt(
    State(sessions): State<Arc<Sessions>>,
    Authenticated(caller): Authenticated,
    id: Result<Path<String>, PathRejection>,
) -> Result<StatusCode, ErrorResponse> {
    let id = session_id(id)?;
    sessions
        .command(caller, id, |session| RunnerCommand::Interrupt { session })
        .await
}

async fn end(
    State(sessions): State<Arc<Sessions>>,
    Authenticated(caller): Authenticated,
    id: Result<Path<String>, PathRejection>,
    body: Body,
) -> Result<StatusCode, ErrorResponse> {
    let id = session_id(id)?;
    let End { mode } = parse(&read(body).await?, "{ \"mode\": \"graceful\" | \"kill\" }")?;
    sessions
        .command(caller, id, |session| RunnerCommand::EndSession {
            session,
            mode,
        })
        .await
}

/// The path's session id; a malformed one names no session, as on the other session routes.
fn session_id(id: Result<Path<String>, PathRejection>) -> Result<SessionId, ErrorResponse> {
    let Path(id) = id.map_err(|_| invalid("The session id must be plain text."))?;
    id.parse().map_err(|_| not_found(&id))
}

/// The body, at most `MAX_BODY` bytes; past it, or unreadable, `400 invalid`.
async fn read(body: Body) -> Result<axum::body::Bytes, ErrorResponse> {
    axum::body::to_bytes(body, MAX_BODY)
        .await
        .map_err(|_| invalid("The body is larger than 1 MiB, or could not be read."))
}

/// The body as `T`, or a `400` saying what it should be.
fn parse<T: DeserializeOwned>(body: &[u8], what: &str) -> Result<T, ErrorResponse> {
    serde_json::from_slice(body).map_err(|e| invalid(format!("The body must be {what}: {e}.")))
}

/// `f` on the blocking pool (a read of the hub's store, the runner's index, the filesystem), for
/// at most [`LOOKUP_TIMEOUT`].
async fn bounded<T: Send + 'static>(
    f: impl FnOnce() -> T + Send + 'static,
) -> Result<T, ErrorResponse> {
    match tokio::time::timeout(LOOKUP_TIMEOUT, tokio::task::spawn_blocking(f)).await {
        Ok(Ok(found)) => Ok(found),
        Ok(Err(e)) => {
            tracing::error!(error = %e, "a lookup for a session command failed");
            Err(ErrorResponse::new(
                ErrorCode::Internal,
                "The session could not be looked up.",
            ))
        }
        Err(_) => {
            tracing::warn!(
                seconds = LOOKUP_TIMEOUT.as_secs(),
                "a lookup for a session command has not returned; it is left running"
            );
            Err(unavailable("The session could not be looked up in time."))
        }
    }
}

fn not_found(session: &dyn std::fmt::Display) -> ErrorResponse {
    ErrorResponse::new(ErrorCode::NotFound, format!("No session {session}."))
}

fn invalid(message: impl Into<String>) -> ErrorResponse {
    ErrorResponse::new(ErrorCode::Invalid, message)
}

fn conflict(message: impl Into<String>) -> ErrorResponse {
    ErrorResponse::new(ErrorCode::Conflict, message)
}

fn unavailable(message: impl Into<String>) -> ErrorResponse {
    ErrorResponse::new(ErrorCode::Unavailable, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::Runner;
    use pitcrew_protocol::events::{Event, EventBody};
    use pitcrew_protocol::ids::WorkspaceId;
    use pitcrew_protocol::model::{Liveness, Machine, MachineKind, Member, MemberKind, Workspace};
    use pitcrew_store::{Store, StoreOptions};

    #[test]
    fn windows_are_named_by_cli_and_folder() {
        assert_eq!(window_name(Engine::Claude, "/w/paper"), "claude paper");
        assert_eq!(window_name(Engine::Codex, "/"), "codex");
        assert_eq!(window_name(Engine::OpenCode, "/a/b/"), "opencode b");
    }

    #[test]
    fn bodies_are_checked() {
        let start: StartSession = parse(
            br#"{"machine":"01JB000000000000000MCH0001","engine":"claude","cwd":"/w","permission_mode":"plan"}"#,
            "x",
        )
        .unwrap();
        assert_eq!(start.engine, Engine::Claude);
        assert_eq!(start.permission_mode, Some(PermissionMode::Plan));
        assert!(start.agent.is_none() && start.task.is_none());
        assert!(start.check().is_ok());
        for bad in [
            &br#"{"engine":"claude","cwd":"/w"}"#[..],
            br#"{"machine":"01JB000000000000000MCH0001","engine":"vim","cwd":"/w"}"#,
            b"",
            b"[]",
        ] {
            let e = parse::<StartSession>(bad, "a StartSession").unwrap_err();
            assert_eq!(e.0.code, ErrorCode::Invalid);
            assert!(e.0.message.contains("a StartSession"), "{}", e.0.message);
        }
        let keys: SendKeys = parse(br#"{"keys":["down","enter","ctrl_c"]}"#, "x").unwrap();
        assert_eq!(keys.keys, [Key::Down, Key::Enter, Key::CtrlC]);
        assert!(parse::<SendKeys>(br#"{"keys":["meta"]}"#, "x").is_err());
        assert!(parse::<End>(br#"{"mode":"later"}"#, "x").is_err());
        assert!(parse::<SendText>(br#"{"text":1}"#, "x").is_err());
    }

    /// Each bound, at its limit and one past it.
    #[test]
    fn bounds_are_kept() {
        let code = |r: Result<(), ErrorResponse>| r.err().map(|e| e.0.code);
        let text = |n: usize| SendText {
            text: "a".repeat(n),
        };
        assert_eq!(code(text(MAX_TEXT).check()), None);
        assert_eq!(code(text(MAX_TEXT + 1).check()), Some(ErrorCode::Invalid));
        let keys = |n: usize| SendKeys {
            keys: vec![Key::Enter; n],
        };
        assert_eq!(code(keys(MAX_KEYS).check()), None);
        assert_eq!(code(keys(MAX_KEYS + 1).check()), Some(ErrorCode::Invalid));
        assert_eq!(code(keys(0).check()), Some(ErrorCode::Invalid));
        let start = |cwd: usize, brief: usize| StartSession {
            machine: MachineId::new(),
            engine: Engine::Claude,
            cwd: format!("/{}", "c".repeat(cwd - 1)),
            agent: None,
            task: None,
            brief: Some("b".repeat(brief)),
            persona: None,
            model: None,
            permission_mode: None,
        };
        assert_eq!(code(start(MAX_CWD, MAX_BRIEF).check()), None);
        assert_eq!(
            code(start(MAX_CWD + 1, 1).check()),
            Some(ErrorCode::Invalid)
        );
        assert_eq!(
            code(start(1, MAX_BRIEF + 1).check()),
            Some(ErrorCode::Invalid)
        );
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let big = Body::from(vec![b' '; MAX_BODY + 1]);
        let e = rt.block_on(read(big)).unwrap_err();
        assert_eq!(e.0.code, ErrorCode::Invalid);
        let fits = Body::from(vec![b' '; MAX_BODY]);
        assert_eq!(rt.block_on(read(fits)).unwrap().len(), MAX_BODY);
    }

    /// The runner's hook rule: a person, sessions without an agent or with one they own; an
    /// agent, its own; nobody, one whose agent is not known.
    #[test]
    fn who_may_command_a_session() {
        let (sam, kim, writer) = (MemberId::new(), MemberId::new(), MemberId::new());
        let person = |member| Caller {
            member,
            scope: TokenScope::Device,
            on_behalf_of: None,
        };
        let agent = |member, owner| Caller {
            member,
            scope: TokenScope::Agent,
            on_behalf_of: Some(owner),
        };
        let runs_as = |agent, owner| SessionAgent::Agent {
            agent,
            owner: Some(owner),
        };
        assert!(may_command(person(sam), SessionAgent::NoAgent));
        assert!(may_command(person(sam), runs_as(writer, sam)));
        assert!(!may_command(person(kim), runs_as(writer, sam)));
        assert!(!may_command(
            person(sam),
            SessionAgent::Agent {
                agent: writer,
                owner: None
            }
        ));
        assert!(!may_command(person(sam), SessionAgent::Unknown));
        assert!(may_command(agent(writer, sam), runs_as(writer, sam)));
        assert!(!may_command(agent(writer, sam), SessionAgent::NoAgent));
        assert!(!may_command(
            agent(MemberId::new(), sam),
            runs_as(writer, sam)
        ));
        assert!(!may_command(agent(writer, sam), SessionAgent::Unknown));
    }

    /// A folder is resolved, `..` and links included. One under a folder every user can write
    /// to is refused, unless that folder is sticky and above it; so is one every user can write
    /// to itself. Group-writable folders, itself or above it, pass and are named.
    #[cfg(unix)]
    #[test]
    fn folders_are_resolved_and_must_be_closed_to_everyone() {
        use std::os::unix::fs::PermissionsExt as _;
        let tmp = tempfile::tempdir().unwrap();
        let real = std::fs::canonicalize(tmp.path()).unwrap();
        let work = real.join("work");
        std::fs::create_dir_all(work.join("sub")).unwrap();
        let path = |cwd: &str| checked_cwd(cwd).map(|f| f.path);
        let dotted = format!("{}/work/sub/..", real.display());
        assert_eq!(path(&dotted).unwrap(), work.to_str().unwrap());
        let link = real.join("link");
        std::os::unix::fs::symlink(&work, &link).unwrap();
        assert_eq!(
            path(link.to_str().unwrap()).unwrap(),
            work.to_str().unwrap()
        );
        assert!(
            checked_cwd(work.to_str().unwrap())
                .unwrap()
                .group_writable
                .is_empty()
        );
        assert!(path("work").is_err(), "relative");
        assert!(path(real.join("none").to_str().unwrap()).is_err());
        std::fs::write(real.join("file"), "x").unwrap();
        assert!(path(real.join("file").to_str().unwrap()).is_err());

        // Under a folder every user can write to.
        let shared = real.join("shared");
        std::fs::create_dir_all(shared.join("w")).unwrap();
        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o777)).unwrap();
        let why = path(shared.join("w").to_str().unwrap()).unwrap_err();
        assert!(why.contains("changed by every user"), "{why}");
        // Sticky, as /tmp: fine above, not as the folder itself.
        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o1777)).unwrap();
        let under = checked_cwd(shared.join("w").to_str().unwrap()).unwrap();
        assert!(under.group_writable.is_empty(), "{under:?}");
        assert!(path(shared.to_str().unwrap()).is_err());
        // Writable by every user itself.
        std::fs::set_permissions(&work, std::fs::Permissions::from_mode(0o757)).unwrap();
        assert!(path(work.to_str().unwrap()).is_err());

        // Group-writable, itself or above it: allowed, and named.
        std::fs::set_permissions(&work, std::fs::Permissions::from_mode(0o775)).unwrap();
        let itself = checked_cwd(work.to_str().unwrap()).unwrap();
        assert_eq!(itself.group_writable, [work.to_str().unwrap()]);
        let team = real.join("team");
        std::fs::create_dir_all(team.join("w")).unwrap();
        std::fs::set_permissions(&team, std::fs::Permissions::from_mode(0o2775)).unwrap();
        let below = checked_cwd(team.join("w").to_str().unwrap()).unwrap();
        assert_eq!(below.path, team.join("w").to_str().unwrap());
        assert_eq!(below.group_writable, [team.to_str().unwrap()]);
        std::fs::set_permissions(&work, std::fs::Permissions::from_mode(0o755)).unwrap();
        // `..` out of a refused folder resolves to where it leads, which is checked instead.
        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o777)).unwrap();
        let out = format!("{}/shared/w/../../work", real.display());
        assert_eq!(path(&out).unwrap(), work.to_str().unwrap());
    }

    /// On Windows a folder is resolved as Windows resolves it: `.`, `..` and `/` in the text,
    /// through a folder that does not exist too. Relative, missing and file paths are refused.
    #[cfg(windows)]
    #[test]
    fn folders_are_resolved_as_windows_resolves_them() {
        let tmp = tempfile::tempdir().unwrap();
        let work = tmp.path().join("work");
        std::fs::create_dir_all(work.join("sub")).unwrap();
        let path = |cwd: &str| checked_cwd(cwd).map(|f| f.path);
        let root = tmp.path().display();
        assert_eq!(
            path(&format!(r"{root}\none\..\work")).unwrap(),
            work.to_str().unwrap()
        );
        assert_eq!(
            path(&format!("{root}/work/sub/..")).unwrap(),
            work.to_str().unwrap()
        );
        assert_eq!(
            path(&format!(r"{root}\work\.")).unwrap(),
            work.to_str().unwrap()
        );
        assert!(path("work").is_err(), "relative");
        assert!(path(r"\work").is_err(), "no drive");
        assert!(path(&format!(r"{root}\none\work")).is_err(), "missing");
        std::fs::write(tmp.path().join("file"), "x").unwrap();
        assert!(path(&format!(r"{root}\file")).is_err(), "a file");
    }

    fn session(machine: MachineId, state: SessionState, agent: Option<MemberId>) -> Session {
        Session {
            id: SessionId::new(),
            engine: Engine::Claude,
            native_id: "n".into(),
            machine,
            cwd: "/w".into(),
            branch: None,
            title: None,
            agent,
            workstream: None,
            task: None,
            link_basis: None,
            state,
            status_line: None,
            started: 1,
            last_activity: 1,
            terminal: None,
            parent: None,
        }
    }

    fn member(kind: MemberKind, handle: &str, owner: Option<MemberId>) -> Member {
        Member {
            id: MemberId::new(),
            kind,
            handle: handle.into(),
            name: handle.trim_start_matches('@').into(),
            owner,
            persona: None,
        }
    }

    fn code<T>(r: &Result<T, ErrorResponse>) -> String {
        match r {
            Ok(_) => "ok".to_owned(),
            Err(e) => format!("{:?}", e.0.code),
        }
    }

    /// Which sessions take a command, before the runner is asked: unknown `404`; without a runner,
    /// or on another machine, `503`; another person's agent's, or an unknown agent's, `403`;
    /// ended, or without a terminal here, `409`. Starting needs a person, a machine of the
    /// workspace, the runner's, and an absolute folder; `agent` and `task` are not supported yet.
    /// With no runtime, a start fails as `503`.
    #[test]
    fn which_sessions_take_commands() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(
            Store::open_with(
                tmp.path().join("hub.db"),
                StoreOptions::default(),
                pitcrew_hub_work::projections(),
            )
            .unwrap(),
        );
        let workspace = Workspace {
            id: WorkspaceId::new(),
            name: "Lab".into(),
        };
        let work = Arc::new(WorkService::new(Arc::clone(&store), workspace.clone()));
        let runner = Runner::idle(&tmp.path().join("runner"));
        let parts = runner.parts();
        let here = Machine {
            id: parts.machine,
            name: "PC".into(),
            kind: MachineKind::Local,
            info: None,
            liveness: Liveness::Live,
        };
        let lee = member(MemberKind::Human, "@lee", None);
        let kim = member(MemberKind::Human, "@kim", None);
        let lees = member(MemberKind::Agent, "@helper", Some(lee.id));
        let kims = member(MemberKind::Agent, "@other", Some(kim.id));
        let (idle, ended, remote, mine, theirs, stranger) = (
            session(parts.machine, SessionState::Idle, None),
            session(parts.machine, SessionState::Ended, None),
            session(MachineId::new(), SessionState::Idle, None),
            session(parts.machine, SessionState::Idle, Some(lees.id)),
            session(parts.machine, SessionState::Idle, Some(kims.id)),
            session(parts.machine, SessionState::Idle, Some(MemberId::new())),
        );
        let mut bodies = vec![EventBody::MachineAdded {
            machine: here.clone(),
        }];
        bodies.extend(
            [&lee, &kim, &lees, &kims].map(|m| EventBody::MemberAdded { member: m.clone() }),
        );
        bodies.extend(
            [&idle, &ended, &remote, &mine, &theirs, &stranger]
                .map(|s| EventBody::SessionDiscovered { session: s.clone() }),
        );
        let events: Vec<Event> = bodies
            .into_iter()
            .map(|b| Event::now(workspace.id, lee.id, b))
            .collect();
        store.append(&events).unwrap();

        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let attached = Arc::new(Attached::default());
        let sessions = Arc::new(Sessions::new(work, Arc::clone(&attached)));
        let as_lee = Caller {
            member: lee.id,
            scope: TokenScope::Device,
            on_behalf_of: None,
        };
        let ready = |id| rt.block_on(sessions.running(as_lee, id));
        assert_eq!(code(&ready(SessionId::new())), "NotFound");
        assert_eq!(code(&ready(idle.id)), "Unavailable", "no runner yet");
        attached.set(parts.clone());
        assert_eq!(code(&ready(SessionId::new())), "NotFound");
        assert_eq!(code(&ready(remote.id)), "Unavailable");
        assert_eq!(
            code(&ready(theirs.id)),
            "Forbidden",
            "another person's agent"
        );
        assert_eq!(code(&ready(stranger.id)), "Forbidden", "an unknown agent");
        assert_eq!(code(&ready(ended.id)), "Conflict");
        assert_eq!(code(&ready(idle.id)), "Conflict", "no terminal here");
        assert_eq!(
            code(&ready(mine.id)),
            "Conflict",
            "lee's agent: no terminal"
        );

        let starting = |caller: Caller, body: serde_json::Value| {
            let body = Body::from(body.to_string());
            rt.block_on(start(
                State(Arc::clone(&sessions)),
                Authenticated(caller),
                body,
            ))
            .map(|_| ())
        };
        let start_body = |machine: MachineId, cwd: &str| serde_json::json!({ "machine": machine, "engine": "claude", "cwd": cwd });
        let folder = std::fs::canonicalize(tmp.path()).unwrap();
        let folder = folder.to_str().unwrap();
        assert_eq!(
            code(&starting(as_lee, start_body(here.id, "work"))),
            "Invalid"
        );
        assert_eq!(
            code(&starting(as_lee, start_body(MachineId::new(), folder))),
            "Invalid",
            "not a machine of the workspace"
        );
        let mut with_agent = start_body(here.id, folder);
        with_agent["agent"] = serde_json::json!(lees.id);
        assert_eq!(code(&starting(as_lee, with_agent)), "Unavailable");
        let as_agent = Caller {
            member: lees.id,
            scope: TokenScope::Agent,
            on_behalf_of: Some(lee.id),
        };
        assert_eq!(
            code(&starting(as_agent, start_body(here.id, folder))),
            "Forbidden"
        );
        // The idle runner's terminals have no runtime.
        assert_eq!(
            code(&starting(as_lee, start_body(here.id, folder))),
            "Unavailable"
        );
        rt.block_on(runner.stop(Duration::from_secs(10)));
    }
}
