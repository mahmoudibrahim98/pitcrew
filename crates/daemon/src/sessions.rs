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
//! [`DISCOVERY`] for the runner to discover the session in that terminal (its
//! `session_discovered` names the terminal), and answers it as the hub stores it. If it does not
//! appear in time (Claude writes its transcript at its first prompt, so a start without a brief
//! may wait for a person), it answers `503 unavailable`: the CLI keeps running in its terminal,
//! and its session appears on the stream once its transcript does.
//!
//! - `agent` and `task` are not supported yet (`503`): a session started for an agent must be
//!   known under the agent before its CLI starts, which needs the runner to adopt a session id
//!   (stream D's dispatch work). `persona` is passed on; the runner does not use it yet.
//! - `machine` must be a machine of the workspace (`400` otherwise) and the runner's (`503` for
//!   another); `cwd` an absolute folder that exists there (`400`).
//!
//! **Answers.** An unknown session is `404`; one on another machine, or any without a runner,
//! `503`; an ended one, or one without a terminal here (PitCrew did not start it), `409`. A
//! command the runner refuses (a folder that does not exist, a permission mode it does not
//! allow, a value that could be read as an option) is `400`; one that fails (the runtime cannot
//! start or reach the terminal, or did not answer in time) is `503`. Commands run on the blocking
//! pool, at most [`MAX_COMMANDS`] at once (`503` past it), each bounded by the runner's own
//! timeouts and by [`COMMAND_TIMEOUT`] here.

use crate::runner::{Attached, Parts};
use axum::body::Bytes;
use axum::extract::rejection::PathRejection;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use pitcrew_auth::ErrorResponse;
use pitcrew_hub_work::{SessionFilter, WorkService};
use pitcrew_protocol::api::ErrorCode;
use pitcrew_protocol::ids::{
    CommandId, MachineId, MemberId, PersonaId, SessionId, TaskId, TerminalId,
};
use pitcrew_protocol::model::{Engine, PermissionMode, Session, SessionState};
use pitcrew_protocol::runner::{CommandOutcome, EndMode, Key, RunnerCommand};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;

/// How long `POST /v1/sessions` waits for the runner to discover the session it started.
pub const DISCOVERY: Duration = Duration::from_secs(30);
/// Commands running at once; past it a command answers `503` at once.
pub const MAX_COMMANDS: usize = 16;
/// The longest a command may take here: the runner bounds each terminal call (5 s), starting a
/// program (30 s) and a graceful end (10 s); this is a backstop over those.
pub const COMMAND_TIMEOUT: Duration = Duration::from_secs(45);
/// How often the hub is looked at while waiting for a started session.
const POLL: Duration = Duration::from_millis(100);
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
    commands: Semaphore,
}

impl Sessions {
    /// Commands for the sessions `work` knows, run by the runner on its machine once it runs.
    #[must_use]
    pub fn new(work: Arc<WorkService>, runner: Arc<Attached>) -> Self {
        Self {
            work,
            runner,
            commands: Semaphore::new(MAX_COMMANDS),
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

    /// Runs `command` on the blocking pool, bounded, and answers its outcome.
    async fn run(&self, runner: &Parts, command: RunnerCommand) -> Result<Done, ErrorResponse> {
        let Ok(_permit) = self.commands.try_acquire() else {
            return Err(unavailable(
                "Too many session commands are running; try again in a moment.",
            ));
        };
        let commands = runner.commands.clone();
        let running = tokio::task::spawn_blocking(move || commands.run(CommandId::new(), &command));
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

    /// The hub's session `id` on the runner's machine with a terminal here, ready for a command;
    /// or why not.
    async fn running(&self, id: SessionId) -> Result<Parts, ErrorResponse> {
        let work = Arc::clone(&self.work);
        let session = blocking(move || work.session(&id)).await?;
        let session = match session {
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
        if session.state == SessionState::Ended {
            return Err(conflict(format!("Session {id} has ended.")));
        }
        let terminals = runner.terminals.clone();
        match blocking(move || terminals.terminal_of(id)).await? {
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

    /// Runs the command `make` builds for session `id`, once it is ready for one.
    async fn command(
        &self,
        id: SessionId,
        make: impl FnOnce(SessionId) -> RunnerCommand,
    ) -> Result<StatusCode, ErrorResponse> {
        let runner = self.running(id).await?;
        self.run(&runner, make(id)).await?;
        Ok(StatusCode::NO_CONTENT)
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

#[derive(Debug, Deserialize)]
struct SendText {
    text: String,
}

#[derive(Debug, Deserialize)]
struct SendKeys {
    keys: Vec<Key>,
}

#[derive(Debug, Deserialize)]
struct End {
    mode: EndMode,
}

async fn start(
    State(sessions): State<Arc<Sessions>>,
    body: Bytes,
) -> Result<(StatusCode, Json<Session>), ErrorResponse> {
    let start: StartSession = parse(&body, "a StartSession")?;
    if start.agent.is_some() || start.task.is_some() {
        return Err(unavailable(
            "Starting a session for an agent or a task is not supported here yet; start it \
             without `agent` and `task`.",
        ));
    }
    if !std::path::Path::new(&start.cwd).is_absolute() {
        return Err(invalid("cwd must be an absolute folder."));
    }
    let runner = sessions.runner()?;
    let work = Arc::clone(&sessions.work);
    let machines = match blocking(move || work.machines()).await? {
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
    let name = window_name(start.engine, &start.cwd);
    let done = sessions
        .run(
            &runner,
            RunnerCommand::StartSession {
                engine: start.engine,
                cwd: start.cwd,
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
    match discovered(&sessions.work, &runner, terminal).await? {
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

/// Waits up to [`DISCOVERY`] for the hub to hold the session the runner found in `terminal`.
async fn discovered(
    work: &Arc<WorkService>,
    runner: &Parts,
    terminal: TerminalId,
) -> Result<Option<Session>, ErrorResponse> {
    let started = Instant::now();
    let mut rescans = RESCANS.iter().peekable();
    let filter = SessionFilter {
        machine: Some(runner.machine),
        ..SessionFilter::default()
    };
    loop {
        let (work, filter) = (Arc::clone(work), filter.clone());
        match blocking(move || work.sessions(&filter)).await? {
            Ok(sessions) => {
                if let Some(found) = sessions.into_iter().find(|s| s.terminal == Some(terminal)) {
                    return Ok(Some(found));
                }
            }
            Err(e) => tracing::warn!(error = %e, "cannot list sessions to find a started one"),
        }
        let waited = started.elapsed();
        if waited >= DISCOVERY {
            return Ok(None);
        }
        if rescans.next_if(|at| waited >= **at).is_some() {
            let commands = runner.commands.clone();
            // Discovery runs on the runner's watcher thread; this only asks for it.
            drop(tokio::task::spawn_blocking(move || {
                commands.run(CommandId::new(), &RunnerCommand::Scan { roots: Vec::new() })
            }));
        }
        tokio::time::sleep(POLL).await;
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
    id: Result<Path<String>, PathRejection>,
    body: Bytes,
) -> Result<StatusCode, ErrorResponse> {
    let id = session_id(id)?;
    let SendText { text } = parse(&body, "{ \"text\": String }")?;
    sessions
        .command(id, |session| RunnerCommand::SendText { session, text })
        .await
}

async fn keys(
    State(sessions): State<Arc<Sessions>>,
    id: Result<Path<String>, PathRejection>,
    body: Bytes,
) -> Result<StatusCode, ErrorResponse> {
    let id = session_id(id)?;
    let SendKeys { keys } = parse(&body, "{ \"keys\": Key[] }")?;
    if keys.is_empty() {
        return Err(invalid("keys must list at least one key."));
    }
    sessions
        .command(id, |session| RunnerCommand::SendKeys { session, keys })
        .await
}

async fn interrupt(
    State(sessions): State<Arc<Sessions>>,
    id: Result<Path<String>, PathRejection>,
) -> Result<StatusCode, ErrorResponse> {
    let id = session_id(id)?;
    sessions
        .command(id, |session| RunnerCommand::Interrupt { session })
        .await
}

async fn end(
    State(sessions): State<Arc<Sessions>>,
    id: Result<Path<String>, PathRejection>,
    body: Bytes,
) -> Result<StatusCode, ErrorResponse> {
    let id = session_id(id)?;
    let End { mode } = parse(&body, "{ \"mode\": \"graceful\" | \"kill\" }")?;
    sessions
        .command(id, |session| RunnerCommand::EndSession { session, mode })
        .await
}

/// The path's session id; a malformed one names no session, as on the other session routes.
fn session_id(id: Result<Path<String>, PathRejection>) -> Result<SessionId, ErrorResponse> {
    let Path(id) = id.map_err(|_| invalid("The session id must be plain text."))?;
    id.parse().map_err(|_| not_found(&id))
}

/// The body as `T`, or a `400` saying what it should be.
fn parse<T: DeserializeOwned>(body: &[u8], what: &str) -> Result<T, ErrorResponse> {
    serde_json::from_slice(body).map_err(|e| invalid(format!("The body must be {what}: {e}.")))
}

/// `f` on the blocking pool (a read of the hub's store, or the runner's index).
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> T + Send + 'static,
) -> Result<T, ErrorResponse> {
    tokio::task::spawn_blocking(f).await.map_err(|e| {
        tracing::error!(error = %e, "a read for a session command failed");
        ErrorResponse::new(ErrorCode::Internal, "The session could not be looked up.")
    })
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
    use pitcrew_protocol::model::{Liveness, Machine, MachineKind, Workspace};
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

    fn session(machine: MachineId, state: SessionState) -> Session {
        Session {
            id: SessionId::new(),
            engine: Engine::Claude,
            native_id: "n".into(),
            machine,
            cwd: "/w".into(),
            branch: None,
            title: None,
            agent: None,
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

    fn code<T>(r: &Result<T, ErrorResponse>) -> String {
        match r {
            Ok(_) => "ok".to_owned(),
            Err(e) => format!("{:?}", e.0.code),
        }
    }

    /// Which sessions take a command, before the runner is asked: unknown `404`; without a runner,
    /// or on another machine, `503`; ended, or without a terminal here, `409`. Starting needs a
    /// machine of the workspace, the runner's, and an absolute folder; `agent` and `task` are not
    /// supported yet. With no runtime, a start fails as `503`.
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
        let (idle, ended, remote) = (
            session(parts.machine, SessionState::Idle),
            session(parts.machine, SessionState::Ended),
            session(MachineId::new(), SessionState::Idle),
        );
        let author = pitcrew_protocol::ids::MemberId::new();
        let mut bodies = vec![EventBody::MachineAdded {
            machine: here.clone(),
        }];
        bodies.extend(
            [&idle, &ended, &remote].map(|s| EventBody::SessionDiscovered { session: s.clone() }),
        );
        let events: Vec<Event> = bodies
            .into_iter()
            .map(|b| Event::now(workspace.id, author, b))
            .collect();
        store.append(&events).unwrap();

        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let attached = Arc::new(Attached::default());
        let sessions = Arc::new(Sessions::new(work, Arc::clone(&attached)));
        let ready = |id| rt.block_on(sessions.running(id));
        assert_eq!(code(&ready(SessionId::new())), "NotFound");
        assert_eq!(code(&ready(idle.id)), "Unavailable", "no runner yet");
        attached.set(parts.clone());
        assert_eq!(code(&ready(SessionId::new())), "NotFound");
        assert_eq!(code(&ready(remote.id)), "Unavailable");
        assert_eq!(code(&ready(ended.id)), "Conflict");
        assert_eq!(code(&ready(idle.id)), "Conflict", "no terminal here");

        let starting = |body: serde_json::Value| {
            let body = Bytes::from(body.to_string());
            rt.block_on(start(State(Arc::clone(&sessions)), body))
                .map(|_| ())
        };
        let start_body = |machine: MachineId, cwd: &str| serde_json::json!({ "machine": machine, "engine": "claude", "cwd": cwd });
        let folder = tmp.path().to_str().unwrap().to_owned();
        assert_eq!(code(&starting(start_body(here.id, "work"))), "Invalid");
        assert_eq!(
            code(&starting(start_body(MachineId::new(), &folder))),
            "Invalid",
            "not a machine of the workspace"
        );
        let mut with_agent = start_body(here.id, &folder);
        with_agent["agent"] = serde_json::json!(author);
        assert_eq!(code(&starting(with_agent)), "Unavailable");
        // The idle runner's terminals have no runtime.
        assert_eq!(code(&starting(start_body(here.id, &folder))), "Unavailable");
        rt.block_on(runner.stop(Duration::from_secs(10)));
    }
}
