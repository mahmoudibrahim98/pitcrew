//! [`UpstreamChange`]: what changed upstream since the last sync, and the diffing that produces
//! it from a freshly read item and its last snapshot.

use crate::bounds::{
    MAX_BODY_CHARS, MAX_TITLE_CHARS, cap_chars, cap_labels, contains_hidden, strip_hidden,
};
use crate::links::linked_issues;
use crate::state::{CloseReason, IssueSnapshot, MilestoneSnapshot, PullSnapshot};
use crate::time::GithubTimestamp;
use crate::wire::{WireIssue, WireMilestone, WirePullRequest};
use pitcrew_protocol::model::{ExternalRef, ExternalSystem};
use serde::{Deserialize, Serialize};

/// The web host an `html_url` must match to be trusted (round 3 review item S-5). GitHub.com's
/// API and its web UI are on *different* hosts (`api.github.com` vs `github.com`), so this is
/// derived from, not simply copied from, the configured `api_base`: `None` (the default, talking
/// to `api.github.com`) means the web host is `github.com`. `Some(api_base)` (GitHub Enterprise
/// Server) means the web host is GHES's own host — its web UI and its API share one host, just
/// different paths — found by parsing `api_base`. A wholly malformed `api_base` (this crate's own
/// configuration, never server data) falls back to its raw text, which then simply never matches
/// any real `html_url` host, rejecting every one rather than trusting an unintended one.
pub(crate) fn expected_web_host(api_base: Option<&str>) -> String {
    match api_base {
        None => "github.com".to_string(),
        Some(base) => url::Url::parse(base)
            .ok()
            .and_then(|u| u.host_str().map(str::to_string))
            .unwrap_or_else(|| base.to_string()),
    }
}

/// Whether `raw` is safe to carry verbatim into an [`ExternalRef`], returning its *parsed* form
/// (round 3 review item S-5: the caller stores that, `parsed.as_str()`, not `raw` itself, so the
/// check and what gets kept can never diverge).
///
/// Trusted means: it parses; its scheme is `https` (GitHub.com and every GitHub Enterprise Server
/// deployment this crate has seen use TLS — a deliberate decision, not an oversight; plain `http`
/// is rejected too — round 2 review item R10); its host is *exactly* `expected_web_host` (S-5:
/// round 2's fix checked only the scheme, so a server could still send an `html_url` on any other
/// `https` host entirely — this pins it to the one web origin this sync is actually about, the
/// same reasoning `origin::trusted_next_url` uses for the API host); and it contains no hidden or
/// direction-changing character (S-5 again — unlike title/body/label text, a URL is not safe to
/// silently *edit*: dropping a character out of it can change what it points to without that
/// being obvious, so one found in a URL means refusing the whole value, not stripping it).
fn trusted_html_url(raw: &str, expected_web_host: &str) -> Option<url::Url> {
    if contains_hidden(raw) {
        return None;
    }
    let parsed = url::Url::parse(raw).ok()?;
    (parsed.scheme() == "https" && parsed.host_str() == Some(expected_web_host)).then_some(parsed)
}

/// Resolves `html_url` into the URL an [`ExternalRef`] should carry: the server's own value,
/// parsed (see [`trusted_html_url`]), when it is trusted, the crate's own constructed fallback
/// otherwise (also used when the server sent none at all). Increments `*malformed_fields` only
/// for the "sent but rejected" case — an absent `html_url` is normal, not malformed.
fn resolved_url(
    html_url: Option<&str>,
    expected_web_host: &str,
    fallback: impl FnOnce() -> String,
    malformed_fields: &mut u32,
) -> String {
    match html_url.and_then(|u| trusted_html_url(u, expected_web_host)) {
        Some(parsed) => parsed.as_str().to_string(),
        None if html_url.is_some() => {
            *malformed_fields += 1;
            fallback()
        }
        None => fallback(),
    }
}

fn issue_ref(
    owner_repo: &str,
    number: u64,
    html_url: Option<&str>,
    expected_web_host: &str,
    malformed_fields: &mut u32,
) -> ExternalRef {
    ExternalRef {
        system: ExternalSystem::Github,
        key: format!("{owner_repo}#{number}"),
        url: Some(resolved_url(
            html_url,
            expected_web_host,
            || format!("https://github.com/{owner_repo}/issues/{number}"),
            malformed_fields,
        )),
    }
}

fn pull_ref(
    owner_repo: &str,
    number: u64,
    html_url: Option<&str>,
    expected_web_host: &str,
    malformed_fields: &mut u32,
) -> ExternalRef {
    ExternalRef {
        system: ExternalSystem::Github,
        key: format!("{owner_repo}#{number}"),
        url: Some(resolved_url(
            html_url,
            expected_web_host,
            || format!("https://github.com/{owner_repo}/pull/{number}"),
            malformed_fields,
        )),
    }
}

fn milestone_ref(
    owner_repo: &str,
    number: u64,
    html_url: Option<&str>,
    expected_web_host: &str,
    malformed_fields: &mut u32,
) -> ExternalRef {
    ExternalRef {
        system: ExternalSystem::Github,
        key: format!("{owner_repo}#milestone:{number}"),
        url: Some(resolved_url(
            html_url,
            expected_web_host,
            || format!("https://github.com/{owner_repo}/milestone/{number}"),
            malformed_fields,
        )),
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
    // Logins are short, bounded by GitHub's own username rules, and never shown verbatim as a
    // long body would be — no length cap needed — but still worth hidden-character-stripping
    // (R10: "names"), since a login is exactly the kind of short text a bidi override could make
    // misleading in a UI list.
    let mut assignees: Vec<String> = issue
        .assignees
        .iter()
        .map(|a| strip_hidden(&a.login))
        .collect();
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
/// `*malformed_fields` is incremented once for each of `issue`'s and its milestone's `html_url`
/// that was present but rejected (an untrusted scheme or host, or a hidden character — R10, S-5)
/// and silently replaced by this crate's own constructed URL; the item itself is still processed
/// normally either way.
pub(crate) fn diff_issue(
    owner_repo: &str,
    issue: &WireIssue,
    previous: Option<&IssueSnapshot>,
    expected_web_host: &str,
    malformed_fields: &mut u32,
) -> Option<(Vec<UpstreamChange>, IssueSnapshot)> {
    if !GithubTimestamp::new(&issue.updated_at).is_well_formed() {
        return None;
    }
    let next = snapshot_of(issue);
    let source = issue_ref(
        owner_repo,
        issue.number,
        issue.html_url.as_deref(),
        expected_web_host,
        malformed_fields,
    );
    let at = next.updated_at.clone();
    let milestone = issue.milestone.as_ref().map(|m| {
        milestone_ref(
            owner_repo,
            m.number,
            m.html_url.as_deref(),
            expected_web_host,
            malformed_fields,
        )
    });
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
/// not well-formed (see [`diff_issue`]). `*malformed_fields` is incremented as in [`diff_issue`].
pub(crate) fn diff_pull(
    owner_repo: &str,
    pr: &WirePullRequest,
    previous: Option<&PullSnapshot>,
    expected_web_host: &str,
    malformed_fields: &mut u32,
) -> Option<(Vec<UpstreamChange>, PullSnapshot)> {
    if !GithubTimestamp::new(&pr.updated_at).is_well_formed() {
        return None;
    }
    let next = pull_snapshot_of(pr);
    let source = pull_ref(
        owner_repo,
        pr.number,
        pr.html_url.as_deref(),
        expected_web_host,
        malformed_fields,
    );
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
/// the REST payload, so the caller's read time stands in for "when". `*malformed_fields` is
/// incremented as in [`diff_issue`].
pub(crate) fn diff_milestone(
    owner_repo: &str,
    milestone: &WireMilestone,
    previous: Option<&MilestoneSnapshot>,
    at: &GithubTimestamp,
    expected_web_host: &str,
    malformed_fields: &mut u32,
) -> (Vec<UpstreamChange>, MilestoneSnapshot) {
    let next = milestone_snapshot_of(milestone);
    let source = milestone_ref(
        owner_repo,
        milestone.number,
        milestone.html_url.as_deref(),
        expected_web_host,
        malformed_fields,
    );
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

    #[test]
    fn expected_web_host_is_github_com_by_default_not_the_api_host() {
        assert_eq!(expected_web_host(None), "github.com");
    }

    #[test]
    fn expected_web_host_is_derived_from_a_ghes_api_base() {
        assert_eq!(
            expected_web_host(Some("https://ghe.example.com/api/v3")),
            "ghe.example.com"
        );
    }

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

    const GITHUB_COM: &str = "github.com";

    #[test]
    fn a_well_formed_issue_timestamp_diffs_normally() {
        let mut malformed_fields = 0u32;
        let result = diff_issue(
            "example-org/demo-repo",
            &issue("2026-01-01T00:00:00Z"),
            None,
            GITHUB_COM,
            &mut malformed_fields,
        );
        assert!(result.is_some());
        assert_eq!(malformed_fields, 0);
    }

    #[test]
    fn a_malformed_issue_timestamp_is_skipped_entirely() {
        let mut malformed_fields = 0u32;
        assert!(
            diff_issue(
                "example-org/demo-repo",
                &issue("not-a-timestamp"),
                None,
                GITHUB_COM,
                &mut malformed_fields
            )
            .is_none()
        );
        assert!(
            diff_issue(
                "example-org/demo-repo",
                &issue("2026-01-01T00:00:00.000Z"),
                None,
                GITHUB_COM,
                &mut malformed_fields
            )
            .is_none(),
            "fractional seconds are not the shape GitHub actually sends"
        );
    }

    #[test]
    fn a_well_formed_pull_timestamp_diffs_normally() {
        let mut malformed_fields = 0u32;
        let result = diff_pull(
            "example-org/demo-repo",
            &pull("2026-01-01T00:00:00Z"),
            None,
            GITHUB_COM,
            &mut malformed_fields,
        );
        assert!(result.is_some());
        assert_eq!(malformed_fields, 0);
    }

    #[test]
    fn a_malformed_pull_timestamp_is_skipped_entirely() {
        let mut malformed_fields = 0u32;
        assert!(
            diff_pull(
                "example-org/demo-repo",
                &pull(""),
                None,
                GITHUB_COM,
                &mut malformed_fields
            )
            .is_none()
        );
    }

    #[test]
    fn an_untrusted_html_url_scheme_is_dropped_and_counted_as_a_malformed_field() {
        let mut malformed_fields = 0u32;
        let mut malicious = issue("2026-01-01T00:00:00Z");
        malicious.html_url = Some("javascript:alert(1)".to_string());
        let (_changes, _snapshot) = diff_issue(
            "example-org/demo-repo",
            &malicious,
            None,
            GITHUB_COM,
            &mut malformed_fields,
        )
        .expect("the item itself is still processed");
        assert_eq!(malformed_fields, 1);
    }

    #[test]
    fn a_plain_http_html_url_is_also_untrusted() {
        let mut malformed_fields = 0u32;
        let mut http_issue = issue("2026-01-01T00:00:00Z");
        http_issue.html_url = Some("http://github.com/example-org/demo-repo/issues/1".to_string());
        diff_issue(
            "example-org/demo-repo",
            &http_issue,
            None,
            GITHUB_COM,
            &mut malformed_fields,
        )
        .expect("the item itself is still processed");
        assert_eq!(malformed_fields, 1);
    }

    #[test]
    fn a_trusted_https_html_url_is_kept_and_not_counted() {
        let mut malformed_fields = 0u32;
        let mut ok_issue = issue("2026-01-01T00:00:00Z");
        ok_issue.html_url = Some("https://github.com/example-org/demo-repo/issues/1".to_string());
        let (changes, _snapshot) = diff_issue(
            "example-org/demo-repo",
            &ok_issue,
            None,
            GITHUB_COM,
            &mut malformed_fields,
        )
        .expect("well-formed");
        assert_eq!(malformed_fields, 0);
        assert_eq!(
            changes[0].source().url.as_deref(),
            Some("https://github.com/example-org/demo-repo/issues/1")
        );
    }

    #[test]
    fn an_html_url_on_an_unexpected_host_is_untrusted() {
        // Round 3 review item S-5: round 2's R10 fix checked only the scheme, so a server could
        // still redirect `html_url` to any other `https` host entirely.
        let mut malformed_fields = 0u32;
        let mut redirected = issue("2026-01-01T00:00:00Z");
        redirected.html_url =
            Some("https://attacker.example/example-org/demo-repo/issues/1".to_string());
        diff_issue(
            "example-org/demo-repo",
            &redirected,
            None,
            GITHUB_COM,
            &mut malformed_fields,
        )
        .expect("the item itself is still processed");
        assert_eq!(malformed_fields, 1);
    }

    #[test]
    fn an_html_url_on_the_ghes_web_host_is_trusted() {
        let mut malformed_fields = 0u32;
        let mut on_ghes = issue("2026-01-01T00:00:00Z");
        on_ghes.html_url =
            Some("https://ghe.example.com/example-org/demo-repo/issues/1".to_string());
        let (changes, _snapshot) = diff_issue(
            "example-org/demo-repo",
            &on_ghes,
            None,
            "ghe.example.com",
            &mut malformed_fields,
        )
        .expect("well-formed");
        assert_eq!(malformed_fields, 0);
        assert_eq!(
            changes[0].source().url.as_deref(),
            Some("https://ghe.example.com/example-org/demo-repo/issues/1")
        );
    }

    #[test]
    fn an_html_url_with_a_hidden_character_is_untrusted() {
        let mut malformed_fields = 0u32;
        let mut spoofed = issue("2026-01-01T00:00:00Z");
        spoofed.html_url =
            Some("https://github.com/example-org/demo-repo\u{200B}/issues/1".to_string());
        diff_issue(
            "example-org/demo-repo",
            &spoofed,
            None,
            GITHUB_COM,
            &mut malformed_fields,
        )
        .expect("the item itself is still processed");
        assert_eq!(malformed_fields, 1);
    }

    #[test]
    fn an_html_url_is_stored_in_its_parsed_form() {
        // Round 3 review item S-2/S-5: the stored URL is `Url::parse(..).as_str()`, not the raw
        // server text, so the check and what is kept can never diverge.
        let mut malformed_fields = 0u32;
        let mut ok_issue = issue("2026-01-01T00:00:00Z");
        // An already-valid URL round-trips unchanged; this mainly documents that the stored value
        // comes from the parser, not a straight copy of `html_url`.
        ok_issue.html_url = Some("https://github.com/example-org/demo-repo/issues/1".to_string());
        let (changes, _snapshot) = diff_issue(
            "example-org/demo-repo",
            &ok_issue,
            None,
            GITHUB_COM,
            &mut malformed_fields,
        )
        .expect("well-formed");
        assert_eq!(
            changes[0].source().url.as_deref(),
            Some("https://github.com/example-org/demo-repo/issues/1")
        );
    }

    #[test]
    fn a_hidden_character_in_an_assignee_login_is_stripped() {
        let mut malformed_fields = 0u32;
        let mut spoofed = issue("2026-01-01T00:00:00Z");
        spoofed.assignees = vec![crate::wire::WireUser {
            login: "ab\u{202E}cd".to_string(),
        }];
        let (_changes, snapshot) = diff_issue(
            "example-org/demo-repo",
            &spoofed,
            None,
            GITHUB_COM,
            &mut malformed_fields,
        )
        .expect("well-formed");
        assert_eq!(snapshot.assignees, vec!["abcd".to_string()]);
    }
}
