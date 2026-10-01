//! [`SyncState`]: everything this sync needs to remember between calls. Plain, serde-serialisable
//! data; the caller persists it (it never goes into the event log, per the brief) — the same
//! contract as `pitcrew_sync_github::SyncState`.

use crate::time::JiraTimestamp;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Jira's status category: every workflow status belongs to exactly one of these three, regardless
/// of how many custom statuses ("In Review", "Blocked", …) a project defines.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatusCategory {
    /// Not started.
    New,
    /// In progress, in some custom status.
    Indeterminate,
    /// Done.
    Done,
}

impl StatusCategory {
    /// Maps Jira's `statusCategory.key` (`"new"`, `"indeterminate"`, `"done"`). An unrecognised
    /// key (a Jira version this crate has not seen) is treated as [`StatusCategory::Indeterminate`]
    /// — "something other than new or done" — rather than guessed either way.
    #[must_use]
    pub(crate) fn from_key(key: &str) -> Self {
        match key {
            "new" => StatusCategory::New,
            "done" => StatusCategory::Done,
            _ => StatusCategory::Indeterminate,
        }
    }
}

/// The owned fields of one issue, as last seen, for diffing against the next read.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssueSnapshot {
    pub(crate) title: String,
    pub(crate) body: String,
    pub(crate) category: StatusCategory,
    pub(crate) resolution: Option<String>,
    pub(crate) labels: Vec<String>,
    pub(crate) assignee: Option<String>,
    pub(crate) epic_key: Option<String>,
    pub(crate) updated: JiraTimestamp,
}

/// The owned fields of one epic, as last seen.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EpicSnapshot {
    pub(crate) title: String,
    pub(crate) category: StatusCategory,
}

/// Everything remembered about one project's sync.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectState {
    /// The incremental cursor: the latest `updated` minute seen so far, already converted into
    /// the searching account's own time zone and floored to the minute (see
    /// [`crate::time::account_minute`]) — JQL's own `"YYYY-MM-DD HH:MM"` shape, with no offset of
    /// its own. `None` means the next sync is a first full sync.
    #[serde(default)]
    pub cursor: Option<String>,
    /// Last-seen owned fields, by issue key (`DEMO-12`).
    #[serde(default)]
    pub issue_snapshots: BTreeMap<String, IssueSnapshot>,
    /// Last-seen owned fields, by epic key.
    #[serde(default)]
    pub epic_snapshots: BTreeMap<String, EpicSnapshot>,
    /// Consecutive rate-limit hits with no `Retry-After` for this project, for exponential
    /// backoff. Reset to zero on any request that is not rate-limited.
    #[serde(default)]
    pub secondary_backoff_attempts: u32,
}

/// Cursors and snapshots for every tracked project, plus the account time zone read once from
/// `/myself`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncState {
    /// The account's time zone (`timeZone` from `/myself`), read once and cached: re-reading it
    /// on every call would be one more request for a value that essentially never changes.
    #[serde(default)]
    pub timezone: Option<String>,
    /// By project key.
    #[serde(default)]
    pub projects: BTreeMap<String, ProjectState>,
}

impl SyncState {
    /// An empty state: every project will do a first full sync, and the time zone will be read.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}
