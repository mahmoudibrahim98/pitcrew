//! Jira REST JSON shapes, for the fields this brief reads. Unknown fields are ignored; a resource
//! missing a field we need fails to parse and is counted as malformed by the caller, rather than
//! failing the whole page.
//!
//! Cloud (v3) and Data Center (v2) share this one set of types: the `fields` shape they both
//! return for an issue is the same JSON, down to `description` being either a plain string (v2)
//! or an Atlassian Document Format object (v3) — [`WireFields::description`] is read as a raw
//! [`serde_json::Value`] and resolved to text by [`crate::change::description_text`], which
//! branches on its *shape*, not on which deployment produced it.

use serde::Deserialize;
use serde_json::{Map, Value};

#[derive(Debug, Deserialize)]
pub(crate) struct WireStatusCategory {
    /// `"new"`, `"indeterminate"` or `"done"`.
    pub key: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct WireStatus {
    /// The status category is all this crate maps to a task status (see
    /// [`crate::state::StatusCategory`]); the display name of the specific workflow status
    /// (`"In Review"`, `"Blocked"`, …) is intentionally not read at all — mapping that to
    /// anything would mean guessing at a project's own custom workflow.
    #[serde(rename = "statusCategory")]
    pub status_category: WireStatusCategory,
}

#[derive(Debug, Deserialize)]
pub(crate) struct WireResolution {
    pub name: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct WireIssueType {
    pub name: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct WireUser {
    #[serde(default, rename = "accountId")]
    pub account_id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default, rename = "displayName")]
    pub display_name: Option<String>,
}

impl WireUser {
    /// A stable identifier for this user: Cloud's opaque `accountId`, Data Center's `name`
    /// (username), falling back to the display name if neither is present. Not itself shown to
    /// people; see [`crate::ownership`] — the hub owns the assignee, so this is recorded for
    /// visibility only.
    pub(crate) fn identifier(&self) -> Option<&str> {
        self.account_id
            .as_deref()
            .or(self.name.as_deref())
            .or(self.display_name.as_deref())
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct WireParentRef {
    pub key: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct WireFields {
    pub summary: String,
    /// A plain string (v2), an ADF object (v3), or absent/null. See the module doc.
    #[serde(default)]
    pub description: Option<Value>,
    pub status: WireStatus,
    #[serde(default)]
    pub resolution: Option<WireResolution>,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub assignee: Option<WireUser>,
    /// A sub-task's parent, or — on modern Jira Cloud — the epic a story/task belongs to.
    #[serde(default)]
    pub parent: Option<WireParentRef>,
    pub issuetype: WireIssueType,
    pub updated: String,
    /// Every field this crate did not name above, kept only so [`WireFields::epic_key`] can look
    /// up a Data Center instance's epic-link custom field (its id varies per instance and is
    /// supplied by the caller — see `SyncConfig::epic_link_field`). Nothing else reads this.
    #[serde(flatten)]
    pub(crate) extra: Map<String, Value>,
}

impl WireFields {
    /// The key of the epic this issue belongs to, if any: `fields.parent` when present (modern
    /// Jira Cloud, and sub-tasks on both deployments), otherwise the configured epic-link custom
    /// field (classic Data Center) if it names one.
    pub(crate) fn epic_key(&self, epic_link_field: Option<&str>) -> Option<String> {
        if let Some(parent) = &self.parent {
            return Some(parent.key.clone());
        }
        let field_id = epic_link_field?;
        match self.extra.get(field_id)? {
            Value::String(s) if !s.is_empty() => Some(s.clone()),
            _ => None,
        }
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct WireIssue {
    pub id: String,
    pub key: String,
    pub fields: WireFields,
}

impl WireIssue {
    pub(crate) fn is_epic(&self) -> bool {
        self.fields.issuetype.name.eq_ignore_ascii_case("epic")
    }
}

/// One page of a Jira Cloud (`/rest/api/3/search/jql`) search response.
#[derive(Debug, Deserialize)]
pub(crate) struct CloudSearchPage {
    #[serde(default)]
    pub issues: Vec<Value>,
    #[serde(default, rename = "nextPageToken")]
    pub next_page_token: Option<String>,
}

/// One page of a Jira Data Center (`/rest/api/2/search`) search response.
#[derive(Debug, Deserialize)]
pub(crate) struct DataCenterSearchPage {
    #[serde(default, rename = "startAt")]
    pub start_at: u64,
    #[serde(default)]
    pub total: u64,
    #[serde(default)]
    pub issues: Vec<Value>,
}

/// The slice of `/rest/api/{2,3}/myself` this crate reads: the account's time zone, read once and
/// cached, as the brief asks — see [`crate::sync`].
#[derive(Debug, Deserialize)]
pub(crate) struct WireMyself {
    #[serde(default, rename = "timeZone")]
    pub time_zone: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epic_key_prefers_parent_over_the_custom_field() {
        let fields: WireFields = serde_json::from_value(serde_json::json!({
            "summary": "s",
            "status": {"name": "To Do", "statusCategory": {"key": "new"}},
            "issuetype": {"name": "Story"},
            "updated": "2026-01-01T00:00:00.000+0000",
            "parent": {"key": "DEMO-1"},
            "customfield_10008": "DEMO-9",
        }))
        .expect("parses");
        assert_eq!(
            fields.epic_key(Some("customfield_10008")).as_deref(),
            Some("DEMO-1")
        );
    }

    #[test]
    fn epic_key_falls_back_to_the_configured_custom_field() {
        let fields: WireFields = serde_json::from_value(serde_json::json!({
            "summary": "s",
            "status": {"name": "To Do", "statusCategory": {"key": "new"}},
            "issuetype": {"name": "Story"},
            "updated": "2026-01-01T00:00:00.000+0000",
            "customfield_10008": "DEMO-9",
        }))
        .expect("parses");
        assert_eq!(
            fields.epic_key(Some("customfield_10008")).as_deref(),
            Some("DEMO-9")
        );
        assert_eq!(fields.epic_key(None), None);
    }

    #[test]
    fn user_identifier_prefers_account_id() {
        let user = WireUser {
            account_id: Some("abc123".to_string()),
            name: Some("demo.user".to_string()),
            display_name: Some("Demo User".to_string()),
        };
        assert_eq!(user.identifier(), Some("abc123"));
    }

    #[test]
    fn user_identifier_falls_back_to_name_then_display_name() {
        let user = WireUser {
            account_id: None,
            name: Some("demo.user".to_string()),
            display_name: Some("Demo User".to_string()),
        };
        assert_eq!(user.identifier(), Some("demo.user"));
        let user = WireUser {
            account_id: None,
            name: None,
            display_name: Some("Demo User".to_string()),
        };
        assert_eq!(user.identifier(), Some("Demo User"));
    }
}
