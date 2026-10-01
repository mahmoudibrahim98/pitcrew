//! Activity blocks: bursts of work, each with what changed and receipts.

use crate::checks::Check;
use pitcrew_protocol::ids::{AskId, EventId, MemberId, ProjectId, SessionId, TaskId, WorkstreamId};
use pitcrew_protocol::model::{
    AskKind, BriefTarget, DispatchOutcome, Health, Receipt, TaskStatus, TimestampMs,
    WorkstreamStatus,
};
use serde::{Deserialize, Serialize};

/// How events are grouped, and the caps that bound every block.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    /// A pause longer than this, in milliseconds, ends a block. Default: 20 minutes.
    pub gap_ms: i64,
    /// Most blocks open at once. Past this, the one idle longest is closed. Default: 4096.
    pub max_open: usize,
    /// Most distinct files listed per block. Default: 20.
    pub max_files: usize,
    /// Most facts per block. Default: 24.
    pub max_facts: usize,
    /// Most task links per block. Default: 8.
    pub max_tasks: usize,
    /// Most distinct authors listed per block. Default: 8.
    pub max_actors: usize,
    /// Most receipts per fact, per file and per count. Default: 8; at least 2.
    pub max_receipts: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            gap_ms: 20 * 60 * 1000,
            max_open: 4096,
            max_files: 20,
            max_facts: 24,
            max_tasks: 8,
            max_actors: 8,
            max_receipts: 8,
        }
    }
}

impl Config {
    /// The same config with every field in a usable range.
    #[must_use]
    pub fn normalized(&self) -> Self {
        Self {
            gap_ms: self.gap_ms.max(0),
            max_open: self.max_open.max(1),
            max_files: self.max_files.max(1),
            max_facts: self.max_facts.max(1),
            max_tasks: self.max_tasks.max(1),
            max_actors: self.max_actors.max(1),
            max_receipts: self.max_receipts.max(2),
        }
    }
}

/// What a block groups: one session's events, or, for events outside any active session, one
/// workstream's (or, for tasks without a workstream, one project's).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum BlockKey {
    /// A session's work.
    Session(SessionId),
    /// Work on a workstream outside any active session: moves, comments, asks, health.
    Workstream(WorkstreamId),
    /// Work on a project's tasks that have no workstream.
    Project(ProjectId),
}

/// A burst of work: events of one key with no pause longer than the gap.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Block {
    /// The first event's id. It is stable: rebuilding from the same events gives the same id.
    pub id: EventId,
    /// The last event's id. With `id`, the range of the log the block covers.
    pub last: EventId,
    /// What the block groups.
    pub key: BlockKey,
    /// Time of the earliest event.
    pub start: TimestampMs,
    /// Time of the latest event.
    pub end: TimestampMs,
    /// The session, for a session's block.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionId>,
    /// The workstream the work belongs to, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workstream: Option<WorkstreamId>,
    /// The project the work belongs to, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<ProjectId>,
    /// Tasks the work touched, in order of first mention (capped).
    #[serde(default)]
    pub tasks: Vec<TaskId>,
    /// The agent doing the work: the session's agent, or the author of the first tool run, edit
    /// or turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<MemberId>,
    /// Distinct authors of the block's events, in order of first appearance (capped).
    #[serde(default)]
    pub actors: Vec<MemberId>,
    /// Counts.
    pub counts: Counts,
    /// Files edited, in order of first edit (capped).
    #[serde(default)]
    pub files: Vec<FileTouch>,
    /// Edits to files beyond the cap.
    #[serde(default)]
    pub files_omitted: u32,
    /// Notable facts, in order of first occurrence (capped).
    #[serde(default)]
    pub facts: Vec<Fact>,
    /// Facts beyond the cap.
    #[serde(default)]
    pub facts_omitted: u32,
    /// Receipts for the tool runs: the first ones.
    #[serde(default)]
    pub tool_receipts: Vec<Receipt>,
    /// Receipts for the turns: the first ones.
    #[serde(default)]
    pub turn_receipts: Vec<Receipt>,
}

impl Block {
    /// Every receipt the block holds.
    pub fn receipts(&self) -> impl Iterator<Item = &Receipt> {
        self.facts
            .iter()
            .flat_map(|f| f.receipts.iter())
            .chain(self.files.iter().flat_map(|f| f.receipts.iter()))
            .chain(self.tool_receipts.iter())
            .chain(self.turn_receipts.iter())
    }
}

/// Counts over all of a block's events. They are never capped.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Counts {
    /// Events in the block.
    pub events: u32,
    /// Tool runs.
    pub tools_run: u32,
    /// Tool runs that failed.
    pub tools_failed: u32,
    /// File edits.
    pub file_edits: u32,
    /// Lines added over all edits.
    pub lines_added: u64,
    /// Lines removed over all edits.
    pub lines_removed: u64,
    /// Turns ended.
    pub turns: u32,
    /// Asks raised.
    pub asks_raised: u32,
    /// Asks answered.
    pub asks_answered: u32,
    /// Task moves.
    pub task_moves: u32,
    /// Comments posted.
    pub comments: u32,
}

/// One file edited in a block.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileTouch {
    /// Path as the agent reported it, cleaned and capped (the end is kept).
    pub path: String,
    /// Edits to it.
    pub edits: u32,
    /// Lines added.
    pub added: u64,
    /// Lines removed.
    pub removed: u64,
    /// The edit events: the first and the latest.
    pub receipts: Vec<Receipt>,
}

/// A notable fact, with the evidence for it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fact {
    /// Who did it: the author of the event (for merged facts, of the first one).
    pub by: MemberId,
    /// When it (first) happened.
    pub at: TimestampMs,
    /// What happened.
    pub kind: FactKind,
    /// Evidence: always the event, plus any transcript, job or file receipts it carries.
    pub receipts: Vec<Receipt>,
}

/// What a fact says. Repeated facts about the same thing in one block are merged: two moves of
/// one task become one move from the first status to the last.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FactKind {
    /// A session was seen for the first time.
    SessionStarted {
        /// Its title, cleaned.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
    },
    /// A session was linked to a workstream or task.
    SessionLinked {
        /// The workstream.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workstream: Option<WorkstreamId>,
        /// The task.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task: Option<TaskId>,
    },
    /// The session stopped to wait for a person.
    SessionWaiting {
        /// The last status line, cleaned.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status_line: Option<String>,
    },
    /// The session ended.
    SessionEnded,
    /// An agent was dispatched to a task.
    DispatchStarted {
        /// The task.
        task: TaskId,
        /// The agent.
        agent: MemberId,
    },
    /// A dispatch finished.
    DispatchFinished {
        /// The task, if the dispatch is known.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task: Option<TaskId>,
        /// How it ended.
        outcome: DispatchOutcome,
        /// The agent's closing summary, cleaned.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        summary: Option<String>,
    },
    /// A task was created.
    TaskCreated {
        /// The task.
        task: TaskId,
    },
    /// A task moved, from its first status in the block to its last.
    TaskMoved {
        /// The task.
        task: TaskId,
        /// Status before the first move.
        from: TaskStatus,
        /// Status after the last move.
        to: TaskStatus,
    },
    /// A task was assigned or unassigned.
    TaskAssigned {
        /// The task.
        task: TaskId,
        /// The latest assignee.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        assignee: Option<MemberId>,
    },
    /// A task's plan (its subtasks) was updated.
    PlanUpdated {
        /// The task.
        task: TaskId,
        /// Subtasks done, in the latest plan.
        done: u32,
        /// Subtasks in the latest plan.
        total: u32,
    },
    /// Checks of one kind ran: tests, builds or lint.
    Checks {
        /// Which kind.
        check: Check,
        /// Runs.
        runs: u32,
        /// Runs that failed.
        failures: u32,
        /// Whether the latest run failed.
        last_failed: bool,
    },
    /// A run diverged, e.g. a training loss went to NaN.
    JobDiverged {
        /// Job ids named by the evidence, cleaned (capped).
        #[serde(default)]
        jobs: Vec<String>,
    },
    /// An ask was raised.
    AskRaised {
        /// The ask.
        ask: AskId,
        /// Its kind.
        ask_kind: AskKind,
        /// Who must answer.
        to: MemberId,
        /// Its title, cleaned.
        title: String,
    },
    /// An ask was answered.
    AskAnswered {
        /// The ask.
        ask: AskId,
    },
    /// A comment was posted.
    Commented {
        /// On this task.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task: Option<TaskId>,
        /// Or on this workstream.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workstream: Option<WorkstreamId>,
        /// Members mentioned (capped).
        #[serde(default)]
        mentions: Vec<MemberId>,
    },
    /// A decision was recorded.
    DecisionRecorded {
        /// The decision, cleaned.
        text: String,
    },
    /// A workstream was created.
    WorkstreamCreated {
        /// The workstream.
        workstream: WorkstreamId,
    },
    /// A workstream's status or health changed.
    WorkstreamChanged {
        /// The workstream.
        workstream: WorkstreamId,
        /// Latest status.
        status: WorkstreamStatus,
        /// Latest health.
        health: Health,
    },
    /// A "Where it stands" was accepted or pinned.
    BriefAccepted {
        /// Which brief.
        target: BriefTarget,
        /// Whether it was pinned.
        pinned: bool,
    },
}
