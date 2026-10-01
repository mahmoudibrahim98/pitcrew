//! [`UpstreamChange`]: what changed upstream since the last sync, and the diffing that produces
//! it from a freshly read item and its last snapshot.

use crate::bounds::{MAX_BODY_CHARS, MAX_TITLE_CHARS, cap_chars, cap_labels};
use crate::links::linked_issues;
use crate::state::{CloseReason, IssueSnapshot, MilestoneSnapshot, PullSnapshot};
use crate::time::GithubTimestamp;
use crate::wire::{WireIssue, WireMilestone, WirePullRequest};
use pitcrew_protocol::model::{ExternalRef, ExternalSystem};
use serde::{Deserialize, Serialize};

fn issue_ref(owner_repo: &str, number: u64, html_url: Option<&str>) -> ExternalRef {
    ExternalRef {
        system: ExternalSystem::Github,
        key: format!("{owner_repo}#{number}"),
        url: Some(
            html_url
                .map(str::to_string)
                .unwrap_or_else(|| format!("https://github.com/{owner_repo}/issues/{number}")),
        ),
    }
}

fn pull_ref(owner_repo: &str, number: u64, html_url: Option<&str>) -> ExternalRef {
    ExternalRef {
        system: ExternalSystem::Github,
        key: format!("{owner_repo}#{number}"),
        url: Some(
            html_url
                .map(str::to_string)
                .unwrap_or_else(|| format!("https://github.com/{owner_repo}/pull/{number}")),
        ),
    }
}

fn milestone_ref(owner_repo: &str, number: u64, html_url: Option<&str>) -> ExternalRef {
    ExternalRef {
        system: ExternalSystem::Github,
        key: format!("{owner_repo}#milestone:{number}"),
        url: Some(
            html_url
                .map(str::to_string)
                .unwrap_or_else(|| format!("https://github.com/{owner_repo}/milestone/{number}")),
        ),
    }
}

/// What changed upstream, discovered by one sync call. Each change carries the [`ExternalRef`] it
/// is about and the upstream time it happened.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UpstreamChange {
    /// An issue was opened (or, on a first sync, seen for the first time).
    IssueOpened {
        /// The issue.
        source: ExternalRef,
        /// When.
        at: GithubTimestamp,
        /// Title.
        title: String,
        /// Body.
        body: String,
        /// Labels.
        labels: Vec<String>,
        /// Linked milestone, if any.
        milestone: Option<ExternalRef>,
    },
    /// An issue's title changed.
    IssueRetitled {
        source: ExternalRef,
        at: GithubTimestamp,
        /// The new title.
        title: String,
    },
    /// An issue's body changed.
    IssueBodyEdited {
        source: ExternalRef,
        at: GithubTimestamp,
        /// The new body.
        body: String,
    },
    /// An issue was closed.
    IssueClosed {
        source: ExternalRef,
        at: GithubTimestamp,
        /// Completed, or not planned.
        reason: CloseReason,
    },
    /// A closed issue was reopened.
    IssueReopened {
        source: ExternalRef,
        at: GithubTimestamp,
    },
    /// An issue's labels changed.
    IssueRelabelled {
        source: ExternalRef,
        at: GithubTimestamp,
        /// The new label set.
        labels: Vec<String>,
    },
    /// An issue's assignees changed. The hub owns the assignee field (see
    /// [`crate::ownership`]), so this is recorded for visibility only; `plan` never acts on it.
    IssueReassigned {
        source: ExternalRef,
        at: GithubTimestamp,
        /// The new assignee logins.
        assignees: Vec<String>,
    },
    /// An issue's milestone changed (set, cleared, or moved to a different milestone).
    IssueMilestoned {
        source: ExternalRef,
        at: GithubTimestamp,
        /// The new milestone, or `None` if cleared.
        milestone: Option<ExternalRef>,
    },
    /// A milestone was created (or seen for the first time on a first sync).
    MilestoneCreated {
        source: ExternalRef,
        at: GithubTimestamp,
        /// Title.
        title: String,
    },
    /// A milestone's title changed.
    MilestoneRenamed {
        source: ExternalRef,
        at: GithubTimestamp,
        /// The new title.
        title: String,
    },
    /// A milestone was closed.
    MilestoneClosed {
        source: ExternalRef,
        at: GithubTimestamp,
    },
    /// A pull request was opened (or seen for the first time on a first sync).
    PullRequestOpened {
        source: ExternalRef,
        at: GithubTimestamp,
        /// Title.
        title: String,
    },
    /// A pull request was merged.
    PullRequestMerged {
        source: ExternalRef,
        at: GithubTimestamp,
        /// Issues its body says it closes (`owner/repo#n`, `#n`, or closing keywords).
        closes: Vec<ExternalRef>,
    },
    /// A pull request was closed without being merged.
    PullRequestClosed {
        source: ExternalRef,
        at: GithubTimestamp,
    },
}

impl UpstreamChange {
    /// The item this change is about.
    #[must_use]
    pub fn source(&self) -> &ExternalRef {
        match self {
            UpstreamChange::IssueOpened { source, .. }
            | UpstreamChange::IssueRetitled { source, .. }
            | UpstreamChange::IssueBodyEdited { source, .. }
            | UpstreamChange::IssueClosed { source, .. }
            | UpstreamChange::IssueReopened { source, .. }
            | UpstreamChange::IssueRelabelled { source, .. }
            | UpstreamChange::IssueReassigned { source, .. }
            | UpstreamChange::IssueMilestoned { source, .. }
            | UpstreamChange::MilestoneCreated { source, .. }
            | UpstreamChange::MilestoneRenamed { source, .. }
            | UpstreamChange::MilestoneClosed { source, .. }
            | UpstreamChange::PullRequestOpened { source, .. }
            | UpstreamChange::PullRequestMerged { source, .. }
            | UpstreamChange::PullRequestClosed { source, .. } => source,
        }
    }
}

/// Builds the issue snapshot for a freshly read (and bounds-capped) issue.
fn snapshot_of(issue: &WireIssue) -> IssueSnapshot {
    let mut labels: Vec<String> = issue.labels.iter().map(|l| l.name.clone()).collect();
    labels.sort();
    let mut assignees: Vec<String> = issue.assignees.iter().map(|a| a.login.clone()).collect();
    assignees.sort();
    IssueSnapshot {
        title: cap_chars(&issue.title, MAX_TITLE_CHARS),
        body: cap_chars(issue.body.as_deref().unwrap_or(""), MAX_BODY_CHARS),
        open: issue.state != "closed",
        close_reason: (issue.state == "closed").then_some(CloseReason::from_state_reason(
            issue.state_reason.as_deref(),
        )),
        labels: cap_labels(&labels),
        assignees,
        milestone_number: issue.milestone.as_ref().map(|m| m.number),
        updated_at: GithubTimestamp::new(&issue.updated_at),
    }
}

/// Diffs a freshly read issue against its last snapshot (`None` on a first sight), returning the
/// changes found and the new snapshot to store — or `None` if `issue.updated_at` is not
/// well-formed, in which case the whole item is treated as malformed (skipped, and counted by the
/// caller) rather than snapshotted or diffed with a timestamp that can't be trusted as a cursor.
pub(crate) fn diff_issue(
    owner_repo: &str,
    issue: &WireIssue,
    previous: Option<&IssueSnapshot>,
) -> Option<(Vec<UpstreamChange>, IssueSnapshot)> {
    if !GithubTimestamp::new(&issue.updated_at).is_well_formed() {
        return None;
    }
    let next = snapshot_of(issue);
    let source = issue_ref(owner_repo, issue.number, issue.html_url.as_deref());
    let at = next.updated_at.clone();
    let milestone = issue
        .milestone
        .as_ref()
        .map(|m| milestone_ref(owner_repo, m.number, m.html_url.as_deref()));
    let mut changes = Vec::new();

    match previous {
        None => {
            changes.push(UpstreamChange::IssueOpened {
                source: source.clone(),
                at: at.clone(),
                title: next.title.clone(),
                body: next.body.clone(),
                labels: next.labels.clone(),
                milestone: milestone.clone(),
            });
            if !next.open {
                changes.push(UpstreamChange::IssueClosed {
                    source,
                    at,
                    reason: next.close_reason.unwrap_or(CloseReason::Completed),
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
            if prev.open && !next.open {
                changes.push(UpstreamChange::IssueClosed {
                    source: source.clone(),
                    at: at.clone(),
                    reason: next.close_reason.unwrap_or(CloseReason::Completed),
                });
            } else if !prev.open && next.open {
                changes.push(UpstreamChange::IssueReopened {
                    source: source.clone(),
                    at: at.clone(),
                });
            }
            if prev.labels != next.labels {
                changes.push(UpstreamChange::IssueRelabelled {
                    source: source.clone(),
                    at: at.clone(),
                    labels: next.labels.clone(),
                });
            }
            if prev.assignees != next.assignees {
                changes.push(UpstreamChange::IssueReassigned {
                    source: source.clone(),
                    at: at.clone(),
                    assignees: next.assignees.clone(),
                });
            }
            if prev.milestone_number != next.milestone_number {
                changes.push(UpstreamChange::IssueMilestoned {
                    source,
                    at,
                    milestone,
                });
            }
        }
    }
    Some((changes, next))
}

fn pull_snapshot_of(pr: &WirePullRequest) -> PullSnapshot {
    PullSnapshot {
        title: cap_chars(&pr.title, MAX_TITLE_CHARS),
        open: pr.state != "closed",
        merged: pr.merged_at.is_some(),
        updated_at: GithubTimestamp::new(&pr.updated_at),
    }
}

/// Diffs a freshly read pull request against its last snapshot — or `None` if `pr.updated_at` is
/// not well-formed (see [`diff_issue`]).
pub(crate) fn diff_pull(
    owner_repo: &str,
    pr: &WirePullRequest,
    previous: Option<&PullSnapshot>,
) -> Option<(Vec<UpstreamChange>, PullSnapshot)> {
    if !GithubTimestamp::new(&pr.updated_at).is_well_formed() {
        return None;
    }
    let next = pull_snapshot_of(pr);
    let source = pull_ref(owner_repo, pr.number, pr.html_url.as_deref());
    let at = next.updated_at.clone();
    let mut changes = Vec::new();

    let closes = || linked_issues(pr.body.as_deref().unwrap_or(""), owner_repo);
    match previous {
        None => {
            changes.push(UpstreamChange::PullRequestOpened {
                source: source.clone(),
                at: at.clone(),
                title: next.title.clone(),
            });
            if next.merged {
                changes.push(UpstreamChange::PullRequestMerged {
                    source,
                    at,
                    closes: closes(),
                });
            } else if !next.open {
                changes.push(UpstreamChange::PullRequestClosed { source, at });
            }
        }
        Some(prev) => {
            if !prev.merged && next.merged {
                changes.push(UpstreamChange::PullRequestMerged {
                    source,
                    at,
                    closes: closes(),
                });
            } else if prev.open && !next.open && !next.merged {
                changes.push(UpstreamChange::PullRequestClosed { source, at });
            }
        }
    }
    Some((changes, next))
}

fn milestone_snapshot_of(m: &WireMilestone) -> MilestoneSnapshot {
    MilestoneSnapshot {
        title: cap_chars(&m.title, MAX_TITLE_CHARS),
        open: m.state != "closed",
    }
}

/// Diffs a freshly read milestone against its last snapshot. Milestones carry no `updated_at` in
/// the REST payload, so the caller's read time stands in for "when".
pub(crate) fn diff_milestone(
    owner_repo: &str,
    milestone: &WireMilestone,
    previous: Option<&MilestoneSnapshot>,
    at: &GithubTimestamp,
) -> (Vec<UpstreamChange>, MilestoneSnapshot) {
    let next = milestone_snapshot_of(milestone);
    let source = milestone_ref(owner_repo, milestone.number, milestone.html_url.as_deref());
    let mut changes = Vec::new();

    match previous {
        None => {
            changes.push(UpstreamChange::MilestoneCreated {
                source: source.clone(),
                at: at.clone(),
                title: next.title.clone(),
            });
            if !next.open {
                changes.push(UpstreamChange::MilestoneClosed {
                    source,
                    at: at.clone(),
                });
            }
        }
        Some(prev) => {
            if prev.title != next.title {
                changes.push(UpstreamChange::MilestoneRenamed {
                    source: source.clone(),
                    at: at.clone(),
                    title: next.title.clone(),
                });
            }
            if prev.open && !next.open {
                changes.push(UpstreamChange::MilestoneClosed {
                    source,
                    at: at.clone(),
                });
            }
        }
    }
    (changes, next)
}

#[cfg(test)]
mod malformed_timestamp_tests {
    use super::*;

    fn issue(updated_at: &str) -> WireIssue {
        WireIssue {
            number: 1,
            title: "Title".to_string(),
            body: None,
            state: "open".to_string(),
            state_reason: None,
            labels: Vec::new(),
            assignees: Vec::new(),
            milestone: None,
            updated_at: updated_at.to_string(),
            html_url: None,
            pull_request: None,
        }
    }

    fn pull(updated_at: &str) -> WirePullRequest {
        WirePullRequest {
            number: 1,
            title: "Title".to_string(),
            body: None,
            state: "open".to_string(),
            merged_at: None,
            updated_at: updated_at.to_string(),
            html_url: None,
        }
    }

    #[test]
    fn a_well_formed_issue_timestamp_diffs_normally() {
        let result = diff_issue(
            "example-org/demo-repo",
            &issue("2026-01-01T00:00:00Z"),
            None,
        );
        assert!(result.is_some());
    }

    #[test]
    fn a_malformed_issue_timestamp_is_skipped_entirely() {
        assert!(diff_issue("example-org/demo-repo", &issue("not-a-timestamp"), None).is_none());
        assert!(
            diff_issue(
                "example-org/demo-repo",
                &issue("2026-01-01T00:00:00.000Z"),
                None
            )
            .is_none(),
            "fractional seconds are not the shape GitHub actually sends"
        );
    }

    #[test]
    fn a_well_formed_pull_timestamp_diffs_normally() {
        let result = diff_pull("example-org/demo-repo", &pull("2026-01-01T00:00:00Z"), None);
        assert!(result.is_some());
    }

    #[test]
    fn a_malformed_pull_timestamp_is_skipped_entirely() {
        assert!(diff_pull("example-org/demo-repo", &pull(""), None).is_none());
    }
}
