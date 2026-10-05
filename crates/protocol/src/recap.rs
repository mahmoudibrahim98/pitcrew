//! Recaps as the API serves them: activity blocks, their one-line summaries, and day paragraphs.
//!
//! The recap engine (`crates/recap`, stream F) builds these from the event log; the API serves
//! them on `GET /v1/recaps/blocks` and `GET /v1/recaps/days` (`docs/build/contracts/api-v1.md`,
//! "Recaps"). Recaps are derived, never stored as events.
//!
//! - A [`Block`] is a burst of one session's (or one workstream's) work with what changed, and
//!   every [`Fact`] in it carries receipts.
//! - A [`Summary`] is text whose every clause is a [`Span`] citing receipts. A span's `range` is a
//!   **UTF-8 byte range** of `text`, on character boundaries, serialised as
//!   `{"start": …, "end": …}`. Clients whose strings are not UTF-8 (JavaScript's are UTF-16)
//!   convert the offsets before slicing.
//! - All text is the engine's cleaned text (control and direction-changing characters removed,
//!   lengths capped), but it still comes from agents and people: render it as text, never as
//!   markup.

use crate::ids::{AskId, EventId, MemberId, ProjectId, SessionId, TaskId, WorkstreamId};
use crate::model::{
    AskKind, BriefTarget, Date, DispatchOutcome, Health, Receipt, TaskStatus, TimestampMs,
    WorkstreamStatus,
};
use serde::{Deserialize, Serialize};
use std::ops::Range;

/// `GET /v1/recaps/blocks`: blocks per page when `limit` is absent.
pub const BLOCKS_DEFAULT_LIMIT: usize = 50;
/// `GET /v1/recaps/blocks`: the largest `limit`.
pub const BLOCKS_MAX_LIMIT: usize = 200;
/// `GET /v1/recaps/days`: days per page when `limit` is absent.
pub const DAYS_DEFAULT_LIMIT: usize = 7;
/// `GET /v1/recaps/days`: the largest `limit`.
pub const DAYS_MAX_LIMIT: usize = 30;
/// `GET /v1/recaps/days`: the widest `tz`, in minutes either side of UTC (14 hours).
pub const MAX_TZ_MINUTES: i32 = 14 * 60;

// ─── Blocks ──────────────────────────────────────────────────────────────────────────────────

/// A kind of check a command runs. The order is the strength used for command chains.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum Check {
    /// A test suite, e.g. `cargo test` or `pytest`.
    Tests,
    /// A linter or type checker, e.g. `cargo clippy` or `ruff`.
    Lint,
    /// A build or compile, e.g. `cargo build` or `latexmk`.
    Build,
}

/// What a block groups: one session's events, or, for events outside any active session, one
/// workstream's (or, for tasks without a workstream, one project's).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
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
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
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
    #[cfg_attr(feature = "ts", ts(optional))]
    pub session: Option<SessionId>,
    /// The workstream the work belongs to, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub workstream: Option<WorkstreamId>,
    /// The project the work belongs to, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub project: Option<ProjectId>,
    /// Tasks the work touched, in order of first mention (capped).
    #[serde(default)]
    pub tasks: Vec<TaskId>,
    /// The agent doing the work: the session's agent. A session that runs as no agent (found on
    /// disk, or started by a person) has none, and prose names the session itself ("Claude ·
    /// its title"), never the person the runner's events are stamped with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
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
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
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
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
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
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
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
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FactKind {
    /// A session was seen for the first time.
    SessionStarted {
        /// Its title, cleaned.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        title: Option<String>,
    },
    /// A session was linked to a workstream or task.
    SessionLinked {
        /// The workstream.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        workstream: Option<WorkstreamId>,
        /// The task.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        task: Option<TaskId>,
    },
    /// The session stopped to wait for a person.
    SessionWaiting {
        /// The last status line, cleaned.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
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
        #[cfg_attr(feature = "ts", ts(optional))]
        task: Option<TaskId>,
        /// How it ended.
        outcome: DispatchOutcome,
        /// The agent's closing summary, cleaned.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
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
        #[cfg_attr(feature = "ts", ts(optional))]
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
        #[cfg_attr(feature = "ts", ts(optional))]
        task: Option<TaskId>,
        /// Or on this workstream.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
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

// ─── Summaries ───────────────────────────────────────────────────────────────────────────────

/// Text whose every clause is a span with receipts.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Summary {
    /// The prose.
    pub text: String,
    /// Clauses of `text`, in order and not overlapping. Text outside spans is only punctuation
    /// and spaces joining them.
    pub spans: Vec<Span>,
}

/// One clause of a summary and the evidence for it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Span {
    /// Byte range in the summary text, on character boundaries. On the wire,
    /// `{"start": …, "end": …}` in UTF-8 bytes.
    pub range: Range<usize>,
    /// Evidence. Never empty.
    pub receipts: Vec<Receipt>,
}

impl Summary {
    /// The text of a span.
    #[must_use]
    pub fn clause(&self, span: &Span) -> &str {
        self.text.get(span.range.clone()).unwrap_or_default()
    }

    /// Every receipt cited.
    pub fn receipts(&self) -> impl Iterator<Item = &Receipt> {
        self.spans.iter().flat_map(|s| s.receipts.iter())
    }
}

/// The paragraph for one workstream's day, with the blocks it covers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct DayRecap {
    /// The workstream; `None` for blocks not linked to one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub workstream: Option<WorkstreamId>,
    /// The day.
    pub date: Date,
    /// Ids of the blocks covered, in order.
    pub blocks: Vec<EventId>,
    /// The paragraph.
    pub summary: Summary,
}

// ─── Pages ───────────────────────────────────────────────────────────────────────────────────

/// A block with its one-line summary.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct RecapBlock {
    /// The block.
    pub block: Block,
    /// Its line, e.g. "@writer edited method.tex (+84 −12)".
    pub line: Summary,
}

/// `GET /v1/recaps/blocks`: a page of blocks, newest first.
///
/// Page backwards by passing the last block's `id` as `before`. Only `at_start` ends paging.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct BlocksPage {
    /// The blocks, newest first (by block id).
    pub blocks: Vec<RecapBlock>,
    /// True when no older matching block exists.
    pub at_start: bool,
}

/// `GET /v1/recaps/days`: a page of day paragraphs, newest day first.
///
/// Page backwards by passing the last entry's `date` as `before`. Only `at_start` ends paging.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct DaysPage {
    /// The paragraphs: newest date first; within a date, the one without a workstream first,
    /// then by workstream id.
    pub days: Vec<DayRecap>,
    /// True when no older day with matching activity exists.
    pub at_start: bool,
}
