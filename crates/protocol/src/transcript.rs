//! Transcript items: what the Agent console renders, and what recaps and receipts point into.
//!
//! Source adapters (stream A) turn each CLI's records into [`TranscriptItem`]s. The API serves
//! them tail-first in [`TranscriptPage`]s: the newest page first, then older pages by byte offset,
//! so a 200 MB transcript opens as fast as a small one.

use crate::model::TimestampMs;
use serde::{Deserialize, Serialize};

/// One meaningful thing in a transcript. Every item carries the byte `offset` of its record, so it
/// can become a receipt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum TranscriptItem {
    /// A prompt typed by a person (not tool results, not injected context).
    UserPrompt {
        /// Time.
        at: TimestampMs,
        /// Text.
        text: String,
        /// Record offset.
        offset: u64,
    },
    /// Assistant text (markdown).
    AssistantText {
        /// Time.
        at: TimestampMs,
        /// Text.
        text: String,
        /// Record offset.
        offset: u64,
    },
    /// A tool call.
    ToolUse {
        /// Time.
        at: TimestampMs,
        /// The CLI's id for the call, which pairs it with its result.
        call_id: String,
        /// Tool name, e.g. `Bash`, `Edit`, `shell`.
        tool: String,
        /// Short target, such as the command or path.
        target: String,
        /// The raw input, for detail views. Large inputs are truncated by the adapter.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        input: Option<serde_json::Value>,
        /// Record offset.
        offset: u64,
    },
    /// A tool result.
    ToolResult {
        /// Time.
        at: TimestampMs,
        /// The call it answers.
        call_id: String,
        /// Whether it reported an error.
        is_error: bool,
        /// A short summary, e.g. the first lines of output.
        summary: String,
        /// Record offset.
        offset: u64,
    },
    /// A file edit.
    FileEdit {
        /// Time.
        at: TimestampMs,
        /// Path.
        path: String,
        /// Lines added.
        added: u32,
        /// Lines removed.
        removed: u32,
        /// A unified diff, if the CLI recorded one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        diff: Option<String>,
        /// Record offset.
        offset: u64,
    },
    /// The agent's plan / todo list changed. This feeds plan-as-subtasks.
    PlanUpdated {
        /// Time.
        at: TimestampMs,
        /// The full plan, in order.
        items: Vec<PlanItem>,
        /// Record offset.
        offset: u64,
    },
    /// The agent is asking the person something.
    Question {
        /// Time.
        at: TimestampMs,
        /// The question.
        text: String,
        /// Options, if any.
        #[serde(default)]
        options: Vec<String>,
        /// Record offset.
        offset: u64,
    },
    /// A turn ended.
    TurnEnded {
        /// Time.
        at: TimestampMs,
        /// Record offset.
        offset: u64,
    },
}

impl TranscriptItem {
    /// The byte offset of the record this item came from.
    #[must_use]
    pub fn offset(&self) -> u64 {
        match self {
            Self::UserPrompt { offset, .. }
            | Self::AssistantText { offset, .. }
            | Self::ToolUse { offset, .. }
            | Self::ToolResult { offset, .. }
            | Self::FileEdit { offset, .. }
            | Self::PlanUpdated { offset, .. }
            | Self::Question { offset, .. }
            | Self::TurnEnded { offset, .. } => *offset,
        }
    }
}

/// One line of an agent's plan.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct PlanItem {
    /// Text.
    pub text: String,
    /// Status.
    pub status: PlanStatus,
}

/// Status of a plan line.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum PlanStatus {
    /// Not started.
    Pending,
    /// Being worked on.
    InProgress,
    /// Done.
    Completed,
}

/// `GET /v1/sessions/{id}/transcript?before=<offset>&limit=<n>`: a page of items, oldest first.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct TranscriptPage {
    /// The items, oldest first.
    pub items: Vec<TranscriptItem>,
    /// Offset of the first record in this page. Pass it as `before` to get the previous page.
    pub from: u64,
    /// Offset just after the last record in this page.
    pub to: u64,
    /// True if this page starts at the beginning of the transcript.
    pub at_start: bool,
}
