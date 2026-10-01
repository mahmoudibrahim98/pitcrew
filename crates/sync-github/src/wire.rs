//! GitHub REST v3 JSON shapes, for the resources this brief reads. Unknown fields are ignored
//! (we never deny-unknown-fields); a resource that is missing a field we need fails to parse and
//! is counted as malformed by the caller, rather than failing the whole page.

use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub(crate) struct WireUser {
    pub login: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct WireLabel {
    pub name: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct WireMilestoneRef {
    pub number: u64,
    #[serde(default)]
    pub html_url: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct WireIssue {
    pub number: u64,
    pub title: String,
    #[serde(default)]
    pub body: Option<String>,
    /// `"open"` or `"closed"`.
    pub state: String,
    /// `"completed"`, `"not_planned"`, `"reopened"`, or absent/null on older issues.
    #[serde(default)]
    pub state_reason: Option<String>,
    #[serde(default)]
    pub labels: Vec<WireLabel>,
    #[serde(default)]
    pub assignees: Vec<WireUser>,
    #[serde(default)]
    pub milestone: Option<WireMilestoneRef>,
    pub updated_at: String,
    #[serde(default)]
    pub html_url: Option<String>,
    /// Present (any value) when this "issue" returned by the issues endpoint is actually a pull
    /// request. The issues endpoint lists both; this crate's issue sync skips these.
    #[serde(default)]
    pub pull_request: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct WirePullRequest {
    pub number: u64,
    pub title: String,
    #[serde(default)]
    pub body: Option<String>,
    /// `"open"` or `"closed"` (GitHub never reports `"merged"` as a state; merges are detected
    /// through `merged_at`).
    pub state: String,
    #[serde(default)]
    pub merged_at: Option<String>,
    pub updated_at: String,
    #[serde(default)]
    pub html_url: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct WireMilestone {
    pub number: u64,
    pub title: String,
    /// `"open"` or `"closed"`.
    pub state: String,
    #[serde(default)]
    pub html_url: Option<String>,
}
