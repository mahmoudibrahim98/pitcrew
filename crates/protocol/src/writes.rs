//! Outward writes to GitHub and Jira, each approved by a person first (api-v1.md, "Outward
//! writes: every one approved first").
//!
//! A hub change that implies a write upstream (or a person asking for one) raises an ask of kind
//! `approval` together with a [`WriteProposal`]: exactly what will be sent. The hub sends it only
//! after the person answers "Send", and records each attempt (`write_started`) and its result
//! (`write_finished`, a [`WriteResult`]). [`UpstreamWrite`] is a write as the routes show it.
//! Nothing here ever carries a credential.

use crate::ids::{AskId, EventId, IntegrationId, MemberId, TaskId};
use crate::model::{ExternalRef, ExternalSystem, TimestampMs};
use serde::{Deserialize, Serialize};

/// The longest comment `POST /v1/writes` sends, in characters.
pub const MAX_COMMENT_CHARS: usize = 65_536;
/// The longest issue body a write sends, in characters (GitHub's own limit).
pub const MAX_BODY_CHARS: usize = 65_536;
/// The options of every approval ask, in order: the first sends, the second does not.
pub const APPROVAL_OPTIONS: [&str; 2] = ["Send", "Don't send"];
/// The option of [`APPROVAL_OPTIONS`] that approves.
pub const SEND_OPTION: usize = 0;

/// What a write does upstream.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum WriteOperation {
    /// Create an issue from a task.
    CreateIssue,
    /// Comment on the task's issue.
    Comment,
    /// Change the issue's title, body, labels, milestone or epic.
    Update,
    /// Close the issue (Jira: a transition into the Done category).
    Close,
    /// Reopen the issue (Jira: a transition into To Do).
    Reopen,
}

/// An issue's state, as a write sets it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum IssueState {
    /// Open (Jira: not in the Done category).
    Open,
    /// Closed (Jira: in the Done category).
    Closed,
}

/// Why an issue is closed (GitHub's `state_reason`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum CloseReason {
    /// The work is done (a task moved to `done`).
    Completed,
    /// It will not be done (a task moved to `canceled`).
    NotPlanned,
}

/// The fields of a write: what it sends (`after`), or upstream's values of the same fields
/// before it (`before`). Only the fields being changed are present.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct WriteFields {
    /// Title (Jira: summary).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub title: Option<String>,
    /// Body (Jira: description).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub body: Option<String>,
    /// The whole label list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub labels: Option<Vec<String>>,
    /// GitHub: the milestone, as a link key (`owner/repo#milestone:2`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub milestone: Option<String>,
    /// Jira: the epic's key (`DEMO-5`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub epic: Option<String>,
    /// Open or closed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub state: Option<IssueState>,
    /// Why it is closed (GitHub).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub close_reason: Option<CloseReason>,
    /// A comment's text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub comment: Option<String>,
}

impl WriteFields {
    /// Whether no field is present.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// The names of the fields present, in a fixed order.
    #[must_use]
    pub fn names(&self) -> Vec<&'static str> {
        let mut out = Vec::new();
        for (present, name) in [
            (self.title.is_some(), "title"),
            (self.body.is_some(), "body"),
            (self.labels.is_some(), "labels"),
            (self.milestone.is_some(), "milestone"),
            (self.epic.is_some(), "epic"),
            (self.state.is_some(), "state"),
            (self.close_reason.is_some(), "close_reason"),
            (self.comment.is_some(), "comment"),
        ] {
            if present {
                out.push(name);
            }
        }
        out
    }
}

/// A write waiting for, or past, a person's approval: exactly what will be sent
/// (`write_proposed`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct WriteProposal {
    /// The approval ask; also the write's id.
    pub ask: AskId,
    /// The integration it goes through.
    pub integration: IntegrationId,
    /// GitHub or Jira.
    pub system: ExternalSystem,
    /// The repository (`owner/repo`) or Jira project (`DEMO`) it writes to.
    pub scope: String,
    /// The issue it changes; absent for `create_issue`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub target: Option<ExternalRef>,
    /// The task it is about.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub task: Option<TaskId>,
    /// What it does.
    pub operation: WriteOperation,
    /// Upstream's values of the fields it changes, as the last sync read them.
    #[serde(default)]
    pub before: WriteFields,
    /// Exactly what is sent.
    pub after: WriteFields,
    /// Whose change implied it, or who asked for it.
    pub requested_by: MemberId,
    /// The event that implied it; absent when a person asked for it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub cause: Option<EventId>,
}

/// What came of one attempt, or of a write that was never sent (`write_finished`). On the wire,
/// tagged by `outcome`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum WriteResult {
    /// Upstream accepted it.
    Sent {
        /// The issue a `create_issue` made; the task's `source` becomes it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        created: Option<ExternalRef>,
        /// A link to what was written.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        url: Option<String>,
    },
    /// Upstream refused it, or could not be reached.
    Failed {
        /// For people to read: upstream's message (capped, without hidden characters) or why it
        /// could not be reached. Never holds a credential.
        message: String,
        /// Upstream's HTTP status, when it answered.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        status: Option<u16>,
    },
    /// Nothing was sent: denied, no longer what the task says, or its integration is gone.
    NotSent {
        /// Why, for people to read.
        reason: String,
    },
}

/// Where a write stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum WriteState {
    /// Waiting for the person's answer.
    Pending,
    /// Approved; about to be sent.
    Approved,
    /// Denied; about to be recorded as not sent.
    Denied,
    /// Being sent now.
    Sending,
    /// Upstream accepted it.
    Sent,
    /// The last attempt failed; it can be retried.
    Failed,
    /// It was never sent, and never will be.
    NotSent,
}

impl WriteState {
    /// Whether nothing more will happen to it.
    #[must_use]
    pub fn is_final(self) -> bool {
        matches!(self, Self::Sent | Self::NotSent)
    }
}

/// A write, as `GET /v1/writes` shows it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct UpstreamWrite {
    /// What it sends.
    pub proposal: WriteProposal,
    /// Where it stands.
    pub state: WriteState,
    /// How many times it was sent (each `write_started`).
    pub attempts: u32,
    /// When it was proposed.
    pub proposed_at: TimestampMs,
    /// When its ask was answered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub answered_at: Option<TimestampMs>,
    /// Who answered it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub answered_by: Option<MemberId>,
    /// When its last attempt, or its denial, was recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub finished_at: Option<TimestampMs>,
    /// What came of it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub result: Option<WriteResult>,
}

/// `POST /v1/writes`: a person asks for a write.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct NewWrite {
    /// The task.
    pub task: TaskId,
    /// `create_issue` or `comment`.
    pub operation: WriteOperation,
    /// A comment's text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub text: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn results_are_tagged_by_outcome() {
        let sent = WriteResult::Sent {
            created: None,
            url: Some("https://github.com/example-org/demo-repo/issues/1".into()),
        };
        assert_eq!(
            serde_json::to_value(&sent).unwrap(),
            serde_json::json!({
                "outcome": "sent",
                "url": "https://github.com/example-org/demo-repo/issues/1"
            })
        );
        let not_sent: WriteResult =
            serde_json::from_value(serde_json::json!({"outcome": "not_sent", "reason": "denied"}))
                .unwrap();
        assert_eq!(
            not_sent,
            WriteResult::NotSent {
                reason: "denied".into()
            }
        );
    }

    #[test]
    fn fields_name_what_they_hold() {
        let fields = WriteFields {
            title: Some("New".into()),
            state: Some(IssueState::Closed),
            ..WriteFields::default()
        };
        assert_eq!(fields.names(), vec!["title", "state"]);
        assert!(WriteFields::default().is_empty());
        assert_eq!(
            serde_json::to_value(&fields).unwrap(),
            serde_json::json!({"title": "New", "state": "closed"})
        );
    }

    #[test]
    fn the_first_option_sends() {
        assert_eq!(APPROVAL_OPTIONS[SEND_OPTION], "Send");
        assert!(WriteState::Sent.is_final() && WriteState::NotSent.is_final());
        assert!(!WriteState::Failed.is_final());
    }
}
