//! Dispatching: `POST /v1/tasks/{id}/dispatch` starts a session for an agent on a task.
//!
//! The hub records the dispatch, then asks a [`Dispatcher`] (the runner link) to start the
//! session:
//!
//! 1. Under the command lock, it checks the request (`404` unknown task; `400` unknown agent or
//!    machine, a person named as the agent, or a brief longer than [`MAX_BRIEF`]; `403` an agent
//!    the caller does not own: a person runs only their own agents; `409` a done or canceled
//!    task, or an agent that already holds an active dispatch on the task, such as a second
//!    click; `503` no live machine to run on), then asks whether it can start there at all (`503`
//!    without a dispatcher, or when [`Dispatcher::can_start`] says no: no runner attached, a
//!    machine it cannot reach).
//!    Nothing is recorded for any of these. Then it appends, in one transaction:
//!    - `task_assigned` to the agent, if the task has no assignee;
//!    - `dispatch_started`, naming the session it will run in (a new id);
//!    - `session_discovered` for that session: state `starting`, linked to the task and its
//!      workstream with `link_basis: dispatch`.
//! 2. Without the lock (a runner may take a while), it calls [`Dispatcher::start`]. The runner
//!    starts the CLI under the dispatch's session id ([`DispatchRequest::start_command`] names it),
//!    so the CLI's transcript is reported as that session, never as a second one.
//! 3. If that fails, it logs why, appends `dispatch_finished` (outcome `failed`, the reason as the
//!    summary) and `session_ended`, so no dispatch or session is left dangling, and answers `503`
//!    (the machine is unreachable), `409` (the runner refused) or `500` (it failed).
//!
//! **The task moves itself** ([`WorkService::follow_sessions`], fed what the runner reports):
//! - when the dispatched session first reports `working`, the task moves to in progress
//!   ([`WorkService::dispatch_working`]);
//! - when the agent reports the work done (it moves its task to review, which is what
//!   `pitcrew report <task> --review` does), the dispatch finishes as `succeeded`, in the same
//!   transaction as the move ([`WorkService::move_task`]); a task a person already moved to review
//!   counts as that report. The back office's `dispatch_to_review` rule moves a task still in
//!   progress when a dispatch succeeds;
//! - when the session ends without that report, the dispatch finishes as `canceled` ("stopped
//!   work"), or as `failed` if its CLI never reported the session at all.
//!
//! **A crash between steps 1 and 3** (the hub stops after the first append, before the start
//! returns or its failure is recorded) leaves a `starting` session and an open dispatch. The
//! runner link reconciles them at start, and whenever a start it made is not confirmed: if the
//! CLI did start, the runner reports it under [`DispatchRequest::session`]; if not,
//! [`WorkService::abandon_session`] finishes the dispatch as `failed` and ends the session.
//!
//! **Where it runs.** The machine is the request's `machine`, else the machine of the task's
//! workstream's first location, else the project's root, else the hub's own machine. The daemon
//! must name that one with [`WorkService::with_hub_machine`] (or, for a hub set up while it runs,
//! [`WorkService::set_hub_machine`]): without it, a dispatch with nowhere
//! else to run answers `503` rather than guessing among the workspace's machines. The folder is
//! the first of those locations on that machine, or `~` when none is. The engine, model and
//! permission mode come from the agent's persona (Claude Code by default).

use crate::error::{Result, WorkError};
use crate::query::{self, TaskRef};
use crate::service::{WorkService, no_task};
use pitcrew_protocol::api::{Caller, ErrorCode};
use pitcrew_protocol::events::{Event, EventBody};
use pitcrew_protocol::ids::{
    DispatchId, MachineId, MemberId, PersonaId, SessionId, TaskId, TaskKey, WorkstreamId,
};
use pitcrew_protocol::model::{
    Dispatch, DispatchOutcome, Engine, LinkBasis, Liveness, Location, Machine, Member, MemberKind,
    PermissionMode, Session, SessionState, Task, TaskStatus,
};
use pitcrew_protocol::runner::RunnerCommand;
use pitcrew_store::sql::Connection;
use serde::Deserialize;
use std::panic::{AssertUnwindSafe, catch_unwind};

/// The longest brief a dispatch passes its CLI, in bytes (it goes on the CLI's command line), as
/// `POST /v1/sessions` allows.
pub const MAX_BRIEF: usize = 64 * 1024;

/// `POST /v1/tasks/{id}/dispatch`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct NewDispatch {
    /// The agent to run.
    pub agent: MemberId,
    /// What to tell it first. Defaults to the task's description, or its title.
    #[serde(default)]
    pub brief: Option<String>,
    /// Where to run it. See the [module docs](self) for the default.
    #[serde(default)]
    pub machine: Option<MachineId>,
}

/// What the hub asks the runner link to start for a dispatch. Everything is decided: the runner
/// starts exactly this.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispatchRequest {
    /// The dispatch, already recorded (`dispatch_started`).
    pub dispatch: DispatchId,
    /// The id the session **must** have. The hub has recorded it (`session_discovered`, state
    /// `starting`, linked by `dispatch`); the runner reports on it under this id and does not
    /// discover it again under another.
    pub session: SessionId,
    /// The task.
    pub task: TaskId,
    /// The task's key, e.g. `PAP-5`.
    pub key: TaskKey,
    /// The task's workstream.
    pub workstream: Option<WorkstreamId>,
    /// The agent.
    pub agent: MemberId,
    /// The agent's owner, whom its events act for.
    pub owner: Option<MemberId>,
    /// The machine.
    pub machine: MachineId,
    /// The working directory on that machine (`~` for the home directory).
    pub cwd: String,
    /// The git branch of the location, when it names one.
    pub branch: Option<String>,
    /// The CLI.
    pub engine: Engine,
    /// The agent's persona.
    pub persona: Option<PersonaId>,
    /// The persona's model.
    pub model: Option<String>,
    /// The persona's permission mode.
    pub permission_mode: PermissionMode,
    /// The session's name: the task's key and title.
    pub name: String,
    /// The first prompt.
    pub brief: String,
}

impl DispatchRequest {
    /// The runner command that starts it.
    #[must_use]
    pub fn start_command(&self) -> RunnerCommand {
        RunnerCommand::StartSession {
            engine: self.engine,
            cwd: self.cwd.clone(),
            name: self.name.clone(),
            brief: Some(self.brief.clone()),
            persona: self.persona,
            model: self.model.clone(),
            account: None,
            permission_mode: self.permission_mode,
            session: Some(self.session),
        }
    }
}

/// Why a dispatched session could not be started.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DispatchError {
    /// The machine's runner cannot be reached. The route answers `503 unavailable`.
    #[error("the machine cannot be reached: {0}")]
    Unavailable(String),
    /// The runner refused, e.g. a permission mode it does not allow. `409 conflict`.
    #[error("the runner refused: {0}")]
    Rejected(String),
    /// It tried and failed. `500 internal`; the reason is logged and kept as the dispatch's
    /// summary.
    #[error("the session failed to start: {0}")]
    Failed(String),
}

/// Starts the sessions that dispatches ask for: the runner link. Give it to the service with
/// [`WorkService::with_dispatcher`].
///
/// [`Dispatcher::start`] is called on a blocking thread, after `dispatch_started` and
/// `session_discovered` are stored and without the service's command lock. It returns once the
/// runner has **accepted** the start (the program is launching), not when the agent is working;
/// the runner reports the session's progress as events for [`DispatchRequest::session`], which the
/// runner link hands to [`WorkService::follow_sessions`]. An error is recorded as a failed
/// dispatch.
pub trait Dispatcher: Send + Sync + std::fmt::Debug {
    /// Whether a session could start on `machine` now, asked before anything is recorded: an
    /// error refuses the dispatch with nothing appended (`503` for
    /// [`DispatchError::Unavailable`], such as no runner attached yet, `409` for
    /// [`DispatchError::Rejected`]). Called under the service's command lock: answer at once, and
    /// never call back into the service. The default says yes.
    ///
    /// # Errors
    ///
    /// [`DispatchError`]: why no session can start there.
    fn can_start(&self, machine: &MachineId) -> std::result::Result<(), DispatchError> {
        let _ = machine;
        Ok(())
    }

    /// Starts the session `request` describes.
    ///
    /// # Errors
    ///
    /// [`DispatchError`]: the machine is unreachable, the runner refused, or the start failed.
    fn start(&self, request: &DispatchRequest) -> std::result::Result<(), DispatchError>;
}

/// Everything decided under the lock.
struct Plan {
    task: Task,
    owner: Option<MemberId>,
    machine: Machine,
    cwd: String,
    branch: Option<String>,
    engine: Engine,
    persona: Option<PersonaId>,
    model: Option<String>,
    permission_mode: PermissionMode,
    brief: String,
}

/// Where a task runs by default: its workstream's locations, then its project's root.
fn locations(conn: &Connection, task: &Task) -> Result<Vec<Location>> {
    let mut out = match &task.workstream {
        Some(id) => query::workstream(conn, id)?.map_or_else(Vec::new, |w| w.locations),
        None => Vec::new(),
    };
    if let Some(root) = query::project(conn, &task.project)?.and_then(|p| p.root) {
        out.push(root);
    }
    Ok(out)
}

impl WorkService {
    /// Checks a dispatch by `caller` and decides where it runs. See the [module docs](self).
    fn plan_dispatch(
        &self,
        conn: &Connection,
        caller: &Caller,
        task: &TaskRef,
        new: &NewDispatch,
    ) -> Result<Plan> {
        let task = query::task(conn, task)?.ok_or_else(|| no_task(task))?;
        let agent = query::member(conn, &new.agent)?
            .ok_or_else(|| WorkError::invalid(format!("agent: no member {}.", new.agent)))?;
        if agent.kind != MemberKind::Agent {
            return Err(WorkError::invalid(format!(
                "agent must be an agent; {} is a person.",
                agent.handle
            )));
        }
        let requested = match &new.machine {
            Some(id) => Some(
                query::machine(conn, id)?
                    .ok_or_else(|| WorkError::invalid(format!("machine: no machine {id}.")))?,
            ),
            None => None,
        };
        let given = new.brief.clone().filter(|b| !b.trim().is_empty());
        let brief = given
            .clone()
            .or_else(|| Some(task.description.clone()).filter(|d| !d.trim().is_empty()))
            .unwrap_or_else(|| task.title.clone());
        if brief.len() > MAX_BRIEF {
            return Err(WorkError::invalid(if given.is_some() {
                format!("brief must be at most {MAX_BRIEF} bytes.")
            } else {
                format!(
                    "{}'s description is longer than a brief may be ({MAX_BRIEF} bytes); give a \
                     shorter brief.",
                    task.key
                )
            }));
        }
        require_owner(caller, &agent)?;
        if matches!(task.status, TaskStatus::Done | TaskStatus::Canceled) {
            let status = crate::codec::enum_text(&task.status).unwrap_or_default();
            return Err(WorkError::conflict(format!(
                "{} is {status}; reopen it before dispatching.",
                task.key
            )));
        }
        if query::has_active_dispatch(conn, &task.id, &agent.id)? {
            return Err(WorkError::conflict(format!(
                "{} is already working on {}; let that dispatch finish first.",
                agent.handle, task.key
            )));
        }
        let locations = locations(conn, &task)?;
        let machine = match requested {
            Some(machine) => machine,
            None => self.default_machine(conn, &locations)?,
        };
        if machine.liveness != Liveness::Live {
            let liveness = crate::codec::enum_text(&machine.liveness).unwrap_or_default();
            return Err(WorkError::unavailable(format!(
                "{} is {liveness}; its runner cannot be reached.",
                machine.name
            )));
        }
        let place = locations.iter().find(|l| l.machine == machine.id);
        let persona = match &agent.persona {
            Some(id) => query::persona(conn, id)?,
            None => None,
        };
        Ok(Plan {
            owner: agent.owner,
            cwd: place.map_or_else(|| "~".to_owned(), |l| l.path.clone()),
            branch: place.and_then(|l| l.branch.clone()),
            engine: persona.as_ref().map_or(Engine::Claude, |p| p.engine),
            persona: persona.as_ref().map(|p| p.id),
            model: persona.as_ref().and_then(|p| p.model.clone()),
            permission_mode: persona.map(|p| p.permission_mode).unwrap_or_default(),
            machine,
            task,
            brief,
        })
    }

    /// The machine of the first location, else the hub's own machine
    /// ([`WorkService::set_hub_machine`]); `unavailable` when there is neither.
    fn default_machine(&self, conn: &Connection, locations: &[Location]) -> Result<Machine> {
        if let Some(location) = locations.first() {
            return query::machine(conn, &location.machine)?.ok_or_else(|| {
                WorkError::unavailable(format!(
                    "The task's folder is on machine {}, which this hub does not know.",
                    location.machine
                ))
            });
        }
        let unavailable = || {
            WorkError::unavailable(
                "No machine can run this dispatch: the task has no folder and this hub has no \
                 machine of its own. Name a machine.",
            )
        };
        let id = self.hub_machine().ok_or_else(unavailable)?;
        query::machine(conn, &id)?.ok_or_else(unavailable)
    }

    /// Dispatches `task` to an agent: records the dispatch and its session, then starts the
    /// session through the [`Dispatcher`]. People only. Returns the dispatch as stored.
    ///
    /// See the [module docs](self) for the steps and the refusals.
    ///
    /// # Errors
    ///
    /// `forbidden` for an agent, or an agent the caller does not own; `not_found` for an unknown
    /// task; `invalid` for an unknown agent or machine, a person as the agent, or a brief longer
    /// than [`MAX_BRIEF`]; `conflict` for a done or canceled task, an agent already dispatched on
    /// it, or a runner that refused; `unavailable` with no dispatcher, no live machine, or an
    /// unreachable runner; `internal` when the start failed.
    pub fn dispatch_task(
        &self,
        caller: &Caller,
        task: &TaskRef,
        new: NewDispatch,
    ) -> Result<Dispatch> {
        crate::commands::require_person(caller, "Dispatching a task")?;
        let (request, dispatcher) = {
            let _guard = self.lock();
            let plan = self.read(|c| self.plan_dispatch(c, caller, task, &new))?;
            // After the plan, so its 404, 400 and 409 answer first, as api-v1 orders them.
            let dispatcher = self.dispatcher().ok_or_else(|| {
                WorkError::unavailable("This hub cannot start sessions: it has no runner link.")
            })?;
            let ready = catch_unwind(AssertUnwindSafe(|| dispatcher.can_start(&plan.machine.id)))
                .unwrap_or_else(|_| Err(DispatchError::Failed("the runner link panicked".into())));
            if let Err(error) = ready {
                return Err(refused(&error));
            }
            let now = self.now();
            let task = &plan.task;
            let session_id = SessionId::new();
            let dispatch = Dispatch {
                id: DispatchId::new(),
                task: task.id,
                agent: new.agent,
                session: Some(session_id),
                brief: plan.brief.clone(),
                started: now,
                ended: None,
                outcome: None,
                summary: None,
            };
            let request = DispatchRequest {
                dispatch: dispatch.id,
                session: session_id,
                task: task.id,
                key: task.key.clone(),
                workstream: task.workstream,
                agent: new.agent,
                owner: plan.owner,
                machine: plan.machine.id,
                cwd: plan.cwd,
                branch: plan.branch,
                engine: plan.engine,
                persona: plan.persona,
                model: plan.model,
                permission_mode: plan.permission_mode,
                name: format!("{} {}", task.key, task.title),
                brief: plan.brief,
            };
            let session = Session {
                id: request.session,
                engine: request.engine,
                // The CLI's own id is the runner's to report, once it knows it.
                native_id: String::new(),
                machine: request.machine,
                cwd: request.cwd.clone(),
                branch: request.branch.clone(),
                title: Some(task.title.clone()),
                agent: Some(request.agent),
                workstream: task.workstream,
                task: Some(task.id),
                link_basis: Some(LinkBasis::Dispatch),
                state: SessionState::Starting,
                status_line: None,
                started: now,
                last_activity: now,
                terminal: None,
                parent: None,
            };
            let mut events = Vec::with_capacity(3);
            if task.assignee.is_none() {
                events.push(self.by(
                    caller,
                    EventBody::TaskAssigned {
                        task: task.id,
                        assignee: Some(new.agent),
                    },
                ));
            }
            events.push(self.by(caller, EventBody::DispatchStarted { dispatch }));
            events.push(self.by(caller, EventBody::SessionDiscovered { session }));
            self.append(&events)?;
            (request, dispatcher)
        };

        // A panicking runner link must not leave the dispatch open either.
        let started = catch_unwind(AssertUnwindSafe(|| dispatcher.start(&request)))
            .unwrap_or_else(|_| Err(DispatchError::Failed("the runner link panicked".into())));
        if let Err(error) = started {
            // Logged first, so the reason survives even if recording the failure fails.
            tracing::warn!(
                dispatch = %request.dispatch,
                session = %request.session,
                machine = %request.machine,
                error = %error,
                "a dispatched session could not start"
            );
            let _guard = self.lock();
            self.append(&[
                self.by(
                    caller,
                    EventBody::DispatchFinished {
                        dispatch: request.dispatch,
                        outcome: DispatchOutcome::Failed,
                        summary: Some(format!("The session could not start: {error}.")),
                    },
                ),
                self.by(
                    caller,
                    EventBody::SessionEnded {
                        session: request.session,
                    },
                ),
            ])?;
            return Err(match error {
                DispatchError::Failed(why) => WorkError::internal(format!(
                    "dispatch {} failed to start its session: {why}",
                    request.dispatch
                )),
                other => refused(&other),
            });
        }
        self.dispatch(&request.dispatch)
    }

    /// Follows what the runner reported about sessions (`events`, as the store accepted them),
    /// moving the dispatches that run in them (see "The task moves itself" in the
    /// [module docs](self)):
    /// - the first `working` of a dispatched session (its `session_discovered` in that state, or
    ///   a `session_state_changed` to it) moves the task to in progress, once per dispatch while
    ///   this service runs: a person who moves the task back is not overruled by the agent's
    ///   next turn;
    /// - its `session_ended` (or a change to `ended`) finishes a dispatch still open: `canceled`
    ///   with "The session ended without a report.", or `failed` if the runner never reported
    ///   the session (its CLI never started).
    ///
    /// Events about sessions without an open dispatch change nothing, so replays are harmless.
    /// The runner link calls it after each batch the store accepts.
    ///
    /// # Errors
    ///
    /// Database errors. A move that loses a race with another writer is not an error.
    pub fn follow_sessions(&self, events: &[Event]) -> Result<()> {
        for event in events {
            match &event.body {
                EventBody::SessionDiscovered { session }
                    if session.state == SessionState::Working =>
                {
                    self.session_working(&session.id)?;
                }
                EventBody::SessionStateChanged {
                    session,
                    to: SessionState::Working,
                    ..
                } => self.session_working(session)?,
                EventBody::SessionEnded { session }
                | EventBody::SessionStateChanged {
                    session,
                    to: SessionState::Ended,
                    ..
                } => self.session_finished(session)?,
                _ => {}
            }
        }
        Ok(())
    }

    /// A dispatched session reported `working`: the first time, its task moves to in progress.
    fn session_working(&self, session: &SessionId) -> Result<()> {
        let Some(dispatch) = self.read(|c| query::active_dispatch_of(c, session))? else {
            return Ok(());
        };
        if !self.first_working(dispatch.id) {
            return Ok(());
        }
        match self.dispatch_working(&dispatch.id) {
            Ok(task) => {
                tracing::debug!(dispatch = %dispatch.id, task = %task.key, "a dispatched session is working");
                Ok(())
            }
            // It ended meanwhile, or another writer moved the task first.
            Err(e) if e.code() == ErrorCode::Conflict || e.code() == ErrorCode::NotFound => {
                tracing::debug!(dispatch = %dispatch.id, error = %e, "the task was not moved");
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    /// A dispatched session ended: its dispatch, if still open, finishes without a report.
    fn session_finished(&self, session: &SessionId) -> Result<()> {
        let _guard = self.lock();
        let found = self.read(|c| {
            let Some(dispatch) = query::active_dispatch_of(c, session)? else {
                return Ok(None);
            };
            let reported = query::session(c, session)?.is_some_and(|s| !s.native_id.is_empty());
            let owner = query::member(c, &dispatch.agent)?.and_then(|m| m.owner);
            Ok(Some((dispatch, reported, owner)))
        })?;
        let Some((dispatch, reported, owner)) = found else {
            return Ok(());
        };
        let (outcome, summary) = if reported {
            (DispatchOutcome::Canceled, ENDED_WITHOUT_REPORT)
        } else {
            (DispatchOutcome::Failed, NEVER_STARTED)
        };
        tracing::info!(dispatch = %dispatch.id, %session, outcome = ?outcome, "a dispatched session ended without a report");
        let body = EventBody::DispatchFinished {
            dispatch: dispatch.id,
            outcome,
            summary: Some(summary.to_owned()),
        };
        self.append(&[self.event(dispatch.agent, owner, body)])?;
        Ok(())
    }

    /// A session the hub stored ahead of the runner (a dispatch's, or a start for an agent or a
    /// task) whose CLI did not start: the runner has no terminal and no transcript for it, after
    /// a crash between the dispatch and its start, or once its terminal is gone. Its open
    /// dispatch, if any, finishes as `failed` with `reason`, and the session ends, in one
    /// transaction. Does nothing for a session that has ended, or that the runner has reported
    /// meanwhile (it has a CLI id): the runner link calls it from its reconciliation.
    ///
    /// # Errors
    ///
    /// `not_found` for an unknown session; database errors.
    pub fn abandon_session(&self, session: &SessionId, reason: &str) -> Result<()> {
        let _guard = self.lock();
        let (found, dispatch) = self.read(|c| {
            let found = query::session(c, session)?
                .ok_or_else(|| WorkError::not_found(format!("No session {session}.")))?;
            let dispatch = match query::active_dispatch_of(c, session)? {
                Some(d) => {
                    let owner = query::member(c, &d.agent)?.and_then(|m| m.owner);
                    Some((d, owner))
                }
                None => None,
            };
            Ok((found, dispatch))
        })?;
        if found.state == SessionState::Ended || !found.native_id.is_empty() {
            return Ok(());
        }
        // Authored by the dispatch's agent, else the session's, for its owner (as the hub's other
        // reports of a dispatch are); for a session without either, by the workspace's person.
        let (author, owner) = match (&dispatch, found.agent) {
            (Some((d, owner)), _) => (d.agent, *owner),
            (None, Some(agent)) => (
                agent,
                self.read(|c| Ok(query::member(c, &agent)?.and_then(|m| m.owner)))?,
            ),
            (None, None) => (
                self.read(query::first_person)?.ok_or_else(|| {
                    WorkError::internal("a session is stored, but the workspace has no person")
                })?,
                None,
            ),
        };
        let mut events = Vec::with_capacity(2);
        if let Some((d, _)) = &dispatch {
            events.push(self.event(
                author,
                owner,
                EventBody::DispatchFinished {
                    dispatch: d.id,
                    outcome: DispatchOutcome::Failed,
                    summary: Some(format!(
                        "The session could not start: {}.",
                        reason.trim_end_matches('.')
                    )),
                },
            ));
        }
        events.push(self.event(author, owner, EventBody::SessionEnded { session: *session }));
        tracing::warn!(%session, dispatch = ?dispatch.as_ref().map(|(d, _)| d.id), reason, "a session the hub stored never started; ending it");
        self.append(&events)?;
        Ok(())
    }

    /// Sessions on `machine` that the hub stored ahead of the runner and the runner has not
    /// reported yet (state `starting`, no CLI id): what the runner link reconciles.
    ///
    /// # Errors
    ///
    /// Database errors.
    pub fn unreported_sessions(&self, machine: &MachineId) -> Result<Vec<Session>> {
        self.read(|c| query::unreported_sessions(c, machine))
    }
}

/// A session a person starts for an agent or a task (`POST /v1/sessions` with `agent` or `task`):
/// recorded by [`WorkService::record_start`] before its CLI starts, so the runner reports the CLI
/// under it, as a dispatch's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedStart {
    /// The machine it runs on.
    pub machine: MachineId,
    /// The CLI.
    pub engine: Engine,
    /// Its folder there, as the CLI will start in it.
    pub cwd: String,
    /// The agent it runs as.
    pub agent: Option<MemberId>,
    /// The task it is linked to (`link_basis: manual`), and so the task's workstream.
    pub task: Option<TaskId>,
}

impl WorkService {
    /// Records a session a person starts for an agent or a task, before its CLI starts:
    /// `session_discovered`, state `starting`, with the agent, and linked to the task (and its
    /// workstream) with `link_basis: manual`. Returns it. The caller starts the CLI under its id,
    /// and calls [`WorkService::abandon_session`] if that fails.
    ///
    /// # Errors
    ///
    /// `forbidden` for an agent, or an agent the caller does not own (a person runs only their
    /// own agents, as for a dispatch); `invalid` for an unknown machine, agent or task, or a
    /// person named as the agent.
    pub fn record_start(&self, caller: &Caller, start: RecordedStart) -> Result<Session> {
        crate::commands::require_person(caller, "Starting a session")?;
        let _guard = self.lock();
        let task = self.read(|c| {
            if query::machine(c, &start.machine)?.is_none() {
                return Err(WorkError::invalid(format!(
                    "machine: no machine {}.",
                    start.machine
                )));
            }
            if let Some(id) = &start.agent {
                let agent = query::member(c, id)?
                    .ok_or_else(|| WorkError::invalid(format!("agent: no member {id}.")))?;
                if agent.kind != MemberKind::Agent {
                    return Err(WorkError::invalid(format!(
                        "agent must be an agent; {} is a person.",
                        agent.handle
                    )));
                }
                require_owner(caller, &agent)?;
            }
            match &start.task {
                Some(id) => query::task(c, &TaskRef::Id(*id))?
                    .map(Some)
                    .ok_or_else(|| WorkError::invalid(format!("task: no task {id}."))),
                None => Ok(None),
            }
        })?;
        let now = self.now();
        let session = Session {
            id: SessionId::new(),
            engine: start.engine,
            // The CLI's own id is the runner's to report, once it knows it.
            native_id: String::new(),
            machine: start.machine,
            cwd: start.cwd,
            branch: None,
            title: task.as_ref().map(|t| t.title.clone()),
            agent: start.agent,
            workstream: task.as_ref().and_then(|t| t.workstream),
            task: task.as_ref().map(|t| t.id),
            link_basis: task.as_ref().map(|_| LinkBasis::Manual),
            state: SessionState::Starting,
            status_line: None,
            started: now,
            last_activity: now,
            terminal: None,
            parent: None,
        };
        self.append(&[self.by(
            caller,
            EventBody::SessionDiscovered {
                session: session.clone(),
            },
        )])?;
        Ok(session)
    }
}

/// A person runs only their own agents: `forbidden` unless `caller` owns `agent`. An agent with
/// no owner is no one's to run.
fn require_owner(caller: &Caller, agent: &Member) -> Result<()> {
    if agent.owner == Some(caller.member) {
        return Ok(());
    }
    Err(WorkError::forbidden(format!(
        "{} is not your agent: a person may run only their own agents.",
        agent.handle
    )))
}

/// The summary of a dispatch whose session ended without the agent's report.
pub const ENDED_WITHOUT_REPORT: &str = "The session ended without a report.";
/// The summary of a dispatch whose session ended before the runner ever reported it.
pub const NEVER_STARTED: &str = "The session ended before its CLI started.";

/// The answer for a start refused before or by the runner: `503` unavailable, `409` rejected,
/// `500` failed.
fn refused(error: &DispatchError) -> WorkError {
    match error {
        DispatchError::Unavailable(why) => WorkError::unavailable(format!(
            "The session could not start: the machine cannot be reached ({why})."
        )),
        DispatchError::Rejected(why) => {
            WorkError::conflict(format!("The runner refused to start the session: {why}"))
        }
        DispatchError::Failed(why) => {
            WorkError::internal(format!("the session could not start: {why}"))
        }
    }
}
