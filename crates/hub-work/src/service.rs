//! [`WorkService`]: the work model's reads and commands over one store.

use crate::dispatch::Dispatcher;
use crate::error::{Result, WorkError};
use crate::query::{self, AskFilter, SessionFilter, TaskFilter, TaskRef};
use crate::recap::RecapSync;
use pitcrew_protocol::api::{Caller, TokenScope};
use pitcrew_protocol::events::{BriefTarget, Event, EventBody};
use pitcrew_protocol::ids::{
    AskId, DispatchId, MachineId, MemberId, ProjectId, SessionId, TaskId, WorkspaceId, WorkstreamId,
};
use pitcrew_protocol::model::{
    Ask, Brief, Dispatch, Machine, Member, Persona, Project, Session, Task, Team, TimestampMs,
    Workspace, Workstream,
};
use pitcrew_store::sql::Connection;
use pitcrew_store::{RevRange, Store};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

/// Where commands get the time from. The default is the system clock.
pub type Clock = Arc<dyn Fn() -> TimestampMs + Send + Sync>;

fn system_clock() -> TimestampMs {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// `GET /v1/workspace`: the workspace, and the event revision the work model reflects.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceAt {
    /// The workspace.
    pub workspace: Workspace,
    /// The revision every work table reflects. A client that loads the work model and then
    /// opens the stream with `since=rev` misses nothing.
    pub rev: u64,
}

/// The work model of one workspace: reads of its projections, and commands that validate a
/// change against them and append the events that make it.
///
/// The store must have been opened with [`crate::projections`]. Reads never wait for commands.
///
/// # One writer
///
/// **Exactly one `Arc<WorkService>` per store appends work events.** Build it once, share it (the
/// routes take it as an extension, the runner link and the back office get clones of the same
/// `Arc`), and do not append work events to the store any other way.
///
/// The service's command lock covers validating a command and appending its events, so two
/// commands cannot both pass a check that only one of them should: two `create_task`s never get
/// the same key, two moves never both start from the same status. A second service (or process)
/// writing to the same store would break that. The projections stay deterministic even then (a
/// `task_created` whose key is taken is recorded as a clash and not applied; a `task_moved` whose
/// `from` is stale is ignored), and the losing command answers `409 conflict`: `create_task` when
/// its key was taken, `move_task` and `dispatch_working` when the task ended up somewhere other
/// than where they moved it. The rule is what keeps commands from losing.
///
/// Every event a command appends is stamped from the [`Caller`]: `author` is the caller's member
/// and, for an agent, `on_behalf_of` is its owner; never anything from a request body.
pub struct WorkService {
    store: Arc<Store>,
    workspace: Workspace,
    clock: Clock,
    writes: Mutex<()>,
    dispatcher: Option<Arc<dyn Dispatcher>>,
    hub_machine: Option<MachineId>,
    /// The recap index, built from the log on first use (see [`crate::RecapIndex`]).
    recaps: Mutex<RecapSync>,
}

impl std::fmt::Debug for WorkService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkService")
            .field("workspace", &self.workspace)
            .field("store", &self.store)
            .field("dispatcher", &self.dispatcher)
            .field("hub_machine", &self.hub_machine)
            .finish_non_exhaustive()
    }
}

impl WorkService {
    /// A service over `store` (opened with [`crate::projections`]) for `workspace`. See "One
    /// writer" above: make one per store.
    #[must_use]
    pub fn new(store: Arc<Store>, workspace: Workspace) -> Self {
        Self {
            store,
            workspace,
            clock: Arc::new(system_clock),
            writes: Mutex::new(()),
            dispatcher: None,
            hub_machine: None,
            recaps: Mutex::new(RecapSync::default()),
        }
    }

    /// Uses `clock` for the times commands record (`created`, `at` of answers and events).
    #[must_use]
    pub fn with_clock(mut self, clock: Clock) -> Self {
        self.clock = clock;
        self
    }

    /// Starts dispatched sessions through `dispatcher` (the runner link). Without one,
    /// `POST /v1/tasks/{id}/dispatch` answers `503 unavailable`.
    #[must_use]
    pub fn with_dispatcher(mut self, dispatcher: Arc<dyn Dispatcher>) -> Self {
        self.dispatcher = Some(dispatcher);
        self
    }

    /// The machine the hub runs on, where a dispatch runs when the task has no folder. The daemon
    /// must set it: without it, such a dispatch answers `503 unavailable` (the hub does not guess
    /// one of the workspace's machines).
    #[must_use]
    pub fn with_hub_machine(mut self, machine: MachineId) -> Self {
        self.hub_machine = Some(machine);
        self
    }

    /// The store.
    #[must_use]
    pub fn store(&self) -> &Arc<Store> {
        &self.store
    }

    /// The workspace's id.
    #[must_use]
    pub fn workspace(&self) -> WorkspaceId {
        self.workspace.id
    }

    /// The workspace and the revision the work model reflects (`GET /v1/workspace`).
    ///
    /// # Errors
    ///
    /// Database errors.
    pub fn workspace_at(&self) -> Result<WorkspaceAt> {
        let rev = self.read(query::work_rev)?;
        Ok(WorkspaceAt {
            workspace: self.workspace.clone(),
            rev,
        })
    }

    pub(crate) fn dispatcher(&self) -> Option<Arc<dyn Dispatcher>> {
        self.dispatcher.clone()
    }

    pub(crate) fn hub_machine(&self) -> Option<MachineId> {
        self.hub_machine
    }

    pub(crate) fn recap_lock(&self) -> &Mutex<RecapSync> {
        &self.recaps
    }

    pub(crate) fn now(&self) -> TimestampMs {
        (self.clock)()
    }

    /// Takes the command lock. A panic while holding it cannot leave anything half-written (the
    /// store's append is one transaction), so a poisoned lock is still usable.
    pub(crate) fn lock(&self) -> MutexGuard<'_, ()> {
        self.writes.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Runs `f` on one consistent snapshot of the tables. See the [`query`] module.
    ///
    /// # Errors
    ///
    /// Whatever `f` returns, or a database error.
    pub fn read<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        self.store.read(f)
    }

    /// An event stamped with `author` and `on_behalf_of`, now.
    pub(crate) fn event(
        &self,
        author: MemberId,
        on_behalf_of: Option<MemberId>,
        body: EventBody,
    ) -> Event {
        Event {
            id: pitcrew_protocol::ids::EventId::new(),
            at: self.now(),
            workspace: self.workspace.id,
            author,
            on_behalf_of,
            body,
        }
    }

    /// An event by `caller`: `author` is its member and, only for an agent, `on_behalf_of` its
    /// owner. A person acts for nobody, whatever the caller says.
    pub(crate) fn by(&self, caller: &Caller, body: EventBody) -> Event {
        let on_behalf_of = match caller.scope {
            TokenScope::Agent => caller.on_behalf_of,
            TokenScope::Device => None,
        };
        self.event(caller.member, on_behalf_of, body)
    }

    pub(crate) fn append(&self, events: &[Event]) -> Result<RevRange> {
        Ok(self.store.append(events)?)
    }

    /// Appends the events whose ids the log does not hold yet, in one transaction.
    pub(crate) fn append_new(&self, events: &[Event]) -> Result<RevRange> {
        Ok(self.store.append_new(events)?.0)
    }

    // ─── Reads ───────────────────────────────────────────────────────────────────────────────

    /// Every machine.
    ///
    /// # Errors
    ///
    /// Database errors.
    pub fn machines(&self) -> Result<Vec<Machine>> {
        self.read(query::machines)
    }

    /// Every member, people and agents.
    ///
    /// # Errors
    ///
    /// Database errors.
    pub fn members(&self) -> Result<Vec<Member>> {
        self.read(query::members)
    }

    /// One member; `not_found` if unknown.
    ///
    /// # Errors
    ///
    /// `not_found`, or database errors.
    pub fn member(&self, id: &MemberId) -> Result<Member> {
        self.read(|c| query::member(c, id))?
            .ok_or_else(|| WorkError::not_found(format!("No member {id}.")))
    }

    /// Every persona.
    ///
    /// # Errors
    ///
    /// Database errors.
    pub fn personas(&self) -> Result<Vec<Persona>> {
        self.read(query::personas)
    }

    /// Every team.
    ///
    /// # Errors
    ///
    /// Database errors.
    pub fn teams(&self) -> Result<Vec<Team>> {
        self.read(query::teams)
    }

    /// Every project.
    ///
    /// # Errors
    ///
    /// Database errors.
    pub fn projects(&self) -> Result<Vec<Project>> {
        self.read(query::projects)
    }

    /// One project; `not_found` if unknown.
    ///
    /// # Errors
    ///
    /// `not_found`, or database errors.
    pub fn project(&self, id: &ProjectId) -> Result<Project> {
        self.read(|c| query::project(c, id))?
            .ok_or_else(|| WorkError::not_found(format!("No project {id}.")))
    }

    /// Workstreams, all or those of one project.
    ///
    /// # Errors
    ///
    /// Database errors.
    pub fn workstreams(&self, project: Option<&ProjectId>) -> Result<Vec<Workstream>> {
        self.read(|c| query::workstreams(c, project))
    }

    /// One workstream; `not_found` if unknown.
    ///
    /// # Errors
    ///
    /// `not_found`, or database errors.
    pub fn workstream(&self, id: &WorkstreamId) -> Result<Workstream> {
        self.read(|c| query::workstream(c, id))?
            .ok_or_else(|| WorkError::not_found(format!("No workstream {id}.")))
    }

    /// Tasks matching `filter`, in creation order.
    ///
    /// # Errors
    ///
    /// Database errors.
    pub fn tasks(&self, filter: &TaskFilter) -> Result<Vec<Task>> {
        self.read(|c| query::tasks(c, filter))
    }

    /// The same list as [`WorkService::tasks`], already as a JSON array (what `GET /v1/tasks`
    /// sends), built from the stored documents without decoding them.
    ///
    /// # Errors
    ///
    /// Database errors.
    pub fn tasks_json(&self, filter: &TaskFilter) -> Result<String> {
        self.read(|c| query::tasks_json(c, filter))
    }

    /// One task, by id or key; `not_found` if unknown.
    ///
    /// # Errors
    ///
    /// `not_found`, or database errors.
    pub fn task(&self, task: &TaskRef) -> Result<Task> {
        self.read(|c| query::task(c, task))?
            .ok_or_else(|| no_task(task))
    }

    /// Sessions matching `filter`.
    ///
    /// # Errors
    ///
    /// Database errors.
    pub fn sessions(&self, filter: &SessionFilter) -> Result<Vec<Session>> {
        self.read(|c| query::sessions(c, filter))
    }

    /// One session; `not_found` if unknown.
    ///
    /// # Errors
    ///
    /// `not_found`, or database errors.
    pub fn session(&self, id: &SessionId) -> Result<Session> {
        self.read(|c| query::session(c, id))?
            .ok_or_else(|| WorkError::not_found(format!("No session {id}.")))
    }

    /// Every dispatch.
    ///
    /// # Errors
    ///
    /// Database errors.
    pub fn dispatches(&self) -> Result<Vec<Dispatch>> {
        self.read(query::dispatches)
    }

    /// One dispatch; `not_found` if unknown.
    ///
    /// # Errors
    ///
    /// `not_found`, or database errors.
    pub fn dispatch(&self, id: &DispatchId) -> Result<Dispatch> {
        self.read(|c| query::dispatch(c, id))?
            .ok_or_else(|| WorkError::not_found(format!("No dispatch {id}.")))
    }

    /// Asks matching `filter`.
    ///
    /// # Errors
    ///
    /// Database errors.
    pub fn asks(&self, filter: &AskFilter) -> Result<Vec<Ask>> {
        self.read(|c| query::asks(c, filter))
    }

    /// One ask; `not_found` if unknown.
    ///
    /// # Errors
    ///
    /// `not_found`, or database errors.
    pub fn ask(&self, id: &AskId) -> Result<Ask> {
        self.read(|c| query::ask(c, id))?
            .ok_or_else(|| WorkError::not_found(format!("No ask {id}.")))
    }

    /// Every brief in force.
    ///
    /// # Errors
    ///
    /// Database errors.
    pub fn briefs(&self) -> Result<Vec<Brief>> {
        self.read(query::briefs)
    }

    /// The brief in force for `target`, if any.
    ///
    /// # Errors
    ///
    /// Database errors.
    pub fn brief(&self, target: &BriefTarget) -> Result<Option<Brief>> {
        self.read(|c| query::brief(c, target))
    }

    /// Reads a task back after a command changed it.
    pub(crate) fn reload_task(&self, id: TaskId) -> Result<Task> {
        self.read(|c| query::task(c, &TaskRef::Id(id)))?
            .ok_or_else(|| WorkError::internal(format!("task {id} is missing after a change")))
    }
}

pub(crate) fn no_task(task: &TaskRef) -> WorkError {
    WorkError::not_found(format!("No task {task}."))
}
