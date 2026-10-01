//! What rules ask for, and what became of it.

use pitcrew_protocol::events::EventBody;
use pitcrew_protocol::ids::{EventId, MemberId, SessionId, TaskId};
use pitcrew_protocol::model::{AskKind, Receipt, TimestampMs};
use pitcrew_recap::BriefProposal;
use serde::{Deserialize, Serialize};

/// Something a rule wants done. The office never does it itself: the caller applies emitted
/// actions through [`Commands`](crate::Commands).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Action {
    /// Append an event, e.g. a task move by the back office.
    Append {
        /// The event body.
        body: EventBody,
        /// The evidence for it.
        because: Vec<Receipt>,
    },
    /// Raise an ask.
    RaiseAsk {
        /// The ask.
        ask: AskDraft,
    },
    /// Propose a "Where it stands".
    ProposeBrief {
        /// The proposal.
        proposal: BriefProposal,
    },
}

impl Action {
    /// The evidence for the action.
    #[must_use]
    pub fn receipts(&self) -> &[Receipt] {
        match self {
            Self::Append { because, .. } => because,
            Self::RaiseAsk { ask } => &ask.receipts,
            Self::ProposeBrief { proposal } => &proposal.receipts,
        }
    }
}

/// An ask to raise. The caller gives it an id, its author (the back office's member), its state
/// and its time.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AskDraft {
    /// Its kind.
    pub kind: AskKind,
    /// Who must answer.
    pub to: MemberId,
    /// The related task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<TaskId>,
    /// The related session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionId>,
    /// One-line title.
    pub title: String,
    /// Context.
    #[serde(default)]
    pub body: String,
    /// Offered options, in order.
    #[serde(default)]
    pub options: Vec<String>,
    /// The evidence. Never empty.
    pub receipts: Vec<Receipt>,
}

/// Which cap stopped an action.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapScope {
    /// The rule's own cap per hour.
    Rule,
    /// The cap per hour over all rules.
    Global,
}

/// Why an action was refused. The first three are the hard "never" list.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Refusal {
    /// Never send anything outward: no approval asks (the way outward writes to GitHub or Jira
    /// are requested), and nothing but internal events.
    SendsOutward,
    /// Never mark a task done unless it allows automatic acceptance.
    MarksDone,
    /// Never answer an ask addressed to a person (or to anyone not known to be an agent), nor a
    /// decision or an approval, which are always a person's.
    AnswersPerson,
    /// The event is not one the office writes.
    NotAllowed,
    /// The move breaks `TaskStatus::can_move` for the back office, or starts from a status the
    /// task is not in.
    MoveNotAllowed,
    /// The task is not known.
    UnknownTask,
    /// The action cites no evidence.
    NoEvidence,
}

impl Refusal {
    /// A short code, as in the run log, e.g. `marks_done`.
    #[must_use]
    pub fn code(self) -> &'static str {
        match self {
            Self::SendsOutward => "sends_outward",
            Self::MarksDone => "marks_done",
            Self::AnswersPerson => "answers_person",
            Self::NotAllowed => "not_allowed",
            Self::MoveNotAllowed => "move_not_allowed",
            Self::UnknownTask => "unknown_task",
            Self::NoEvidence => "no_evidence",
        }
    }

    fn from_code(code: &str) -> Option<Self> {
        [
            Self::SendsOutward,
            Self::MarksDone,
            Self::AnswersPerson,
            Self::NotAllowed,
            Self::MoveNotAllowed,
            Self::UnknownTask,
            Self::NoEvidence,
        ]
        .into_iter()
        .find(|r| r.code() == code)
    }
}

/// What became of an action.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum Outcome {
    /// Handed to the caller to apply.
    Emitted,
    /// Over a cap: logged, not applied.
    Capped {
        /// Which cap.
        scope: CapScope,
    },
    /// Against the rules: logged, not applied.
    Refused {
        /// Why.
        refusal: Refusal,
    },
}

impl Outcome {
    /// `emitted`, `capped` or `refused`, as in the run log.
    #[must_use]
    pub fn code(self) -> &'static str {
        match self {
            Self::Emitted => "emitted",
            Self::Capped { .. } => "capped",
            Self::Refused { .. } => "refused",
        }
    }

    /// Which cap, or which refusal, as in the run log.
    #[must_use]
    pub fn reason(self) -> Option<&'static str> {
        match self {
            Self::Emitted => None,
            Self::Capped {
                scope: CapScope::Rule,
            } => Some("rule"),
            Self::Capped {
                scope: CapScope::Global,
            } => Some("global"),
            Self::Refused { refusal } => Some(refusal.code()),
        }
    }

    /// Reads [`Outcome::code`] and [`Outcome::reason`] back.
    #[must_use]
    pub fn from_parts(code: &str, reason: Option<&str>) -> Option<Self> {
        match (code, reason) {
            ("emitted", None) => Some(Self::Emitted),
            ("capped", Some("rule")) => Some(Self::Capped {
                scope: CapScope::Rule,
            }),
            ("capped", Some("global")) => Some(Self::Capped {
                scope: CapScope::Global,
            }),
            ("refused", Some(r)) => Refusal::from_code(r).map(|refusal| Self::Refused { refusal }),
            _ => None,
        }
    }
}

/// One line of the run log: a rule's action for an event, and its outcome.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// The event's revision.
    pub rev: u64,
    /// The action's place among the event's actions, from 0.
    pub seq: u32,
    /// The event that set it off.
    pub event: EventId,
    /// The event's time.
    pub at: TimestampMs,
    /// The rule.
    pub rule: String,
    /// The action.
    pub action: Action,
    /// What became of it.
    pub outcome: Outcome,
}
