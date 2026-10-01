//! [`UpstreamChange`]: what changed upstream since the last sync, and the diffing that produces
//! it from a freshly read item and its last snapshot. The shape mirrors
//! `pitcrew_sync_github::UpstreamChange` closely — same idea, Jira's own fields.

use crate::adf::adf_to_text;
use crate::bounds::{MAX_BODY_CHARS, MAX_TITLE_CHARS, cap_chars, cap_labels};
use crate::state::{EpicSnapshot, IssueSnapshot, StatusCategory};
use crate::time::JiraTimestamp;
use crate::wire::WireIssue;
use pitcrew_protocol::model::{ExternalRef, ExternalSystem};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Builds a reference to one Jira item (issue or epic — both are issues at the wire level) by
/// key, with its browsable URL. Jira's REST responses carry no browsable URL field of their own
/// (unlike GitHub's `html_url`), so this builds one from `site_base`.
fn item_ref(site_base: &str, key: &str) -> ExternalRef {
    ExternalRef {
        system: ExternalSystem::Jira,
        key: key.to_string(),
        url: Some(format!("{site_base}/browse/{key}")),
    }
}

/// What changed upstream, discovered by one sync call. Each change carries the [`ExternalRef`] it
/// is about and the upstream time it happened.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UpstreamChange {
    /// An issue was created (or, on a first sync, seen for the first time).
    IssueCreated {
        source: ExternalRef,
        at: JiraTimestamp,
        title: String,
        body: String,
        labels: Vec<String>,
        /// The epic this issue belongs to, if any.
        epic: Option<ExternalRef>,
    },
    /// An issue's summary changed.
    IssueRetitled {
        source: ExternalRef,
        at: JiraTimestamp,
        title: String,
    },
    /// An issue's description changed.
    IssueBodyEdited {
        source: ExternalRef,
        at: JiraTimestamp,
        body: String,
    },
    /// An issue's status category moved to `done` (or, on a first sync, was already `done`).
    /// Mirrors `pitcrew_sync_github::UpstreamChange::IssueClosed`: emitted only on a genuine
    /// transition into `done`, never on a move between `new` and `indeterminate` (Jira's many
    /// custom "in progress"-shaped statuses are not something sync needs visibility into).
    IssueDone {
        source: ExternalRef,
        at: JiraTimestamp,
        /// The resolution name Jira reports alongside the move, if any.
        resolution: Option<String>,
    },
    /// An issue's status category moved *out of* `done`, back to `new` or `indeterminate`.
    /// Mirrors `pitcrew_sync_github::UpstreamChange::IssueReopened`: emitted only on a genuine
    /// transition out of `done` (never derived from the hub's own task status — see
    /// [`crate::ownership::plan`] for why that distinction matters).
    IssueReopened {
        source: ExternalRef,
        at: JiraTimestamp,
    },
    /// An issue's labels changed.
    IssueRelabelled {
        source: ExternalRef,
        at: JiraTimestamp,
        labels: Vec<String>,
    },
    /// An issue's assignee changed. The hub owns the assignee field (see [`crate::ownership`]), so
    /// this is recorded for visibility only; `plan` never acts on it.
    IssueReassigned {
        source: ExternalRef,
        at: JiraTimestamp,
        assignee: Option<String>,
    },
    /// An issue's epic (or, for a sub-task, its parent issue) changed.
    IssueReparented {
        source: ExternalRef,
        at: JiraTimestamp,
        epic: Option<ExternalRef>,
    },
    /// An epic was created (or seen for the first time on a first sync).
    EpicCreated {
        source: ExternalRef,
        at: JiraTimestamp,
        title: String,
    },
    /// An epic's summary changed.
    EpicRenamed {
        source: ExternalRef,
        at: JiraTimestamp,
        title: String,
    },
    /// An epic's status category moved to `done`.
    EpicClosed {
        source: ExternalRef,
        at: JiraTimestamp,
    },
}

impl UpstreamChange {
    /// The item this change is about.
    #[must_use]
    pub fn source(&self) -> &ExternalRef {
        match self {
            UpstreamChange::IssueCreated { source, .. }
            | UpstreamChange::IssueRetitled { source, .. }
            | UpstreamChange::IssueBodyEdited { source, .. }
            | UpstreamChange::IssueDone { source, .. }
            | UpstreamChange::IssueReopened { source, .. }
            | UpstreamChange::IssueRelabelled { source, .. }
            | UpstreamChange::IssueReassigned { source, .. }
            | UpstreamChange::IssueReparented { source, .. }
            | UpstreamChange::EpicCreated { source, .. }
            | UpstreamChange::EpicRenamed { source, .. }
            | UpstreamChange::EpicClosed { source, .. } => source,
        }
    }
}

/// Resolves a `fields.description` value to plain text, branching on its *shape* rather than on
/// which deployment sent it (v2 sends a plain string; v3 sends an ADF object; either can be
/// absent or `null`). Bounded to [`MAX_BODY_CHARS`] either way.
pub(crate) fn description_text(description: &Option<Value>) -> String {
    match description {
        Some(Value::String(s)) => cap_chars(s, MAX_BODY_CHARS),
        Some(doc @ Value::Object(_)) => adf_to_text(doc),
        _ => String::new(),
    }
}

fn snapshot_of(issue: &WireIssue, epic_link_field: Option<&str>) -> IssueSnapshot {
    let mut labels = issue.fields.labels.clone();
    labels.sort();
    IssueSnapshot {
        title: cap_chars(&issue.fields.summary, MAX_TITLE_CHARS),
        body: description_text(&issue.fields.description),
        category: StatusCategory::from_key(&issue.fields.status.status_category.key),
        resolution: issue.fields.resolution.as_ref().map(|r| r.name.clone()),
        labels: cap_labels(&labels),
        assignee: issue
            .fields
            .assignee
            .as_ref()
            .and_then(|a| a.identifier())
            .map(str::to_string),
        epic_key: issue.fields.epic_key(epic_link_field),
        updated: JiraTimestamp::new(&issue.fields.updated),
    }
}

/// Diffs a freshly read issue against its last snapshot (`None` on a first sight), returning the
/// changes found and the new snapshot to store — or `None` if `issue.fields.updated` is not
/// well-formed, in which case the whole item is treated as malformed (skipped, and counted by the
/// caller) rather than snapshotted or diffed with a timestamp that can't be trusted as a cursor.
pub(crate) fn diff_issue(
    site_base: &str,
    issue: &WireIssue,
    previous: Option<&IssueSnapshot>,
    epic_link_field: Option<&str>,
) -> Option<(Vec<UpstreamChange>, IssueSnapshot)> {
    if !JiraTimestamp::new(&issue.fields.updated).is_well_formed() {
        return None;
    }
    let next = snapshot_of(issue, epic_link_field);
    let source = item_ref(site_base, &issue.key);
    let at = next.updated.clone();
    let epic = next.epic_key.as_deref().map(|k| item_ref(site_base, k));
    let mut changes = Vec::new();

    match previous {
        None => {
            changes.push(UpstreamChange::IssueCreated {
                source: source.clone(),
                at: at.clone(),
                title: next.title.clone(),
                body: next.body.clone(),
                labels: next.labels.clone(),
                epic: epic.clone(),
            });
            if next.category == StatusCategory::Done {
                changes.push(UpstreamChange::IssueDone {
                    source,
                    at,
                    resolution: next.resolution.clone(),
                });
            }
        }
        Some(prev) => {
            if prev.title != next.title {
                changes.push(UpstreamChange::IssueRetitled {
                    source: source.clone(),
                    at: at.clone(),
                    title: next.title.clone(),
                });
            }
            if prev.body != next.body {
                changes.push(UpstreamChange::IssueBodyEdited {
                    source: source.clone(),
                    at: at.clone(),
                    body: next.body.clone(),
                });
            }
            if prev.category != StatusCategory::Done && next.category == StatusCategory::Done {
                changes.push(UpstreamChange::IssueDone {
                    source: source.clone(),
                    at: at.clone(),
                    resolution: next.resolution.clone(),
                });
            } else if prev.category == StatusCategory::Done && next.category != StatusCategory::Done
            {
                changes.push(UpstreamChange::IssueReopened {
                    source: source.clone(),
                    at: at.clone(),
                });
            }
            // A move between `new` and `indeterminate` (neither side `done`) is not reported at
            // all — see `UpstreamChange::IssueDone`'s doc.
            if prev.labels != next.labels {
                changes.push(UpstreamChange::IssueRelabelled {
                    source: source.clone(),
                    at: at.clone(),
                    labels: next.labels.clone(),
                });
            }
            if prev.assignee != next.assignee {
                changes.push(UpstreamChange::IssueReassigned {
                    source: source.clone(),
                    at: at.clone(),
                    assignee: next.assignee.clone(),
                });
            }
            if prev.epic_key != next.epic_key {
                changes.push(UpstreamChange::IssueReparented { source, at, epic });
            }
        }
    }
    Some((changes, next))
}

fn epic_snapshot_of(issue: &WireIssue) -> EpicSnapshot {
    EpicSnapshot {
        title: cap_chars(&issue.fields.summary, MAX_TITLE_CHARS),
        category: StatusCategory::from_key(&issue.fields.status.status_category.key),
    }
}

/// Diffs a freshly read epic against its last snapshot — or `None` if not well-formed (see
/// [`diff_issue`]).
pub(crate) fn diff_epic(
    site_base: &str,
    issue: &WireIssue,
    previous: Option<&EpicSnapshot>,
) -> Option<(Vec<UpstreamChange>, EpicSnapshot)> {
    if !JiraTimestamp::new(&issue.fields.updated).is_well_formed() {
        return None;
    }
    let next = epic_snapshot_of(issue);
    let source = item_ref(site_base, &issue.key);
    let at = JiraTimestamp::new(&issue.fields.updated);
    let mut changes = Vec::new();

    match previous {
        None => {
            changes.push(UpstreamChange::EpicCreated {
                source: source.clone(),
                at: at.clone(),
                title: next.title.clone(),
            });
            if next.category == StatusCategory::Done {
                changes.push(UpstreamChange::EpicClosed { source, at });
            }
        }
        Some(prev) => {
            if prev.title != next.title {
                changes.push(UpstreamChange::EpicRenamed {
                    source: source.clone(),
                    at: at.clone(),
                    title: next.title.clone(),
                });
            }
            if prev.category != StatusCategory::Done && next.category == StatusCategory::Done {
                changes.push(UpstreamChange::EpicClosed { source, at });
            }
        }
    }
    Some((changes, next))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn issue(updated: &str, category: &str) -> WireIssue {
        serde_json::from_value(json!({
            "id": "1",
            "key": "DEMO-1",
            "fields": {
                "summary": "Title",
                "status": {"name": "To Do", "statusCategory": {"key": category}},
                "issuetype": {"name": "Story"},
                "updated": updated,
            }
        }))
        .expect("valid wire issue")
    }

    #[test]
    fn a_well_formed_issue_timestamp_diffs_normally() {
        let result = diff_issue(
            "https://jira.example.com",
            &issue("2026-01-01T00:00:00.000+0000", "new"),
            None,
            None,
        );
        assert!(result.is_some());
    }

    #[test]
    fn a_malformed_issue_timestamp_is_skipped_entirely() {
        assert!(
            diff_issue(
                "https://jira.example.com",
                &issue("not-a-timestamp", "new"),
                None,
                None
            )
            .is_none()
        );
    }

    #[test]
    fn description_text_handles_both_shapes_and_absence() {
        assert_eq!(description_text(&None), "");
        assert_eq!(
            description_text(&Some(Value::String("plain v2 text".to_string()))),
            "plain v2 text"
        );
        let adf = json!({"type": "doc", "content": [{"type": "paragraph", "content": [{"type": "text", "text": "v3 text"}]}]});
        assert!(description_text(&Some(adf)).contains("v3 text"));
    }

    #[test]
    fn first_sight_already_done_emits_created_then_done() {
        let (changes, snap) = diff_issue(
            "https://jira.example.com",
            &issue("2026-01-01T00:00:00.000+0000", "done"),
            None,
            None,
        )
        .expect("well-formed");
        assert!(matches!(changes[0], UpstreamChange::IssueCreated { .. }));
        assert!(matches!(changes[1], UpstreamChange::IssueDone { .. }));
        assert_eq!(snap.category, StatusCategory::Done);
    }

    fn snapshot_with_category(category: &str) -> IssueSnapshot {
        let (_, snap) = diff_issue(
            "https://jira.example.com",
            &issue("2026-01-01T00:00:00.000+0000", category),
            None,
            None,
        )
        .expect("well-formed");
        snap
    }

    #[test]
    fn a_move_to_done_emits_issue_done() {
        let prev = snapshot_with_category("new");
        let (changes, _) = diff_issue(
            "https://jira.example.com",
            &issue("2026-01-02T00:00:00.000+0000", "done"),
            Some(&prev),
            None,
        )
        .expect("well-formed");
        assert_eq!(changes.len(), 1, "{changes:#?}");
        assert!(matches!(changes[0], UpstreamChange::IssueDone { .. }));
    }

    #[test]
    fn a_move_out_of_done_emits_issue_reopened() {
        let prev = snapshot_with_category("done");
        let (changes, _) = diff_issue(
            "https://jira.example.com",
            &issue("2026-01-02T00:00:00.000+0000", "new"),
            Some(&prev),
            None,
        )
        .expect("well-formed");
        assert_eq!(changes.len(), 1, "{changes:#?}");
        assert!(matches!(changes[0], UpstreamChange::IssueReopened { .. }));
    }

    #[test]
    fn a_move_between_new_and_indeterminate_emits_nothing() {
        let prev = snapshot_with_category("new");
        let (changes, _) = diff_issue(
            "https://jira.example.com",
            &issue("2026-01-02T00:00:00.000+0000", "indeterminate"),
            Some(&prev),
            None,
        )
        .expect("well-formed");
        assert!(changes.is_empty(), "{changes:#?}");
    }
}
