//! What the recap engine knows about the workspace: which session works on which task, which task
//! belongs to which workstream, and the names to use in prose. It is seeded from projections (the
//! event log is usually read from a recent point, not from the start) and kept current from
//! events.

use crate::text::{NAME_CHARS, clean};
use pitcrew_protocol::events::{Event, EventBody};
use pitcrew_protocol::ids::{
    AskId, DispatchId, MemberId, ProjectId, SessionId, TaskId, WorkstreamId,
};
use pitcrew_protocol::model::{Ask, AskKind, Dispatch, Member, Session, Task, Workstream};
use std::collections::HashMap;
use std::hash::Hash;

/// Most entries kept per kind. Ids come from untrusted events; past this, new ones are ignored.
const MAX_ENTRIES: usize = 100_000;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct SessionInfo {
    pub(crate) agent: Option<MemberId>,
    pub(crate) workstream: Option<WorkstreamId>,
    pub(crate) task: Option<TaskId>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TaskInfo {
    pub(crate) key: String,
    pub(crate) project: ProjectId,
    pub(crate) workstream: Option<WorkstreamId>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WorkstreamInfo {
    pub(crate) name: String,
    pub(crate) project: ProjectId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DispatchInfo {
    pub(crate) task: TaskId,
    pub(crate) session: Option<SessionId>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AskInfo {
    pub(crate) kind: AskKind,
    pub(crate) from: MemberId,
    pub(crate) task: Option<TaskId>,
    pub(crate) session: Option<SessionId>,
}

/// Links and names the recap engine needs. Build one from the current projections with the
/// `add_*` methods; the block builder then keeps it current with [`Directory::observe`].
#[derive(Clone, Debug, Default)]
pub struct Directory {
    members: HashMap<MemberId, String>,
    sessions: HashMap<SessionId, SessionInfo>,
    tasks: HashMap<TaskId, TaskInfo>,
    task_sessions: HashMap<TaskId, SessionId>,
    workstreams: HashMap<WorkstreamId, WorkstreamInfo>,
    dispatches: HashMap<DispatchId, DispatchInfo>,
    asks: HashMap<AskId, AskInfo>,
}

fn put<K: Hash + Eq, V>(map: &mut HashMap<K, V>, key: K, value: V) {
    if map.len() < MAX_ENTRIES || map.contains_key(&key) {
        map.insert(key, value);
    }
}

fn session_entry(
    map: &mut HashMap<SessionId, SessionInfo>,
    id: SessionId,
) -> Option<&mut SessionInfo> {
    if map.len() < MAX_ENTRIES || map.contains_key(&id) {
        Some(map.entry(id).or_default())
    } else {
        None
    }
}

impl Directory {
    /// An empty directory.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds or replaces a member, for its handle.
    pub fn add_member(&mut self, member: &Member) {
        put(
            &mut self.members,
            member.id,
            clean(&member.handle, NAME_CHARS),
        );
    }

    /// Adds or replaces a workstream, for its name and project.
    pub fn add_workstream(&mut self, workstream: &Workstream) {
        put(
            &mut self.workstreams,
            workstream.id,
            WorkstreamInfo {
                name: clean(&workstream.name, NAME_CHARS),
                project: workstream.project,
            },
        );
    }

    /// Adds or replaces a task, for its key, project and workstream.
    pub fn add_task(&mut self, task: &Task) {
        put(
            &mut self.tasks,
            task.id,
            TaskInfo {
                key: clean(&task.key.to_string(), NAME_CHARS),
                project: task.project,
                workstream: task.workstream,
            },
        );
    }

    /// Adds or replaces a session, for its agent and links.
    pub fn add_session(&mut self, session: &Session) {
        put(
            &mut self.sessions,
            session.id,
            SessionInfo {
                agent: session.agent,
                workstream: session.workstream,
                task: session.task,
            },
        );
        if let Some(task) = session.task {
            put(&mut self.task_sessions, task, session.id);
        }
    }

    /// Adds a dispatch. A dispatch with a session also links that session to its task and agent.
    pub fn add_dispatch(&mut self, dispatch: &Dispatch) {
        put(
            &mut self.dispatches,
            dispatch.id,
            DispatchInfo {
                task: dispatch.task,
                session: dispatch.session,
            },
        );
        if let Some(session) = dispatch.session {
            if let Some(info) = session_entry(&mut self.sessions, session) {
                info.task = Some(dispatch.task);
                info.agent = info.agent.or(Some(dispatch.agent));
                if info.workstream.is_none() {
                    info.workstream = self.tasks.get(&dispatch.task).and_then(|t| t.workstream);
                }
            }
            put(&mut self.task_sessions, dispatch.task, session);
        }
    }

    /// Adds an ask, so its answer can be placed and described.
    pub fn add_ask(&mut self, ask: &Ask) {
        put(
            &mut self.asks,
            ask.id,
            AskInfo {
                kind: ask.kind,
                from: ask.from,
                task: ask.task,
                session: ask.session,
            },
        );
    }

    /// Learns from one event: new sessions, tasks, workstreams, dispatches and asks, and session
    /// links.
    pub fn observe(&mut self, event: &Event) {
        match &event.body {
            EventBody::SessionDiscovered { session } => self.add_session(session),
            EventBody::SessionLinked {
                session,
                workstream,
                task,
                ..
            } => {
                if let Some(info) = session_entry(&mut self.sessions, *session) {
                    if workstream.is_some() {
                        info.workstream = *workstream;
                    }
                    if task.is_some() {
                        info.task = *task;
                    }
                }
                if let Some(task) = task {
                    put(&mut self.task_sessions, *task, *session);
                }
            }
            EventBody::TaskCreated { task } => self.add_task(task),
            EventBody::WorkstreamCreated { workstream } => self.add_workstream(workstream),
            EventBody::DispatchStarted { dispatch } => self.add_dispatch(dispatch),
            EventBody::AskRaised { ask } => self.add_ask(ask),
            _ => {}
        }
    }

    /// A member's handle, e.g. `@writer`.
    #[must_use]
    pub fn handle(&self, id: MemberId) -> Option<&str> {
        self.members.get(&id).map(String::as_str)
    }

    /// A task's key, e.g. `PAP-1`.
    #[must_use]
    pub fn task_key(&self, id: TaskId) -> Option<&str> {
        self.tasks.get(&id).map(|t| t.key.as_str())
    }

    /// A workstream's name.
    #[must_use]
    pub fn workstream_name(&self, id: WorkstreamId) -> Option<&str> {
        self.workstreams.get(&id).map(|w| w.name.as_str())
    }

    pub(crate) fn session(&self, id: SessionId) -> Option<&SessionInfo> {
        self.sessions.get(&id)
    }

    pub(crate) fn task(&self, id: TaskId) -> Option<&TaskInfo> {
        self.tasks.get(&id)
    }

    pub(crate) fn task_session(&self, id: TaskId) -> Option<SessionId> {
        self.task_sessions.get(&id).copied()
    }

    pub(crate) fn workstream_project(&self, id: WorkstreamId) -> Option<ProjectId> {
        self.workstreams.get(&id).map(|w| w.project)
    }

    pub(crate) fn dispatch(&self, id: DispatchId) -> Option<&DispatchInfo> {
        self.dispatches.get(&id)
    }

    pub(crate) fn ask(&self, id: AskId) -> Option<&AskInfo> {
        self.asks.get(&id)
    }
}
