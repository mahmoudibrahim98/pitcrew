//! The append-only event log.
//!
//! Every change in PitCrew is an [`Event`]: a session starting, a task moving, an ask being
//! answered, a recap being proposed. Events are:
//! - **authored**: `author` is stamped by the hub from the caller's token, never taken from a
//!   request body (ADR-0006). An agent's events also name the person it acts for, in
//!   `on_behalf_of`;
//! - **ordered**: event ids are ULIDs, so they sort by creation time on any machine;
//! - **append-only**: nothing is edited in place. Pages, recaps, the Inbox and search are
//!   projections.

use crate::ids::{
    AskId, DispatchId, EventId, MachineId, MemberId, SessionId, TaskId, WorkspaceId, WorkstreamId,
};
use crate::model::{
    Answer, Ask, Dispatch, DispatchOutcome, Health, LinkBasis, Liveness, Mover, Project, Receipt,
    Session, SessionState, Subtask, Task, TaskStatus, TimestampMs, Workstream, WorkstreamStatus,
};
use serde::{Deserialize, Serialize};

pub use crate::model::BriefTarget;

/// One entry in the log.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    /// Id; sorts by creation time.
    pub id: EventId,
    /// When it happened.
    pub at: TimestampMs,
    /// The workspace.
    pub workspace: WorkspaceId,
    /// Who did it. Stamped by the hub.
    pub author: MemberId,
    /// For an agent's events, the person it acts for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_behalf_of: Option<MemberId>,
    /// What happened.
    pub body: EventBody,
}

impl Event {
    /// Creates an event with a new id and the current time.
    #[must_use]
    pub fn now(workspace: WorkspaceId, author: MemberId, body: EventBody) -> Self {
        let at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
        Self {
            id: EventId::new(),
            at,
            workspace,
            author,
            on_behalf_of: None,
            body,
        }
    }
}

/// What happened. On the wire: `{"type": "task_moved", "data": {…}}`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
#[non_exhaustive]
pub enum EventBody {
    // Machines and sessions: written by runners.
    /// A machine became reachable, unreachable or stopped.
    MachineLiveness {
        /// The machine.
        machine: MachineId,
        /// New liveness.
        liveness: Liveness,
    },
    /// A session was seen for the first time, whether started, imported or discovered.
    SessionDiscovered {
        /// The session as first seen.
        session: Session,
    },
    /// A session changed state.
    SessionStateChanged {
        /// The session.
        session: SessionId,
        /// Previous state.
        from: SessionState,
        /// New state.
        to: SessionState,
        /// A one-line status, if the runner has one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status_line: Option<String>,
    },
    /// An agent finished a turn.
    TurnEnded {
        /// The session.
        session: SessionId,
        /// Where the turn is in the transcript.
        receipt: Receipt,
    },
    /// An agent ran a tool, such as a shell command.
    ToolRan {
        /// The session.
        session: SessionId,
        /// The tool, e.g. `Bash`.
        tool: String,
        /// A short target, e.g. the command or file.
        target: String,
        /// A short outcome, e.g. "212 of 240 passed".
        outcome: String,
        /// Whether it failed.
        failed: bool,
        /// Where it is in the transcript.
        receipt: Receipt,
    },
    /// An agent edited a file.
    FileEdited {
        /// The session.
        session: SessionId,
        /// Path, relative to the session's working directory where possible.
        path: String,
        /// Lines added.
        added: u32,
        /// Lines removed.
        removed: u32,
    },
    /// A session was linked to a workstream or task.
    SessionLinked {
        /// The session.
        session: SessionId,
        /// The workstream.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workstream: Option<WorkstreamId>,
        /// The task.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task: Option<TaskId>,
        /// Why.
        basis: LinkBasis,
    },
    /// A session's process ended.
    SessionEnded {
        /// The session.
        session: SessionId,
    },

    // Work structure: written by the hub.
    /// A project was created.
    ProjectCreated {
        /// The project.
        project: Project,
    },
    /// A workstream was created.
    WorkstreamCreated {
        /// The workstream.
        workstream: Workstream,
    },
    /// A workstream's status or health changed.
    WorkstreamChanged {
        /// The workstream.
        workstream: WorkstreamId,
        /// New status.
        status: WorkstreamStatus,
        /// New health.
        health: Health,
    },
    /// A task was created.
    TaskCreated {
        /// The task.
        task: Task,
    },
    /// A task moved. The hub validates it with `TaskStatus::can_move`.
    TaskMoved {
        /// The task.
        task: TaskId,
        /// Previous status.
        from: TaskStatus,
        /// New status.
        to: TaskStatus,
        /// Who moved it.
        mover: Mover,
    },
    /// A task was assigned or unassigned.
    TaskAssigned {
        /// The task.
        task: TaskId,
        /// The new assignee.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        assignee: Option<MemberId>,
    },
    /// A task's subtasks were replaced, for example from an agent's updated plan.
    SubtasksReplaced {
        /// The task.
        task: TaskId,
        /// The full new list.
        subtasks: Vec<Subtask>,
    },

    // Delegation.
    /// A dispatch started.
    DispatchStarted {
        /// The dispatch.
        dispatch: Dispatch,
    },
    /// A dispatch finished.
    DispatchFinished {
        /// The dispatch.
        dispatch: DispatchId,
        /// Outcome.
        outcome: DispatchOutcome,
        /// The agent's closing summary.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        summary: Option<String>,
    },

    // Attention.
    /// An ask was raised.
    AskRaised {
        /// The ask.
        ask: Ask,
    },
    /// An ask was answered.
    AskAnswered {
        /// The ask.
        ask: AskId,
        /// The answer.
        answer: Answer,
    },

    // Conversation and record.
    /// A comment on a task or workstream.
    CommentPosted {
        /// On this task.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task: Option<TaskId>,
        /// Or on this workstream.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workstream: Option<WorkstreamId>,
        /// Text (markdown).
        text: String,
        /// Mentioned members. A mention gives an agent one turn.
        #[serde(default)]
        mentions: Vec<MemberId>,
    },
    /// The back office proposed a new "Where it stands".
    BriefProposed {
        /// Which brief.
        target: BriefTarget,
        /// Proposed text.
        text: String,
        /// Evidence for every claim.
        receipts: Vec<Receipt>,
    },
    /// A brief was accepted, applied automatically or edited.
    BriefAccepted {
        /// Which brief.
        target: BriefTarget,
        /// The text now in force.
        text: String,
        /// Whether a person pinned it. Pinned briefs only get proposals.
        pinned: bool,
    },
    /// A decision was recorded.
    DecisionRecorded {
        /// Workstream it belongs to.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workstream: Option<WorkstreamId>,
        /// The decision.
        text: String,
        /// Why.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        why: Option<String>,
        /// Evidence.
        #[serde(default)]
        receipts: Vec<Receipt>,
    },
}
