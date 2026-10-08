//! The Orchestrator panel's conversation (api-v1.md, "Orchestrator").
//!
//! A person asks about their work across projects, sessions and machines. Each question runs as a
//! session of an agent CLI the person already uses, started by the hub in a private scratch folder
//! with a versioned prompt and a [`crate::api::TokenScope::Reader`] token: the CLI finds things
//! with `pitcrew`'s read verbs and cannot change anything. Its answer streams from the session's
//! transcript into an [`OrchestratorTurn`], with the [`AnswerReference`]s it cites checked against
//! the hub, and any action it would take returned as an [`AnswerSuggestion`] the person clicks.
//!
//! Conversations are kept per person by the hub, outside the event log, and a person may clear
//! theirs.

use crate::ids::{ConversationId, MemberId, ProjectId, SessionId, TaskId, TaskKey, WorkstreamId};
use crate::model::{Date, Engine, TaskStatus, TimestampMs};
use serde::{Deserialize, Serialize};

/// The longest question, in characters, once cleaned and trimmed.
pub const MAX_QUESTION_CHARS: usize = 4000;
/// The longest answer, in bytes (UTF-8): past it the answer is cut and its CLI gets Esc.
pub const MAX_ANSWER_BYTES: usize = 16 * 1024;
/// The longest an answer may take, in seconds from its question.
pub const MAX_ANSWER_SECONDS: u32 = 300;
/// The most questions in one conversation.
pub const MAX_TURNS: usize = 20;
/// The most conversations kept per person; the oldest are forgotten first.
pub const MAX_CONVERSATIONS: usize = 20;
/// The most references one answer resolves.
pub const MAX_REFERENCES: usize = 50;
/// The most suggestions one answer holds.
pub const MAX_SUGGESTIONS: usize = 10;

/// `GET /v1/orchestrator`: the caller's own.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Orchestrator {
    /// Each agent CLI, and whether it is installed where questions run.
    pub engines: Vec<EngineStatus>,
    /// The engine the caller chose last for a new conversation: the next one's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub engine: Option<Engine>,
    /// The bounds.
    pub limits: OrchestratorLimits,
    /// The caller's conversations, newest first.
    pub conversations: Vec<Conversation>,
}

/// Whether an agent CLI can answer questions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct EngineStatus {
    /// The CLI.
    pub engine: Engine,
    /// Whether it is on the `PATH` of the hub's own machine, where questions run.
    pub installed: bool,
}

/// The Orchestrator's bounds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct OrchestratorLimits {
    /// [`MAX_QUESTION_CHARS`].
    pub question_chars: u32,
    /// [`MAX_ANSWER_BYTES`].
    pub answer_bytes: u32,
    /// [`MAX_ANSWER_SECONDS`].
    pub answer_seconds: u32,
    /// [`MAX_TURNS`].
    pub turns: u32,
    /// [`MAX_CONVERSATIONS`].
    pub conversations: u32,
}

impl OrchestratorLimits {
    /// The bounds this protocol sets.
    pub const CURRENT: Self = Self {
        question_chars: MAX_QUESTION_CHARS as u32,
        answer_bytes: MAX_ANSWER_BYTES as u32,
        answer_seconds: MAX_ANSWER_SECONDS,
        turns: MAX_TURNS as u32,
        conversations: MAX_CONVERSATIONS as u32,
    };
}

/// `POST /v1/orchestrator/questions`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Question {
    /// The question.
    pub text: String,
    /// The CLI for a new conversation; the remembered one, else `claude`, when absent. A
    /// follow-up keeps its conversation's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub engine: Option<Engine>,
    /// The conversation to follow up in; a new conversation when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub conversation: Option<ConversationId>,
    /// The agent the session runs as: one of the caller's own. The caller's back office when
    /// absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub agent: Option<MemberId>,
}

/// A person's questions and the answers to them, in one agent CLI.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Conversation {
    /// Its id.
    pub id: ConversationId,
    /// Its CLI.
    pub engine: Engine,
    /// The agent its sessions run as.
    pub agent: MemberId,
    /// When it started.
    pub started: TimestampMs,
    /// The session that answers it, while that lives.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub session: Option<SessionId>,
    /// Its questions and answers, oldest first.
    pub turns: Vec<OrchestratorTurn>,
}

/// One question and its answer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct OrchestratorTurn {
    /// The question, as sent to the CLI.
    pub question: String,
    /// When it was asked.
    pub asked: TimestampMs,
    /// The session that answers it.
    pub session: SessionId,
    /// Where it stands.
    pub state: TurnState,
    /// The answer so far, from the session's transcript; untrusted text, shown as text.
    pub answer: String,
    /// What the answer cites that the hub knows.
    pub references: Vec<AnswerReference>,
    /// Actions the answer suggests: they do nothing unless the person clicks one.
    pub suggestions: Vec<AnswerSuggestion>,
    /// What it took, once it has ended.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub usage: Option<AnswerUsage>,
    /// When it ended.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub ended: Option<TimestampMs>,
    /// Why it ended, when it did not end with an answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub note: Option<String>,
}

/// Where an answer stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum TurnState {
    /// Its CLI is answering.
    Answering,
    /// Its transcript's turn ended.
    Answered,
    /// The person stopped it.
    Canceled,
    /// It took longer than [`MAX_ANSWER_SECONDS`].
    TimedOut,
    /// It passed [`MAX_ANSWER_BYTES`], and was cut there.
    TooLong,
    /// Its session ended, or could not start, before it answered.
    Failed,
}

impl TurnState {
    /// Whether it has ended, whichever way.
    #[must_use]
    pub fn has_ended(self) -> bool {
        self != Self::Answering
    }
}

/// What an answer took, from its transcript.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct AnswerUsage {
    /// From the question to the answer's end, in milliseconds.
    pub duration_ms: u64,
    /// The tools its CLI ran meanwhile (the `pitcrew` reads among them).
    pub tool_runs: u32,
    /// The answer's size, in bytes.
    pub answer_bytes: u32,
}

/// Something an answer cites that the hub knows, which the panel links to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct AnswerReference {
    /// The reference exactly as the answer has it, e.g. `ses_01JB…` or `PAP-4`.
    pub text: String,
    /// What it points to.
    pub target: ReferenceTarget,
    /// A short name for it: a session's title, a task's key and title, a workstream's name.
    pub label: String,
}

/// What a reference points to. Each is an app route: the console's session, a task, a
/// workstream, a project, or a workstream's or project's recap.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReferenceTarget {
    /// A session.
    Session {
        /// Its id.
        id: SessionId,
    },
    /// A task.
    Task {
        /// Its id.
        id: TaskId,
        /// Its key.
        key: TaskKey,
    },
    /// A workstream.
    Workstream {
        /// Its id.
        id: WorkstreamId,
        /// Its project.
        project: ProjectId,
    },
    /// A project.
    Project {
        /// Its id.
        id: ProjectId,
    },
    /// A workstream's or project's recap, or one day of it.
    Recap {
        /// The project.
        project: ProjectId,
        /// The workstream, for a workstream's recap.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        workstream: Option<WorkstreamId>,
        /// The day.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        date: Option<Date>,
    },
}

/// An action an answer suggests. It does nothing by itself: the panel shows it, and only the
/// person's click acts, as the person, through the usual route.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AnswerSuggestion {
    /// Move a task (`POST /v1/tasks/{id}/move`, after the person confirms).
    MoveTask {
        /// The task.
        task: TaskId,
        /// Its key.
        key: TaskKey,
        /// Where to.
        to: TaskStatus,
        /// What the button says, e.g. "Move PAP-4 to review".
        label: String,
    },
    /// Open a page.
    Open {
        /// The page.
        target: ReferenceTarget,
        /// What the button says.
        label: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn turns_and_targets_have_their_wire_shapes() {
        let session: SessionId = "01J00000000000000000000001".parse().unwrap_or_default();
        let turn = OrchestratorTurn {
            question: "What did my agents do today?".into(),
            asked: 1,
            session,
            state: TurnState::TimedOut,
            answer: String::new(),
            references: vec![AnswerReference {
                text: "recap:wst_01J00000000000000000000002@2026-10-05".into(),
                target: ReferenceTarget::Recap {
                    project: "01J00000000000000000000003".parse().unwrap_or_default(),
                    workstream: Some("01J00000000000000000000002".parse().unwrap_or_default()),
                    date: Some(Date("2026-10-05".into())),
                },
                label: "Recap of Paper, 2026-10-05".into(),
            }],
            suggestions: Vec::new(),
            usage: None,
            ended: Some(2),
            note: Some("It took longer than 300 seconds.".into()),
        };
        let value = serde_json::to_value(&turn).unwrap_or_default();
        assert_eq!(value["state"], "timed_out");
        assert_eq!(value["references"][0]["target"]["kind"], "recap");
        assert!(value.get("usage").is_none());
        assert_eq!(
            serde_json::from_value::<OrchestratorTurn>(value).ok(),
            Some(turn)
        );
        let question: Question = serde_json::from_value(json!({"text": "What is blocked?"}))
            .unwrap_or(Question {
                text: String::new(),
                engine: None,
                conversation: None,
                agent: None,
            });
        assert_eq!(question.text, "What is blocked?");
        assert_eq!(question.conversation, None);
        assert!(TurnState::Canceled.has_ended() && !TurnState::Answering.has_ended());
        assert_eq!(OrchestratorLimits::CURRENT.answer_bytes, 16 * 1024);
    }
}
