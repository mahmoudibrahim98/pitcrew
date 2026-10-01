//! [`SyncState`]: everything one repository's sync needs to remember between calls. It is plain,
//! serde-serialisable data; the caller persists it (it never goes into the event log, per the
//! brief).

use crate::time::GithubTimestamp;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Cached cursor and conditional-request data for one list endpoint (issues, pull requests or
/// milestones) on one repository.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListCache {
    /// The `ETag` of the last response, sent back as `If-None-Match`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
    /// The `Last-Modified` of the last response, sent back as `If-Modified-Since`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_modified: Option<String>,
    /// The latest `updated_at` seen so far: the incremental cursor. For issues this is sent as
    /// `since`; for pull requests (whose list endpoint has no `since` parameter) it is where a
    /// descending-by-updated listing stops.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<GithubTimestamp>,
}

/// Whether an issue is open or closed, and why if closed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloseReason {
    /// Closed as completed (or closed before GitHub recorded a reason).
    Completed,
    /// Closed as not planned.
    NotPlanned,
}

impl CloseReason {
    /// Maps GitHub's `state_reason` string. Anything other than `"not_planned"` (including a
    /// missing or unrecognised value) is treated as `Completed`, matching older issues closed
    /// before GitHub introduced this field.
    #[must_use]
    pub(crate) fn from_state_reason(state_reason: Option<&str>) -> Self {
        match state_reason {
            Some("not_planned") => CloseReason::NotPlanned,
            _ => CloseReason::Completed,
        }
    }
}

/// The owned fields of one issue, as last seen, for diffing against the next read.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssueSnapshot {
    pub(crate) title: String,
    pub(crate) body: String,
    pub(crate) open: bool,
    pub(crate) close_reason: Option<CloseReason>,
    pub(crate) labels: Vec<String>,
    pub(crate) assignees: Vec<String>,
    pub(crate) milestone_number: Option<u64>,
    pub(crate) updated_at: GithubTimestamp,
}

/// The owned fields of one pull request, as last seen.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullSnapshot {
    pub(crate) title: String,
    pub(crate) open: bool,
    pub(crate) merged: bool,
    pub(crate) updated_at: GithubTimestamp,
}

/// The owned fields of one milestone, as last seen.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MilestoneSnapshot {
    pub(crate) title: String,
    pub(crate) open: bool,
}

/// Everything remembered about one repository's sync.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoState {
    /// Cache for the issues list.
    #[serde(default)]
    pub issues: ListCache,
    /// Cache for the pull requests list.
    #[serde(default)]
    pub pulls: ListCache,
    /// Cache for the milestones list.
    #[serde(default)]
    pub milestones: ListCache,
    /// Last-seen owned fields, by issue number.
    #[serde(default)]
    pub issue_snapshots: BTreeMap<u64, IssueSnapshot>,
    /// Last-seen owned fields, by pull request number.
    #[serde(default)]
    pub pull_snapshots: BTreeMap<u64, PullSnapshot>,
    /// Last-seen owned fields, by milestone number.
    #[serde(default)]
    pub milestone_snapshots: BTreeMap<u64, MilestoneSnapshot>,
    /// Consecutive secondary-rate-limit hits for this repository, for exponential backoff. Reset
    /// to zero on any request that is not rate-limited.
    #[serde(default)]
    pub secondary_backoff_attempts: u32,
}

/// Cursors, `ETag`s and snapshots for every tracked repository.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncState {
    /// By `owner/repo`.
    #[serde(default)]
    pub repos: BTreeMap<String, RepoState>,
}

impl SyncState {
    /// An empty state: every repository will do a first full sync.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}
