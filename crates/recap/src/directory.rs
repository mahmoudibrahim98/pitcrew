//! What the recap engine knows about the workspace: which session works on which task, which task
//! belongs to which workstream, and the names to use in prose. It is seeded from projections (the
//! event log is usually read from a recent point, not from the start) and kept current from
//! events.
//!
//! # The hub's rules
//!
//! Where the hub's projections (`crates/hub-work`) decide what is true, the directory follows
//! them, so that recaps say what the hub shows:
//!
//! - **Firm links stay** (`work.sessions`). A link made by a dispatch, a person or the agent itself
//!   (`dispatch`, `manual`, `claimed`) is replaced only by another firm link: not by an inferred
//!   one (`folder`, `branch`, `imported`), and not by a re-stated `session_discovered` that has
//!   none. Anything replaces an inferred link or none. A link is replaced whole (workstream, task
//!   and basis), and a dispatch with a session links it firmly to the dispatch's task.
//! - **Agents stay.** A re-stated session that names no agent keeps the agent it had.
//! - **Moves name where they start** (`work.tasks`). A `task_moved` whose `from` is not the task's
//!   status lost a race with another writer's move, and the hub ignores it. A task's status is
//!   known from its `task_created` and the moves counted since; a move counts when it starts there,
//!   or when it ends where the task already is because a `task_created` put it there (a log that
//!   states tasks as they are now and then replays older moves, as the hub's seed writes, says
//!   nothing false by it). Before any of that, nothing is known and every move counts: a status
//!   from a seed (`add_task`) is the projections' as they are now, which may be ahead of the
//!   events that follow.
//!
//! Events the hub ignores (a stale move, a link that would replace a firm one) are not activity:
//! the block builder leaves them out, like events it cannot place.
//!
//! The directory also learns members from `member_added`, so one directory can serve both the
//! block builder and the names in prose.
//!
//! # Bounded
//!
//! Ids come from untrusted events, so the directory keeps at most a limit of each kind of entry
//! (members, sessions, tasks, task-to-session links, workstreams, dispatches and asks), 100,000 by
//! default ([`Directory::with_limit`]). Past it, the entry of that kind used longest ago goes. An
//! entry is used by every `add_*` that states it and every event that states or names it (a
//! session by its own activity, a task by its moves and comments, a dispatch by its end, an ask
//! by its answer, a member by the events it authors). So a log of any length keeps being learned
//! from: what goes is what nothing has mentioned for longest, in practice sessions that ended long
//! ago, with their dispatches and asks. Use depends only on the order of events and `add_*` calls,
//! so the same events give the same directory however they are batched.
//!
//! What a dropped entry costs: later events about a dropped session, task or dispatch are placed
//! as if it were new (or not at all), and prose that needs a dropped name says "someone", "a task",
//! "a workstream" or "an ask". [`Directory::names_version`] moves on whenever a name may read
//! differently, so a cache of prose knows to write it again.
//!
//! Memory: with every kind full at the default limit (which only a flood of made-up ids reaches),
//! a directory measured about 125 MB, and 170 MB with every name at its 60-character cap in 4-byte
//! characters (release build, Linux x86-64).

use crate::lru::Lru;
use crate::text::{NAME_CHARS, clean};
use pitcrew_protocol::events::{BriefTarget, Event, EventBody};
use pitcrew_protocol::ids::{
    AskId, DispatchId, MemberId, ProjectId, SessionId, TaskId, WorkstreamId,
};
use pitcrew_protocol::model::{
    Ask, AskKind, Dispatch, LinkBasis, Member, Session, Task, TaskStatus, Workstream,
};

/// Most entries kept of each kind by default.
const MAX_ENTRIES: usize = 100_000;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct SessionInfo {
    pub(crate) agent: Option<MemberId>,
    pub(crate) workstream: Option<WorkstreamId>,
    pub(crate) task: Option<TaskId>,
    /// Why the session is linked; `None` when it is not, or nobody said.
    basis: Option<LinkBasis>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TaskInfo {
    pub(crate) key: String,
    pub(crate) project: ProjectId,
    pub(crate) workstream: Option<WorkstreamId>,
    /// The task's status as the hub has it, when the events tell.
    status: Option<TaskStatus>,
    /// Whether a `task_created` set `status`, rather than a move.
    stated: bool,
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

/// Whether a link was made by a dispatch, a person or the agent itself.
fn is_firm(basis: Option<LinkBasis>) -> bool {
    matches!(
        basis,
        Some(LinkBasis::Dispatch | LinkBasis::Manual | LinkBasis::Claimed)
    )
}

/// Whether a link with basis `incoming` may replace one with basis `existing`: anything replaces
/// an inferred link or none, and only a firm link replaces a firm one. The same rule as
/// `pitcrew_hub_work`'s `work.sessions`.
fn replaces_link(existing: Option<LinkBasis>, incoming: Option<LinkBasis>) -> bool {
    !is_firm(existing) || is_firm(incoming)
}

/// Kinds of names that have had an entry dropped for the limit.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Dropped {
    members: bool,
    tasks: bool,
    workstreams: bool,
    asks: bool,
}

/// Links and names the recap engine needs. Build one from the current projections with the
/// `add_*` methods; the block builder then keeps it current with [`Directory::observe`]. See the
/// [module docs](self) for the hub's rules it follows and its bound.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Directory {
    members: Lru<MemberId, String>,
    sessions: Lru<SessionId, SessionInfo>,
    tasks: Lru<TaskId, TaskInfo>,
    task_sessions: Lru<TaskId, SessionId>,
    workstreams: Lru<WorkstreamId, WorkstreamInfo>,
    dispatches: Lru<DispatchId, DispatchInfo>,
    asks: Lru<AskId, AskInfo>,
    names_version: u64,
    dropped: Dropped,
}

impl Default for Directory {
    fn default() -> Self {
        Self::with_limit(MAX_ENTRIES)
    }
}

impl Directory {
    /// An empty directory that keeps up to 100,000 entries of each kind.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// An empty directory that keeps at most `entries` of each kind (members, sessions, tasks,
    /// task-to-session links, workstreams, dispatches and asks; at least one). Past that, the
    /// entry used longest ago goes: see the [module docs](self).
    #[must_use]
    pub fn with_limit(entries: usize) -> Self {
        Self {
            members: Lru::new(entries),
            sessions: Lru::new(entries),
            tasks: Lru::new(entries),
            task_sessions: Lru::new(entries),
            workstreams: Lru::new(entries),
            dispatches: Lru::new(entries),
            asks: Lru::new(entries),
            names_version: 0,
            dropped: Dropped::default(),
        }
    }

    /// Adds or replaces a member, for its handle.
    pub fn add_member(&mut self, member: &Member) {
        let put = self
            .members
            .insert(member.id, clean(&member.handle, NAME_CHARS));
        let renamed = put
            .old
            .as_ref()
            .map(|old| self.members.get(&member.id) != Some(old));
        let dropped = &mut self.dropped.members;
        self.names_version = names_moved(self.names_version, renamed, put.evicted, dropped);
    }

    /// Adds or replaces a workstream, for its name and project.
    pub fn add_workstream(&mut self, workstream: &Workstream) {
        let name = clean(&workstream.name, NAME_CHARS);
        let put = self.workstreams.insert(
            workstream.id,
            WorkstreamInfo {
                name,
                project: workstream.project,
            },
        );
        let renamed = put.old.as_ref().map(|old| {
            self.workstreams
                .get(&workstream.id)
                .is_none_or(|now| now.name != old.name)
        });
        let dropped = &mut self.dropped.workstreams;
        self.names_version = names_moved(self.names_version, renamed, put.evicted, dropped);
    }

    /// Adds or replaces a task, for its key, project and workstream. Its status is not taken: a
    /// seed may be ahead of the events that follow it (see the [module docs](self)).
    pub fn add_task(&mut self, task: &Task) {
        self.put_task(task, None);
    }

    /// Adds or replaces a session, for its agent and links, by the hub's rules: a firm link stays
    /// unless the session comes with another firm one, and an agent stays unless it names one.
    pub fn add_session(&mut self, session: &Session) {
        let (info, _) = self.sessions.entry(session.id, SessionInfo::default);
        if session.agent.is_some() {
            info.agent = session.agent;
        }
        if replaces_link(info.basis, session.link_basis) {
            info.workstream = session.workstream;
            info.task = session.task;
            info.basis = session.link_basis;
            if let Some(task) = session.task {
                self.task_sessions.insert(task, session.id);
            }
        }
    }

    /// Adds a dispatch. A dispatch with a session also links that session firmly to its task (in
    /// the task's workstream, unless the session was already linked to that task in one), and
    /// gives it the dispatch's agent if it had none.
    pub fn add_dispatch(&mut self, dispatch: &Dispatch) {
        self.dispatches.insert(
            dispatch.id,
            DispatchInfo {
                task: dispatch.task,
                session: dispatch.session,
            },
        );
        if let Some(session) = dispatch.session {
            let task_workstream = self.tasks.get(&dispatch.task).and_then(|t| t.workstream);
            let (info, _) = self.sessions.entry(session, SessionInfo::default);
            if info.task != Some(dispatch.task) || info.workstream.is_none() {
                info.workstream = task_workstream;
            }
            info.task = Some(dispatch.task);
            info.agent = info.agent.or(Some(dispatch.agent));
            info.basis = Some(LinkBasis::Dispatch);
            self.task_sessions.insert(dispatch.task, session);
        }
    }

    /// Adds an ask, so its answer can be placed and described.
    pub fn add_ask(&mut self, ask: &Ask) {
        let put = self.asks.insert(
            ask.id,
            AskInfo {
                kind: ask.kind,
                from: ask.from,
                task: ask.task,
                session: ask.session,
            },
        );
        let changed = put
            .old
            .as_ref()
            .map(|old| (old.kind, old.from) != (ask.kind, ask.from));
        let dropped = &mut self.dropped.asks;
        self.names_version = names_moved(self.names_version, changed, put.evicted, dropped);
    }

    /// Learns from one event: new members, sessions, tasks, workstreams, dispatches and asks,
    /// session links, tasks moved to another workstream or status, and which entries were used.
    pub fn observe(&mut self, event: &Event) {
        self.learn(event);
    }

    /// [`Directory::observe`], and whether the event is activity: `false` for one the hub ignores
    /// (a stale move, or a link that would replace a firm one).
    pub(crate) fn learn(&mut self, event: &Event) -> bool {
        self.members.touch(event.author);
        match &event.body {
            EventBody::MemberAdded { member } => self.add_member(member),
            EventBody::SessionDiscovered { session } => self.add_session(session),
            EventBody::SessionLinked {
                session,
                workstream,
                task,
                basis,
            } => return self.link(*session, *workstream, *task, *basis),
            EventBody::SessionStateChanged { session, .. }
            | EventBody::TurnEnded { session, .. }
            | EventBody::ToolRan { session, .. }
            | EventBody::FileEdited { session, .. }
            | EventBody::SessionUpdated { session, .. }
            | EventBody::SessionEnded { session } => self.sessions.touch(*session),
            EventBody::TaskCreated { task } => self.put_task(task, Some(task.status)),
            EventBody::TaskUpdated { task, patch } => {
                self.tasks.touch(*task);
                if let (Some(workstream), Some(info)) = (patch.workstream, self.tasks.get_mut(task))
                {
                    info.workstream = workstream;
                }
            }
            EventBody::TaskMoved { task, from, to, .. } => {
                return self.take_move(*task, *from, *to);
            }
            EventBody::TaskAssigned { task, .. } | EventBody::SubtasksReplaced { task, .. } => {
                self.tasks.touch(*task);
            }
            EventBody::CommentPosted {
                task, workstream, ..
            } => {
                if let Some(task) = task {
                    self.tasks.touch(*task);
                }
                if let Some(workstream) = workstream {
                    self.workstreams.touch(*workstream);
                }
            }
            EventBody::WorkstreamCreated { workstream } => self.add_workstream(workstream),
            EventBody::WorkstreamChanged { workstream, .. }
            | EventBody::DecisionRecorded {
                workstream: Some(workstream),
                ..
            }
            | EventBody::BriefProposed {
                target: BriefTarget::Workstream(workstream),
                ..
            }
            | EventBody::BriefAccepted {
                target: BriefTarget::Workstream(workstream),
                ..
            } => self.workstreams.touch(*workstream),
            EventBody::DispatchStarted { dispatch } => self.add_dispatch(dispatch),
            EventBody::DispatchFinished { dispatch, .. } => self.dispatches.touch(*dispatch),
            EventBody::AskRaised { ask } => self.add_ask(ask),
            EventBody::AskAnswered { ask, .. } => self.asks.touch(*ask),
            _ => {}
        }
        true
    }

    /// A `session_linked`: whether it replaced the session's link.
    fn link(
        &mut self,
        session: SessionId,
        workstream: Option<WorkstreamId>,
        task: Option<TaskId>,
        basis: LinkBasis,
    ) -> bool {
        let (info, _) = self.sessions.entry(session, SessionInfo::default);
        if !replaces_link(info.basis, Some(basis)) {
            return false;
        }
        info.workstream = workstream;
        info.task = task;
        info.basis = Some(basis);
        if let Some(task) = task {
            self.task_sessions.insert(task, session);
        }
        true
    }

    /// A `task_moved`: whether it counts (see the [module docs](self)).
    fn take_move(&mut self, task: TaskId, from: TaskStatus, to: TaskStatus) -> bool {
        self.tasks.touch(task);
        let Some(info) = self.tasks.get_mut(&task) else {
            return true;
        };
        let counts = match info.status {
            None => true,
            Some(now) => now == from || (now == to && info.stated),
        };
        if counts {
            info.status = Some(to);
            info.stated = false;
        }
        counts
    }

    fn put_task(&mut self, task: &Task, status: Option<TaskStatus>) {
        let put = self.tasks.insert(
            task.id,
            TaskInfo {
                key: clean(&task.key.to_string(), NAME_CHARS),
                project: task.project,
                workstream: task.workstream,
                status,
                stated: status.is_some(),
            },
        );
        let renamed = put.old.as_ref().map(|old| {
            self.tasks
                .get(&task.id)
                .is_none_or(|now| now.key != old.key)
        });
        let dropped = &mut self.dropped.tasks;
        self.names_version = names_moved(self.names_version, renamed, put.evicted, dropped);
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

    /// An ask's kind and who raised it: what prose says about an answer to it ("answered a
    /// question from @writer").
    #[must_use]
    pub fn ask(&self, id: AskId) -> Option<(AskKind, MemberId)> {
        self.asks.get(&id).map(|a| (a.kind, a.from))
    }

    /// A number that moves on whenever prose written with this directory may now read
    /// differently: a member's handle, a task's key or a workstream's name re-stated differently,
    /// an ask raised again as another kind or by someone else, or any of them dropped for the
    /// limit; and, once one of a kind has been dropped, any name of that kind learned (it may be
    /// one dropped before). Learning a name that was never known does not move it: whether prose
    /// named that id while it was unknown is the caller's to track.
    #[must_use]
    pub fn names_version(&self) -> u64 {
        self.names_version
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

    pub(crate) fn ask_info(&self, id: AskId) -> Option<&AskInfo> {
        self.asks.get(&id)
    }
}

/// The names version after a write of a name: `changed` is `None` for a new entry, else whether
/// its name changed; `evicted` is what went to make room.
fn names_moved<K, V>(
    version: u64,
    changed: Option<bool>,
    evicted: Option<(K, V)>,
    dropped: &mut bool,
) -> u64 {
    let learned_after_drop = changed.is_none() && *dropped;
    if evicted.is_some() {
        *dropped = true;
    }
    if changed == Some(true) || learned_after_drop || evicted.is_some() {
        version.wrapping_add(1)
    } else {
        version
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pitcrew_protocol::ids::MachineId;
    use pitcrew_protocol::model::{Engine, SessionState};
    use ulid::Ulid;

    fn session(n: usize, task: TaskId) -> Session {
        Session {
            id: SessionId(Ulid::from(n as u128)),
            engine: Engine::Claude,
            native_id: String::new(),
            machine: MachineId(Ulid::nil()),
            cwd: String::new(),
            branch: None,
            title: None,
            agent: None,
            workstream: None,
            task: Some(task),
            link_basis: None,
            state: SessionState::Working,
            status_line: None,
            started: 0,
            last_activity: 0,
            terminal: None,
            parent: None,
        }
    }

    /// The first-come cap kept the first 100,000 sessions and ignored the rest for good.
    #[test]
    fn at_the_default_limit_the_oldest_goes_not_the_newest() {
        let mut dir = Directory::new();
        let task = TaskId(Ulid::from(1u128));
        for n in 0..=MAX_ENTRIES {
            dir.add_session(&session(n, task));
        }
        assert_eq!(dir.sessions.len(), MAX_ENTRIES);
        assert!(dir.session(SessionId(Ulid::from(0u128))).is_none());
        let newest = SessionId(Ulid::from(MAX_ENTRIES as u128));
        assert_eq!(dir.session(newest).and_then(|s| s.task), Some(task));
        assert_eq!(dir.task_session(task), Some(newest));
        assert_eq!(dir.names_version(), 0, "no name was dropped");
    }

    #[test]
    fn firm_links_are_replaced_only_by_firm_links() {
        use LinkBasis::{Branch, Claimed, Dispatch, Folder, Imported, Manual};
        let firm = [Dispatch, Manual, Claimed];
        let inferred = [Folder, Branch, Imported];
        for existing in firm {
            for incoming in firm {
                assert!(replaces_link(Some(existing), Some(incoming)));
            }
            for incoming in inferred {
                assert!(!replaces_link(Some(existing), Some(incoming)));
            }
            assert!(!replaces_link(Some(existing), None));
        }
        for existing in inferred.map(Some).into_iter().chain([None]) {
            for incoming in firm.into_iter().chain(inferred).map(Some).chain([None]) {
                assert!(replaces_link(existing, incoming));
            }
        }
    }
}
