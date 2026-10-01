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
    /// The incremental cursor: the latest `updated` instant seen so far, as RFC 3339 text (e.g.
    /// `"2026-01-01T13:04:00Z"`, `jiff::Timestamp`'s own `Display`/`FromStr` shape). Stored as a
    /// real instant, not pre-rendered into the account's local time: the account's Jira profile
    /// zone can change between syncs, and a cursor rendered under a *stale* cached zone would
    /// silently misplace itself relative to a freshly-read zone — see `crate::sync`, which reads
    /// `/myself` on every call for exactly this reason and renders this cursor (via
    /// [`crate::time::account_minute`]) only at the point it builds the next query, never before.
    /// `None` means the next sync is a first full sync.
    ///
    /// An older format stored this pre-rendered instead, as a bare `"YYYY-MM-DD HH:MM"` local-time
    /// string with no zone of its own attached. Such a value cannot be safely reinterpreted after
    /// the fact — this crate no longer knows which zone it was rendered in, and any guess could
    /// place the reconstructed instant *later* than the true one, reintroducing the skip this
    /// field exists to prevent. `crate::sync` treats any stored string that does not parse as an
    /// RFC 3339 instant as `None`, starting one fresh full sync for that project rather than risk
    /// it — see its tests for the old-format case.
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

/// Cursors and snapshots for every tracked project, plus the account time zone most recently read
/// from `/myself`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncState {
    /// The account's time zone (`timeZone` from `/myself`). `crate::sync` re-reads this on every
    /// call — a Jira account can change its own profile zone between syncs, and caching it
    /// indefinitely would silently mis-render every cursor built afterwards — but still keeps the
    /// last successfully-read value here, so a transient failure to reach `/myself` on one call
    /// falls back to the last known-good zone rather than immediately degrading to the (safe, but
    /// much less precise) UTC-12 fallback. `None` only when `/myself` has never once succeeded.
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
