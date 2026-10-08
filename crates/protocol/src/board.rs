//! Drafting a workstream's board from its history (api-v1.md, "Board drafts").
//!
//! A person asks for a draft of one workstream's board: open tasks, what is in progress, what looks
//! done. PitCrew starts an agent CLI the person already uses, with a versioned prompt and a
//! bounded, redacted summary of the workstream's sessions, and the agent answers with a
//! [`BoardProposal`] through `pitcrew board submit`. **Nothing is created until the person
//! reviews it**: the tasks they accept become tasks through the normal commands, labelled
//! [`DRAFTED_LABEL`]; the rest create nothing.
//!
//! - Before anything is sent, [`DraftPreview`] shows what would be (the summary itself, its size,
//!   how many sessions it covers) and an estimate of the agent's usage ([`DraftCost`]). The start
//!   names the preview's [`DraftPreview::digest`], so what starts is exactly what was shown.
//! - [`BoardDraft`] is a draft as the hub keeps it, built from three events:
//!   `board_draft_started`, `board_proposed` and `board_draft_reviewed`.

use crate::ids::{DraftId, MemberId, SessionId, TaskId, WorkstreamId};
use crate::model::{Engine, Task, TaskStatus, TimestampMs};
use serde::{Deserialize, Serialize};

/// The label every task created from an accepted draft carries.
pub const DRAFTED_LABEL: &str = "drafted";

/// The largest proposal an agent may submit, in bytes of JSON.
pub const MAX_PROPOSAL_BYTES: usize = 32 * 1024;
/// The most tasks one proposal may hold.
pub const MAX_PROPOSED_TASKS: usize = 50;
/// The longest proposed title, in characters, once trimmed.
pub const MAX_PROPOSED_TITLE: usize = 200;
/// The longest proposed description, in characters.
pub const MAX_PROPOSED_DESCRIPTION: usize = 2000;
/// The most sessions one proposed task may cite as its evidence.
pub const MAX_EVIDENCE: usize = 20;
/// The longest note, in characters.
pub const MAX_NOTE: usize = 2000;

/// An estimate of what the drafting agent uses, in tokens.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct UsageEstimate {
    /// What it reads: the prompt (a token for every 4 bytes, rounded up) and the agent CLI's own
    /// instructions (a fixed allowance).
    pub input_tokens: u32,
    /// The most its answer may take: the proposal's bound (a token for every 4 bytes).
    pub output_tokens: u32,
}

/// What a draft sends to its agent, and what that is likely to cost.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct DraftCost {
    /// The workstream's sessions the summary covers.
    pub sessions: u32,
    /// The workstream's sessions left out by the summary's bounds: the least recently active.
    pub sessions_left_out: u32,
    /// The workstream's tasks the summary lists, so the agent does not propose them again.
    pub tasks: u32,
    /// The summary's size in bytes (UTF-8).
    pub summary_bytes: u32,
    /// The whole prompt's size in bytes: the versioned template with the summary in it.
    pub prompt_bytes: u32,
    /// Strings the summary replaced because they looked like secrets, e-mail addresses or a
    /// person's home folder.
    pub redacted: u32,
    /// The agent's likely usage.
    pub estimate: UsageEstimate,
}

/// `GET /v1/workstreams/{id}/board-draft`: what a draft of the workstream would send, before
/// anything is.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct DraftPreview {
    /// The workstream.
    pub workstream: WorkstreamId,
    /// The prompt's name and version, e.g. `draft-board/v1`.
    pub prompt: String,
    /// The sizes and the estimate.
    pub cost: DraftCost,
    /// The summary, exactly as the agent would read it.
    pub summary: String,
    /// The SHA-256 of the whole prompt, as lowercase hex. A start names it: if the workstream has
    /// changed since, the prompt would differ, and the start is refused (`409`).
    pub digest: String,
}

/// `POST /v1/workstreams/{id}/board-drafts`: start drafting, with what the preview showed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct StartDraft {
    /// The agent to run: one of the caller's own. Default: the back office (`@office`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub agent: Option<MemberId>,
    /// The CLI to run it in. Default: the agent's persona's, else Claude Code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub engine: Option<Engine>,
    /// The [`DraftPreview::digest`] the person confirmed.
    pub digest: String,
}

/// Where a draft is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum DraftState {
    /// Its agent is working on it.
    Running,
    /// Its agent proposed a board, waiting for the person's review.
    Proposed,
    /// The person reviewed the proposal: accepted all, some or none of it.
    Reviewed,
    /// Its session ended without a proposal.
    Ended,
}

/// One task a draft proposes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ProposedTask {
    /// Its title.
    pub title: String,
    /// Where it stands: `backlog`, `todo`, `in_progress`, `review` or `done` (never `canceled`).
    pub status: TaskStatus,
    /// What it is about, if the agent says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub description: Option<String>,
    /// The workstream's sessions it is drawn from.
    #[serde(default)]
    pub evidence: Vec<SessionId>,
}

/// What the drafting agent proposes (`POST /v1/board-drafts/{id}/proposal`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct BoardProposal {
    /// The tasks, in the order the agent gives them; a review names them by their index here.
    pub tasks: Vec<ProposedTask>,
    /// Anything else the agent wants the person to know.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub note: Option<String>,
}

/// `POST /v1/board-drafts/{id}/review`: which proposed tasks to create. Every other one is
/// rejected; an empty list rejects the whole proposal.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct DraftReview {
    /// Indexes into [`BoardProposal::tasks`].
    pub accept: Vec<u32>,
}

/// A proposed task the person accepted, and the task it became.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct DraftedTask {
    /// Its index in [`BoardProposal::tasks`].
    pub item: u32,
    /// The task created.
    pub task: TaskId,
}

/// A board draft, as the hub keeps it (`GET /v1/board-drafts`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct BoardDraft {
    /// Id.
    pub id: DraftId,
    /// The workstream it drafts.
    pub workstream: WorkstreamId,
    /// The agent drafting it.
    pub agent: MemberId,
    /// The CLI it runs in.
    pub engine: Engine,
    /// The session the agent runs in.
    pub session: SessionId,
    /// The person who asked for it.
    pub by: MemberId,
    /// The prompt's name and version.
    pub prompt: String,
    /// What was sent, and the estimate the person confirmed.
    pub cost: DraftCost,
    /// When it started.
    pub started: TimestampMs,
    /// Where it is.
    pub state: DraftState,
    /// The agent's proposal, once it has made one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub proposal: Option<BoardProposal>,
    /// When the proposal came.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub proposed: Option<TimestampMs>,
    /// When the person reviewed it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub reviewed: Option<TimestampMs>,
    /// The proposed tasks the person accepted, and the tasks they became.
    #[serde(default)]
    pub accepted: Vec<DraftedTask>,
    /// The proposed tasks the person rejected.
    #[serde(default)]
    pub rejected: Vec<u32>,
}

/// The answer to a review: the draft as it is now, and the tasks it created.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct DraftReviewed {
    /// The draft, reviewed.
    pub draft: BoardDraft,
    /// The tasks created, in the order of the proposal.
    pub tasks: Vec<Task>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_proposal_reads_what_an_agent_writes() {
        let proposal: BoardProposal = serde_json::from_value(json!({
            "tasks": [
                {"title": "Write the method section", "status": "in_progress",
                 "evidence": ["01JB000000000000000SES0001"]},
                {"title": "Submit", "status": "todo", "description": "After review."}
            ]
        }))
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(proposal.tasks.len(), 2);
        assert_eq!(proposal.tasks[1].evidence, Vec::<SessionId>::new());
        assert_eq!(proposal.note, None);
        let back = serde_json::to_value(&proposal).unwrap_or_else(|e| panic!("{e}"));
        assert!(back["tasks"][0].get("description").is_none());
        assert!(back.get("note").is_none());
    }

    #[test]
    fn a_start_needs_its_digest() {
        assert!(serde_json::from_value::<StartDraft>(json!({})).is_err());
        let start: StartDraft =
            serde_json::from_value(json!({"digest": "ab"})).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!((start.agent, start.engine), (None, None));
    }
}
