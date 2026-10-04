//! [`UpstreamChange`]: what changed upstream since the last sync, and the diffing that produces
//! it from a freshly read item and its last snapshot. The shape mirrors
//! `pitcrew_sync_github::UpstreamChange` closely — same idea, Jira's own fields.

use crate::adf::adf_to_text;
use crate::bounds::{MAX_BODY_CHARS, MAX_TITLE_CHARS, cap_chars, cap_labels, strip_hidden};
use crate::state::{EpicSnapshot, IssueSnapshot, StatusCategory, SyncState};
use crate::time::JiraTimestamp;
use crate::wire::{WireIssue, looks_like_issue_key};
use pitcrew_protocol::model::{ExternalRef, ExternalSystem};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Builds a reference to one Jira item (issue or epic — both are issues at the wire level) by
/// key, with its browsable URL. Jira's REST responses carry no browsable URL field of their own
/// (unlike GitHub's `html_url`), so this builds one from `site_base`. `key` is server-supplied —
/// `site_base` itself is this crate's own trusted configuration, never server data.
///
/// `key` is *validated* against [`looks_like_issue_key`], not sanitised (round 3 review item S-4,
/// correcting round 2's R10 fix, which stripped hidden characters out of the key instead): a
/// zero-width character stripped out of `DEMO-1\u{200B}2` would silently turn it into the
/// unrelated, real issue `DEMO-12`, and a key is never free-form enough to need escaping rather
/// than rejecting outright — an un-stripped `DEMO-1/../../secure/Logout.jspa` spliced into
/// `{site_base}/browse/{key}` would hand an attacker a chosen path under the site's own origin.
/// Returns `None` for a key that does not validate; the caller treats that as this reference being
/// unsafe to build — see each call site for whether that fails the whole item or just drops this
/// one reference.
fn item_ref(site_base: &str, key: &str) -> Option<ExternalRef> {
    if !looks_like_issue_key(key) {
        return None;
    }
    Some(ExternalRef {
        system: ExternalSystem::Jira,
        url: Some(format!("{site_base}/browse/{key}")),
        key: key.to_string(),
    })
}

impl SyncState {
    /// The issue `source` (`DEMO-12`), not done, as a first read would report it, built from its
    /// last snapshot: an [`UpstreamChange::IssueCreated`] in `epic`. For a caller that starts
    /// mirroring an issue it did not mirror before, because a later read moved it under an epic
    /// the caller follows ([`UpstreamChange::IssueReparented`]): the move alone carries none of
    /// the issue's fields. `None` when this state has no snapshot of the issue, or it is done.
    ///
    /// Call it on the state a sync returned, so the snapshot is the one that read just took.
    #[must_use]
    pub fn created_from_snapshot(
        &self,
        source: &ExternalRef,
        epic: Option<&ExternalRef>,
    ) -> Option<UpstreamChange> {
        let (project, _) = source.key.split_once('-')?;
        let snapshot = self
            .projects
            .get(project)?
            .issue_snapshots
            .get(&source.key)?;
        (snapshot.category != StatusCategory::Done).then(|| UpstreamChange::IssueCreated {
            source: source.clone(),
            at: snapshot.updated.clone(),
            title: snapshot.title.clone(),
            body: snapshot.body.clone(),
            labels: snapshot.labels.clone(),
            epic: epic.cloned(),
        })
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
        // Resolution and assignee names are short "names" in the sense R10 means (round 2
        // review): never length-capped (they're already bounded by Jira's own field shapes), but
        // still hidden-character-stripped, the same reasoning as GitHub's assignee logins.
        resolution: issue
            .fields
            .resolution
            .as_ref()
            .map(|r| strip_hidden(&r.name)),
        labels: cap_labels(&labels),
        assignee: issue
            .fields
            .assignee
            .as_ref()
            .and_then(|a| a.identifier())
            .map(strip_hidden),
        epic_key: issue.fields.epic_key(epic_link_field),
        updated: JiraTimestamp::new(&issue.fields.updated),
    }
}

/// Diffs a freshly read issue against its last snapshot (`None` on a first sight), returning the
/// changes found and the new snapshot to store — or `None` if `issue.fields.updated` is not
/// well-formed, or `issue.key` does not validate as a real Jira issue key (round 3 review item
/// S-4), in which case the whole item is treated as malformed (skipped, and counted by the
/// caller) rather than snapshotted or diffed with a source reference that can't be trusted. A
/// referenced epic/parent key that fails to validate only drops that one reference (`epic: None`)
/// rather than failing the whole issue — the issue's own identity is still sound even if a field
/// pointing elsewhere is not.
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
    let source = item_ref(site_base, &issue.key)?;
    let at = next.updated.clone();
    let epic = next
        .epic_key
        .as_deref()
        .and_then(|k| item_ref(site_base, k));
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
    let source = item_ref(site_base, &issue.key)?;
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
    fn an_issue_reparented_later_is_created_from_its_snapshot() {
        let mut wire = issue("2026-01-01T00:00:00.000+0000", "indeterminate");
        wire.key = "DEMO-8".to_string();
        let (_, snapshot) =
            diff_issue("https://jira.example.com", &wire, None, None).expect("well-formed");
        let mut state = SyncState::new();
        state
            .projects
            .entry("DEMO".to_string())
            .or_default()
            .issue_snapshots
            .insert("DEMO-8".to_string(), snapshot.clone());
        let source = item_ref("https://jira.example.com", "DEMO-8").unwrap();
        let epic = item_ref("https://jira.example.com", "DEMO-5").unwrap();
        assert_eq!(
            state.created_from_snapshot(&source, Some(&epic)),
            Some(UpstreamChange::IssueCreated {
                source: source.clone(),
                at: JiraTimestamp::new("2026-01-01T00:00:00.000+0000"),
                title: "Title".to_string(),
                body: String::new(),
                labels: Vec::new(),
                epic: Some(epic.clone()),
            })
        );
        // Unknown issues, and done ones, give nothing.
        let unknown = item_ref("https://jira.example.com", "DEMO-9").unwrap();
        assert_eq!(state.created_from_snapshot(&unknown, None), None);
        let other_project = item_ref("https://jira.example.com", "OTHER-8").unwrap();
        assert_eq!(state.created_from_snapshot(&other_project, None), None);
        let done = IssueSnapshot {
            category: StatusCategory::Done,
            ..snapshot
        };
        state
            .projects
            .get_mut("DEMO")
            .unwrap()
            .issue_snapshots
            .insert("DEMO-8".to_string(), done);
        assert_eq!(state.created_from_snapshot(&source, Some(&epic)), None);
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
    fn an_invalid_issue_key_is_malformed_not_silently_fixed_up() {
        // Round 3 review item S-4, correcting round 2's R10 fix: a key is *validated*, never
        // stripped and carried on with — stripping a hidden character out of a key could silently
        // turn it into a different, real issue's key, and a path-traversal payload in the numeric
        // part must never reach the browse URL `item_ref` builds.
        for bad_key in [
            "DEMO\u{202E}-1",
            "DEMO-1\u{200B}2",
            "DEMO-1/../../secure/Logout.jspa",
        ] {
            let mut spoofed = issue("2026-01-01T00:00:00.000+0000", "new");
            spoofed.key = bad_key.to_string();
            assert!(
                diff_issue("https://jira.example.com", &spoofed, None, None).is_none(),
                "{bad_key:?} must not diff as a well-formed item"
            );
        }
    }

    #[test]
    fn a_well_formed_issue_key_builds_the_expected_ref_and_browse_url() {
        let ok = issue("2026-01-01T00:00:00.000+0000", "new");
        let (changes, _snapshot) =
            diff_issue("https://jira.example.com", &ok, None, None).expect("well-formed");
        let source = changes[0].source();
        assert_eq!(source.key, "DEMO-1");
        assert_eq!(
            source.url.as_deref(),
            Some("https://jira.example.com/browse/DEMO-1")
        );
    }

    #[test]
    fn an_invalid_epic_reference_key_drops_only_that_reference() {
        // A sub-task's/issue's own key is sound, but its `fields.parent.key` (the epic reference)
        // does not validate: the issue itself is still processed, just with no epic reference.
        let mut issue_with_bad_parent = issue("2026-01-01T00:00:00.000+0000", "new");
        issue_with_bad_parent.fields.parent = Some(crate::wire::WireParentRef {
            key: "../../evil".to_string(),
        });
        let (changes, snapshot) = diff_issue(
            "https://jira.example.com",
            &issue_with_bad_parent,
            None,
            None,
        )
        .expect("the issue's own key is still well-formed");
        assert_eq!(snapshot.epic_key.as_deref(), Some("../../evil"));
        match &changes[0] {
            UpstreamChange::IssueCreated { epic, .. } => {
                assert!(
                    epic.is_none(),
                    "an invalid epic reference must not be built"
                );
            }
            other => panic!("expected IssueCreated: {other:?}"),
        }
    }

    #[test]
    fn hidden_characters_in_resolution_and_assignee_are_stripped() {
        let mut issue = issue("2026-01-01T00:00:00.000+0000", "done");
        issue.fields.resolution = Some(crate::wire::WireResolution {
            name: "Fixed\u{200B}".to_string(),
        });
        issue.fields.assignee = Some(crate::wire::WireUser {
            account_id: Some("ab\u{202E}cd".to_string()),
            name: None,
            display_name: None,
        });
        let (_changes, snapshot) =
            diff_issue("https://jira.example.com", &issue, None, None).expect("well-formed");
        assert_eq!(snapshot.resolution.as_deref(), Some("Fixed"));
        assert_eq!(snapshot.assignee.as_deref(), Some("abcd"));
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
