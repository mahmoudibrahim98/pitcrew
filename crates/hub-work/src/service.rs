//! [`WorkService`]: the work model's reads and commands over one store.

use crate::error::{Result, WorkError};
use crate::query::{self, AskFilter, SessionFilter, TaskFilter, TaskRef};
use pitcrew_protocol::api::Caller;
use pitcrew_protocol::events::{BriefTarget, Event, EventBody};
use pitcrew_protocol::ids::{
    AskId, DispatchId, MemberId, ProjectId, SessionId, TaskId, WorkspaceId, WorkstreamId,
};
use pitcrew_protocol::model::{
    Ask, Brief, Dispatch, Machine, Member, Persona, Project, Session, Task, Team, TimestampMs,
    Workstream,
};
use pitcrew_store::sql::Connection;
use pitcrew_store::{RevRange, Store};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

/// Where commands get the time from. The default is the system clock.
pub type Clock = Arc<dyn Fn() -> TimestampMs + Send + Sync>;

fn system_clock() -> TimestampMs {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// The work model of one workspace: reads of its projections, and commands that validate a
/// change against them and append the events that make it.
///
/// The store must have been opened with [`crate::projections`]. Commands run one at a time (a
/// lock covers validating and appending), so two commands cannot both pass a check that only one
/// of them should (such as allocating the same task key). Reads never wait for commands.
///
/// Every event a command appends is stamped from the [`Caller`]: `author` is the caller's member
/// and `on_behalf_of` its owner, never anything from a request body.
pub struct WorkService {
    store: Arc<Store>,
    workspace: WorkspaceId,
    clock: Clock,
    writes: Mutex<()>,
}

impl std::fmt::Debug for WorkService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkService")
            .field("workspace", &self.workspace)
            .field("store", &self.store)
            .finish_non_exhaustive()
    }
}

impl WorkService {
    /// A service over `store` (opened with [`crate::projections`]) for `workspace`.
    #[must_use]
    pub fn new(store: Arc<Store>, workspace: WorkspaceId) -> Self {
        Self {
            store,
            workspace,
            clock: Arc::new(system_clock),
            writes: Mutex::new(()),
        }
    }

    /// Uses `clock` for the times commands record (`created`, `at` of answers and events).
    #[must_use]
    pub fn with_clock(mut self, clock: Clock) -> Self {
        self.clock = clock;
        self
    }

    /// The store.
    #[must_use]
    pub fn store(&self) -> &Arc<Store> {
        &self.store
    }

    /// The workspace.
    #[must_use]
    pub fn workspace(&self) -> WorkspaceId {
        self.workspace
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
            workspace: self.workspace,
            author,
            on_behalf_of,
            body,
        }
    }

    /// An event by `caller`: `author` is its member, `on_behalf_of` its owner.
    pub(crate) fn by(&self, caller: &Caller, body: EventBody) -> Event {
        self.event(caller.member, caller.on_behalf_of, body)
    }

    pub(crate) fn append(&self, events: &[Event]) -> Result<RevRange> {
        Ok(self.store.append(events)?)
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
