//! What the office knows about the workspace: members, tasks, dispatches, sessions, open asks and
//! workstreams. It is built from events only, never seeded from other tables, so replaying the log
//! from the start rebuilds it exactly, and the run log rebuilds identically.
//!
//! Two facts only ever tighten, whatever later events claim: a member once known as a person
//! stays a person, and a task's automatic acceptance, once off, stays off. A crafted event cannot
//! turn a person's ask into an agent's, or a task into one the office may mark done.

use crate::state::{StateRow, Tracked};
use pitcrew_protocol::events::{Event, EventBody};
use pitcrew_protocol::ids::{
    AskId, DispatchId, EventId, MemberId, ProjectId, SessionId, TaskId, WorkstreamId,
};
use pitcrew_protocol::model::{
    AskKind, AskState, BriefTarget, MemberKind, Receipt, TaskStatus, TimestampMs, WorkstreamStatus,
};
use pitcrew_recap::clean;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::ops::Bound;

/// Longest ask title kept, in characters.
pub(crate) const TITLE_CHARS: usize = 80;
/// Most receipts kept per ask.
const ASK_RECEIPTS: usize = 8;
/// Longest text field a kept receipt may have, in bytes.
const RECEIPT_TEXT_BYTES: usize = 1024;

/// A member: whether it is a person, and an agent's owner.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberInfo {
    /// A person. Once true, it stays true.
    pub human: bool,
    /// An agent's owner.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<MemberId>,
}

/// A task as the office sees it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskInfo {
    /// The project.
    pub project: ProjectId,
    /// The workstream, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workstream: Option<WorkstreamId>,
    /// Its latest status.
    pub status: TaskStatus,
    /// Whether review can complete without a person. Once false, it stays false.
    pub accept_auto: bool,
}

/// A dispatch: which task, which agent, which session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DispatchInfo {
    /// The task.
    pub task: TaskId,
    /// The agent.
    pub agent: MemberId,
    /// The session running it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionId>,
}

/// A session's agent and links.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionInfo {
    /// The agent running it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<MemberId>,
    /// The linked workstream.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workstream: Option<WorkstreamId>,
    /// The linked task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<TaskId>,
}

/// An open ask. Answered asks are forgotten.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AskInfo {
    /// Its kind.
    pub kind: AskKind,
    /// Who asked.
    pub from: MemberId,
    /// Who must answer.
    pub to: MemberId,
    /// The related task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<TaskId>,
    /// The related session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionId>,
    /// The title, cleaned.
    pub title: String,
    /// Its receipts (capped).
    #[serde(default)]
    pub receipts: Vec<Receipt>,
    /// The event that raised it.
    pub event: EventId,
    /// When the office saw it raised (its clock: the latest event time so far).
    pub at: TimestampMs,
    /// The revision that raised it.
    pub rev: u64,
}

/// A workstream's status and its latest activity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkstreamInfo {
    /// The project.
    pub project: ProjectId,
    /// Its status.
    pub status: WorkstreamStatus,
    /// When the office last saw activity on it (its clock).
    pub last_at: TimestampMs,
    /// The revision of that activity.
    pub last_rev: u64,
    /// The event of that activity, the receipt for "no activity since".
    pub last_event: EventId,
}

/// The position of an open ask in time order: `(at, rev, ask)`. Positions only grow, because the
/// office's clock never goes back and revisions increase, so a rule can keep a cursor.
pub type AskPos = (TimestampMs, u64, AskId);
/// The position of a workstream's latest activity: `(last_at, last_rev, workstream)`.
pub type QuietPos = (TimestampMs, u64, WorkstreamId);

/// What the office knows. See the [module docs](self).
#[derive(Clone, Debug, Default)]
pub struct World {
    members: Tracked<MemberId, MemberInfo>,
    tasks: Tracked<TaskId, TaskInfo>,
    dispatches: Tracked<DispatchId, DispatchInfo>,
    sessions: Tracked<SessionId, SessionInfo>,
    asks: Tracked<AskId, AskInfo>,
    workstreams: Tracked<WorkstreamId, WorkstreamInfo>,
    /// Open asks by position. Derived; rebuilt on load.
    asks_by_time: BTreeSet<AskPos>,
    /// Workstreams by latest activity. Derived; rebuilt on load.
    quiet_order: BTreeSet<QuietPos>,
}

fn receipt_ok(r: &Receipt) -> bool {
    let short = |s: &str| s.len() <= RECEIPT_TEXT_BYTES;
    match r {
        Receipt::Transcript { .. } | Receipt::Event { .. } => true,
        Receipt::Commit { repo, sha } => short(repo) && short(sha),
        Receipt::PullRequest { url } => short(url),
        Receipt::Job { id, .. } => short(id),
        Receipt::File { location } => {
            short(&location.path) && location.branch.as_deref().is_none_or(short)
        }
    }
}

/// Receipts from an untrusted event, without repeats or oversized ones, at most `cap`.
pub(crate) fn small_receipts<'a>(
    receipts: impl IntoIterator<Item = &'a Receipt>,
    cap: usize,
) -> Vec<Receipt> {
    let mut out: Vec<Receipt> = Vec::new();
    for r in receipts.into_iter().take(cap.saturating_mul(4)) {
        if out.len() >= cap {
            break;
        }
        if receipt_ok(r) && !out.contains(r) {
            out.push(r.clone());
        }
    }
    out
}

impl World {
    /// A member, if known.
    #[must_use]
    pub fn member(&self, id: MemberId) -> Option<&MemberInfo> {
        self.members.get(&id)
    }

    /// A task, if known.
    #[must_use]
    pub fn task(&self, id: TaskId) -> Option<&TaskInfo> {
        self.tasks.get(&id)
    }

    /// A dispatch, if known.
    #[must_use]
    pub fn dispatch(&self, id: DispatchId) -> Option<&DispatchInfo> {
        self.dispatches.get(&id)
    }

    /// A session, if known.
    #[must_use]
    pub fn session(&self, id: SessionId) -> Option<&SessionInfo> {
        self.sessions.get(&id)
    }

    /// An open ask, if known.
    #[must_use]
    pub fn ask(&self, id: AskId) -> Option<&AskInfo> {
        self.asks.get(&id)
    }

    /// A workstream, if known.
    #[must_use]
    pub fn workstream(&self, id: WorkstreamId) -> Option<&WorkstreamInfo> {
        self.workstreams.get(&id)
    }

    /// Whether the member is known to be a person.
    #[must_use]
    pub fn is_human(&self, id: MemberId) -> bool {
        self.members.get(&id).is_some_and(|m| m.human)
    }

    /// Whether the member is known to be an agent.
    #[must_use]
    pub fn is_agent(&self, id: MemberId) -> bool {
        self.members.get(&id).is_some_and(|m| !m.human)
    }

    /// The person answerable for `member`'s work: the member itself if it is a person, else its
    /// owner, else the person its event was on behalf of. Only a known person is returned.
    #[must_use]
    pub fn owner_of(&self, member: MemberId, on_behalf_of: Option<MemberId>) -> Option<MemberId> {
        let candidate = match self.members.get(&member) {
            Some(m) if m.human => Some(member),
            Some(m) => m.owner.or(on_behalf_of),
            None => on_behalf_of,
        };
        candidate.filter(|p| self.is_human(*p))
    }

    /// Open asks raised after `after` and at or before `until` (the office's clock), oldest first.
    pub fn asks_due(
        &self,
        after: Option<AskPos>,
        until: TimestampMs,
    ) -> impl Iterator<Item = (AskPos, &AskInfo)> {
        let start = after.map_or(Bound::Unbounded, Bound::Excluded);
        self.asks_by_time
            .range((start, Bound::Unbounded))
            .take_while(move |(at, _, _)| *at <= until)
            .filter_map(|pos| self.asks.get(&pos.2).map(|a| (*pos, a)))
    }

    /// Workstreams whose latest activity is after `after` and at or before `until`, quietest
    /// first.
    pub fn workstreams_due(
        &self,
        after: Option<QuietPos>,
        until: TimestampMs,
    ) -> impl Iterator<Item = (QuietPos, &WorkstreamInfo)> {
        let start = after.map_or(Bound::Unbounded, Bound::Excluded);
        self.quiet_order
            .range((start, Bound::Unbounded))
            .take_while(move |(at, _, _)| *at <= until)
            .filter_map(|pos| self.workstreams.get(&pos.2).map(|w| (*pos, w)))
    }

    /// The workstream an event is activity on, if any.
    #[must_use]
    pub fn workstream_of(&self, event: &Event) -> Option<WorkstreamId> {
        let of_task = |t: TaskId| self.tasks.get(&t).and_then(|t| t.workstream);
        let of_session = |s: SessionId| {
            self.sessions
                .get(&s)
                .and_then(|i| i.workstream.or_else(|| i.task.and_then(of_task)))
        };
        let of_dispatch = |d: DispatchId| self.dispatches.get(&d).and_then(|d| of_task(d.task));
        match &event.body {
            EventBody::ToolRan { session, .. }
            | EventBody::FileEdited { session, .. }
            | EventBody::TurnEnded { session, .. }
            | EventBody::SessionStateChanged { session, .. }
            | EventBody::SessionUpdated { session, .. }
            | EventBody::SessionEnded { session }
            | EventBody::SessionLinked { session, .. } => of_session(*session),
            EventBody::SessionDiscovered { session } => session
                .workstream
                .or_else(|| session.task.and_then(of_task)),
            EventBody::TaskCreated { task } => task.workstream,
            EventBody::TaskMoved { task, .. }
            | EventBody::TaskAssigned { task, .. }
            | EventBody::TaskUpdated { task, .. }
            | EventBody::SubtasksReplaced { task, .. } => of_task(*task),
            EventBody::DispatchStarted { dispatch } => of_task(dispatch.task),
            EventBody::DispatchFinished { dispatch, .. } => of_dispatch(*dispatch),
            EventBody::AskRaised { ask } => ask
                .task
                .and_then(of_task)
                .or_else(|| ask.session.and_then(of_session)),
            EventBody::AskAnswered { ask, .. } => self.asks.get(ask).and_then(|a| {
                a.task
                    .and_then(of_task)
                    .or_else(|| a.session.and_then(of_session))
            }),
            EventBody::CommentPosted {
                task, workstream, ..
            } => task.and_then(of_task).or(*workstream),
            EventBody::WorkstreamCreated { workstream } => Some(workstream.id),
            EventBody::WorkstreamChanged { workstream, .. } => Some(*workstream),
            EventBody::BriefAccepted {
                target: BriefTarget::Workstream(w),
                ..
            } => Some(*w),
            EventBody::DecisionRecorded { workstream, .. } => *workstream,
            _ => None,
        }
    }

    /// Learns from one event. `now` is the office's clock (the latest event time so far), and
    /// events by `office` are not activity.
    pub(crate) fn observe(
        &mut self,
        rev: u64,
        now: TimestampMs,
        event: &Event,
        office: Option<MemberId>,
    ) {
        // Activity is judged both before the event changes the links (an answered ask is gone
        // after) and after (a session just linked to a workstream).
        let by_office = office == Some(event.author);
        let before = (!by_office).then(|| self.workstream_of(event)).flatten();
        match &event.body {
            EventBody::MemberAdded { member } => {
                let human = member.kind == MemberKind::Human
                    || self.members.get(&member.id).is_some_and(|m| m.human);
                self.members.insert(
                    member.id,
                    MemberInfo {
                        human,
                        owner: member.owner,
                    },
                );
            }
            EventBody::TaskCreated { task } => {
                // A repeated TaskCreated is an upsert, except that it cannot change the status
                // (only TaskMoved does) or turn `accept_auto` back on (facts only tighten).
                let accept_auto =
                    task.accept_auto && self.tasks.get(&task.id).is_none_or(|t| t.accept_auto);
                let status = self.tasks.get(&task.id).map_or(task.status, |t| t.status);
                self.tasks.insert(
                    task.id,
                    TaskInfo {
                        project: task.project,
                        workstream: task.workstream,
                        status,
                        accept_auto,
                    },
                );
            }
            EventBody::TaskMoved { task, to, .. } => {
                self.tasks.update(task, |t| t.status = *to);
            }
            EventBody::TaskUpdated { task, patch } => {
                // A patch can move the task to another workstream, or turn automatic acceptance
                // off; like any event, it cannot turn it back on (facts only tighten).
                self.tasks.update(task, |t| {
                    if let Some(workstream) = patch.workstream {
                        t.workstream = workstream;
                    }
                    if patch.accept_auto == Some(false) {
                        t.accept_auto = false;
                    }
                });
            }
            EventBody::DispatchStarted { dispatch } => {
                self.dispatches.insert(
                    dispatch.id,
                    DispatchInfo {
                        task: dispatch.task,
                        agent: dispatch.agent,
                        session: dispatch.session,
                    },
                );
                if let Some(s) = dispatch.session {
                    let workstream = self.tasks.get(&dispatch.task).and_then(|t| t.workstream);
                    let mut info = self.sessions.get(&s).cloned().unwrap_or_default();
                    info.task = Some(dispatch.task);
                    info.agent = info.agent.or(Some(dispatch.agent));
                    info.workstream = info.workstream.or(workstream);
                    self.sessions.insert(s, info);
                }
            }
            EventBody::SessionDiscovered { session } => {
                self.sessions.insert(
                    session.id,
                    SessionInfo {
                        agent: session.agent,
                        workstream: session.workstream,
                        task: session.task,
                    },
                );
            }
            EventBody::SessionLinked {
                session,
                workstream,
                task,
                ..
            } => {
                let mut info = self.sessions.get(session).cloned().unwrap_or_default();
                info.workstream = workstream.or(info.workstream);
                info.task = task.or(info.task);
                self.sessions.insert(*session, info);
            }
            EventBody::AskRaised { ask } if ask.state == AskState::Open => {
                if let Some(old) = self.asks.get(&ask.id) {
                    self.asks_by_time.remove(&(old.at, old.rev, ask.id));
                }
                let stored = self.asks.insert(
                    ask.id,
                    AskInfo {
                        kind: ask.kind,
                        from: ask.from,
                        to: ask.to,
                        task: ask.task,
                        session: ask.session,
                        title: clean(&ask.title, TITLE_CHARS),
                        receipts: small_receipts(&ask.receipts, ASK_RECEIPTS),
                        event: event.id,
                        at: now,
                        rev,
                    },
                );
                if stored {
                    self.asks_by_time.insert((now, rev, ask.id));
                }
            }
            EventBody::AskAnswered { ask, .. } => {
                if let Some(old) = self.asks.remove(ask) {
                    self.asks_by_time.remove(&(old.at, old.rev, *ask));
                }
            }
            EventBody::WorkstreamCreated { workstream } => {
                if let Some(old) = self.workstreams.get(&workstream.id) {
                    self.quiet_order
                        .remove(&(old.last_at, old.last_rev, workstream.id));
                }
                let stored = self.workstreams.insert(
                    workstream.id,
                    WorkstreamInfo {
                        project: workstream.project,
                        status: workstream.status,
                        last_at: now,
                        last_rev: rev,
                        last_event: event.id,
                    },
                );
                if stored {
                    self.quiet_order.insert((now, rev, workstream.id));
                }
            }
            EventBody::WorkstreamChanged {
                workstream, status, ..
            } => {
                self.workstreams.update(workstream, |w| w.status = *status);
            }
            _ => {}
        }
        let after = (!by_office).then(|| self.workstream_of(event)).flatten();
        for w in [before, after].into_iter().flatten() {
            self.touch(w, rev, now, event.id);
        }
    }

    /// Records activity on a workstream.
    fn touch(&mut self, w: WorkstreamId, rev: u64, now: TimestampMs, event: EventId) {
        let Some(old) = self.workstreams.get(&w) else {
            return;
        };
        if old.last_rev == rev {
            return;
        }
        let old_pos = (old.last_at, old.last_rev, w);
        self.workstreams.update(&w, |info| {
            info.last_at = now;
            info.last_rev = rev;
            info.last_event = event;
        });
        self.quiet_order.remove(&old_pos);
        self.quiet_order.insert((now, rev, w));
    }

    /// Saves every changed entry.
    pub(crate) fn save(&mut self, out: &mut Vec<StateRow>) -> serde_json::Result<()> {
        self.members.save("member", out)?;
        self.tasks.save("task", out)?;
        self.dispatches.save("dispatch", out)?;
        self.sessions.save("session", out)?;
        self.asks.save("ask", out)?;
        self.workstreams.save("workstream", out)
    }

    /// Loads one saved row. Returns `Ok(false)` when the kind is not the world's.
    pub(crate) fn load(&mut self, kind: &str, key: &str, value: &str) -> Result<bool, LoadError> {
        fn parse<T: serde::de::DeserializeOwned>(value: &str) -> Result<T, LoadError> {
            serde_json::from_str(value).map_err(LoadError::Value)
        }
        fn id<K: crate::state::RowKey>(key: &str) -> Result<K, LoadError> {
            K::from_row(key).ok_or(LoadError::Key)
        }
        match kind {
            "member" => self.members.load(id(key)?, parse(value)?),
            "task" => self.tasks.load(id(key)?, parse(value)?),
            "dispatch" => self.dispatches.load(id(key)?, parse(value)?),
            "session" => self.sessions.load(id(key)?, parse(value)?),
            "ask" => {
                let ask: AskId = id(key)?;
                let info: AskInfo = parse(value)?;
                self.asks_by_time.insert((info.at, info.rev, ask));
                self.asks.load(ask, info);
            }
            "workstream" => {
                let w: WorkstreamId = id(key)?;
                let info: WorkstreamInfo = parse(value)?;
                self.quiet_order.insert((info.last_at, info.last_rev, w));
                self.workstreams.load(w, info);
            }
            _ => return Ok(false),
        }
        Ok(true)
    }

    /// How many entries each map holds: members, tasks, dispatches, sessions, open asks,
    /// workstreams.
    #[must_use]
    pub fn sizes(&self) -> [usize; 6] {
        [
            self.members.len(),
            self.tasks.len(),
            self.dispatches.len(),
            self.sessions.len(),
            self.asks.len(),
            self.workstreams.len(),
        ]
    }

    /// Every open ask, in id order.
    pub fn open_asks(&self) -> impl Iterator<Item = (&AskId, &AskInfo)> {
        self.asks.iter()
    }
}

/// A saved row that could not be read back.
#[derive(Debug)]
pub(crate) enum LoadError {
    Key,
    Value(serde_json::Error),
}
