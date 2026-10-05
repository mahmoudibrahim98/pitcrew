//! [`UpstreamChange`]: what changed upstream since the last sync, and the diffing that produces
//! it from a freshly read item and its last snapshot.

use crate::bounds::{
    MAX_BODY_CHARS, MAX_KEPT_URL_BYTES, MAX_TITLE_CHARS, cap_chars, cap_labels, contains_hidden,
    strip_hidden,
};
use crate::links::linked_issues;
use crate::state::{CloseReason, IssueSnapshot, MilestoneSnapshot, PullSnapshot, SyncState};
use crate::time::GithubTimestamp;
use crate::wire::{WireIssue, WireMilestone, WirePullRequest};
use pitcrew_protocol::model::{ExternalRef, ExternalSystem};
use serde::{Deserialize, Serialize};

/// The web host and port an `html_url` must match to be trusted (round 3 review item S-5, and its
/// residual O30/R10: the port is pinned too, not just the host). GitHub.com's API and its web UI
/// are on *different* hosts (`api.github.com` vs `github.com`), so this is derived from, not
/// simply copied from, the configured `api_base`: `None` (the default, talking to
/// `api.github.com`) means the web origin is `github.com`, port 443. `Some(api_base)` (GitHub
/// Enterprise Server) means the web UI shares the API's own host *and* port — GHES serves both
/// from the same place, just different paths — found by parsing `api_base`; no explicit port in
/// `api_base` means the scheme's default. A wholly malformed `api_base` (this crate's own
/// configuration, never server data) falls back to its raw text as the host, with port 443, which
/// then simply never matches any real `html_url` host, rejecting every one rather than trusting an
/// unintended one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WebOrigin {
    pub host: String,
    pub port: u16,
}

pub(crate) fn expected_web_origin(api_base: Option<&str>) -> WebOrigin {
    match api_base {
        None => WebOrigin {
            host: "github.com".to_string(),
            port: 443,
        },
        Some(base) => {
            let parsed = url::Url::parse(base).ok();
            WebOrigin {
                host: parsed
                    .as_ref()
                    .and_then(|u| u.host_str())
                    .map(str::to_string)
                    .unwrap_or_else(|| base.to_string()),
                port: parsed
                    .as_ref()
                    .and_then(url::Url::port_or_known_default)
                    .unwrap_or(443),
            }
        }
    }
}

/// Whether `raw` is safe to carry verbatim into an [`ExternalRef`], returning its *parsed* form
/// (round 3 review item S-5: the caller stores that, `parsed.as_str()`, not `raw` itself, so the
/// check and what gets kept can never diverge).
///
/// Trusted means: it is at most [`MAX_KEPT_URL_BYTES`] long and contains no hidden or
/// direction-changing character (round 3 finding O30/R10's residual for the length; S-5 for hidden
/// characters — unlike title/body/label text, a URL is not safe to silently *edit*: dropping a
/// character out of it can change what it points to without that being obvious, so one found in a
/// URL means refusing the whole value, not stripping it); it parses, with no userinfo (O30: a
/// userinfo component can make a URL *display* as the real host while a parser sends it somewhere
/// else entirely — the same concern `origin::trusted_next_url` refuses it for); its scheme is
/// `https` (GitHub.com and every GitHub Enterprise Server deployment this crate has seen use TLS —
/// a deliberate decision, not an oversight; plain `http` is rejected too — round 2 review item
/// R10); and its host and *port* are exactly `web_origin` (S-5: round 2's fix checked only the
/// scheme, so a server could still send an `html_url` on any other `https` host, or a non-default
/// port on the right host, entirely — O30 closes the port gap; this pins both to the one web
/// origin this sync is actually about, the same reasoning `origin::trusted_next_url` uses for the
/// API host).
pub(crate) fn trusted_html_url(raw: &str, web_origin: &WebOrigin) -> Option<url::Url> {
    if raw.len() > MAX_KEPT_URL_BYTES || contains_hidden(raw) {
        return None;
    }
    let parsed = url::Url::parse(raw).ok()?;
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return None;
    }
    (parsed.scheme() == "https"
        && parsed.host_str() == Some(web_origin.host.as_str())
        && parsed.port_or_known_default() == Some(web_origin.port))
    .then_some(parsed)
}

/// Resolves `html_url` into the URL an [`ExternalRef`] should carry: the server's own value,
/// parsed (see [`trusted_html_url`]), when it is trusted, the crate's own constructed fallback
/// otherwise (also used when the server sent none at all). Increments `*malformed_fields` only
/// for the "sent but rejected" case — an absent `html_url` is normal, not malformed.
fn resolved_url(
    html_url: Option<&str>,
    web_origin: &WebOrigin,
    fallback: impl FnOnce() -> String,
    malformed_fields: &mut u32,
) -> String {
    match html_url.and_then(|u| trusted_html_url(u, web_origin)) {
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
    web_origin: &WebOrigin,
    malformed_fields: &mut u32,
) -> ExternalRef {
    ExternalRef {
        system: ExternalSystem::Github,
        key: format!("{owner_repo}#{number}"),
        url: Some(resolved_url(
            html_url,
            web_origin,
            || format!("https://github.com/{owner_repo}/issues/{number}"),
            malformed_fields,
        )),
    }
}

fn pull_ref(
    owner_repo: &str,
    number: u64,
    html_url: Option<&str>,
    web_origin: &WebOrigin,
    malformed_fields: &mut u32,
) -> ExternalRef {
    ExternalRef {
        system: ExternalSystem::Github,
        key: format!("{owner_repo}#{number}"),
        url: Some(resolved_url(
            html_url,
            web_origin,
            || format!("https://github.com/{owner_repo}/pull/{number}"),
            malformed_fields,
        )),
    }
}

fn milestone_ref(
    owner_repo: &str,
    number: u64,
    html_url: Option<&str>,
    web_origin: &WebOrigin,
    malformed_fields: &mut u32,
) -> ExternalRef {
    ExternalRef {
        system: ExternalSystem::Github,
        key: format!("{owner_repo}#milestone:{number}"),
        url: Some(resolved_url(
            html_url,
            web_origin,
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

impl SyncState {
    /// The open issue `source` (`owner/repo#<n>`) as a first read would report it, built from its
    /// last snapshot: an [`UpstreamChange::IssueOpened`] in `milestone`. For a caller that starts
    /// mirroring an issue it did not mirror before, because a later read moved it into a milestone
    /// the caller follows ([`UpstreamChange::IssueMilestoned`]): the move alone carries none of
    /// the issue's fields. `None` when this state has no snapshot of the issue, or it is closed.
    ///
    /// Call it on the state a sync returned, so the snapshot is the one that read just took.
    #[must_use]
    pub fn opened_from_snapshot(
        &self,
        source: &ExternalRef,
        milestone: Option<&ExternalRef>,
    ) -> Option<UpstreamChange> {
        let (repo, number) = source.key.rsplit_once('#')?;
        let number: u64 = number.parse().ok()?;
        let repo_state = self.repos.get(repo).or_else(|| {
            self.repos
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case(repo))
                .map(|(_, state)| state)
        })?;
        let snapshot = repo_state.issue_snapshots.get(&number)?;
        snapshot.open.then(|| UpstreamChange::IssueOpened {
            source: source.clone(),
            at: snapshot.updated_at.clone(),
            title: snapshot.title.clone(),
            body: snapshot.body.clone(),
            labels: snapshot.labels.clone(),
            milestone: milestone.cloned(),
        })
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
    let title = cap_chars(&issue.title, MAX_TITLE_CHARS);
    let raw_body = issue.body.as_deref().unwrap_or("");
    let body = cap_chars(raw_body, MAX_BODY_CHARS);
    IssueSnapshot {
        title_lossless: title == issue.title,
        body_lossless: body == raw_body,
        title,
        body,
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
    web_origin: &WebOrigin,
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
        web_origin,
        malformed_fields,
    );
    let at = next.updated_at.clone();
    let milestone = issue.milestone.as_ref().map(|m| {
        milestone_ref(
            owner_repo,
            m.number,
            m.html_url.as_deref(),
            web_origin,
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
    web_origin: &WebOrigin,
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
        web_origin,
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
    web_origin: &WebOrigin,
    malformed_fields: &mut u32,
) -> (Vec<UpstreamChange>, MilestoneSnapshot) {
    let next = milestone_snapshot_of(milestone);
    let source = milestone_ref(
        owner_repo,
        milestone.number,
        milestone.html_url.as_deref(),
        web_origin,
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
    fn expected_web_origin_is_github_com_443_by_default_not_the_api_host() {
        let origin = expected_web_origin(None);
        assert_eq!(origin.host, "github.com");
        assert_eq!(origin.port, 443);
    }

    #[test]
    fn expected_web_origin_is_derived_from_a_ghes_api_base() {
        let origin = expected_web_origin(Some("https://ghe.example.com/api/v3"));
        assert_eq!(origin.host, "ghe.example.com");
        assert_eq!(origin.port, 443);
    }

    #[test]
    fn expected_web_origin_keeps_a_ghes_api_bases_non_default_port() {
        // O30: the port is pinned too, not just the host — a GHES deployment on a non-default
        // port has a web UI on that same port, not 443.
        let origin = expected_web_origin(Some("https://ghe.example.com:8443/api/v3"));
        assert_eq!(origin.host, "ghe.example.com");
        assert_eq!(origin.port, 8443);
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

    fn github_com() -> WebOrigin {
        WebOrigin {
            host: "github.com".to_string(),
            port: 443,
        }
    }

    #[test]
    fn a_snapshot_says_whether_its_title_and_body_are_exactly_what_github_sent() {
        let snap = |title: &str, body: Option<&str>| {
            let mut wire = issue("2026-01-01T00:00:00Z");
            wire.title = title.to_string();
            wire.body = body.map(str::to_string);
            let mut malformed = 0u32;
            diff_issue(
                "example-org/demo-repo",
                &wire,
                None,
                &github_com(),
                &mut malformed,
            )
            .unwrap()
            .1
        };
        let plain = snap("Fix the login test", Some("Line one.\n\n- a list item\n"));
        assert!(plain.title_lossless() && plain.body_lossless());
        assert!(
            snap("No body", None).body_lossless(),
            "an absent body is an empty one"
        );
        // A zero-width joiner (an emoji sequence) is stripped from the copy: not exact.
        let joined = snap(
            "Ship it \u{1F469}\u{200D}\u{1F4BB}",
            Some("Ok \u{200D} then"),
        );
        assert!(!joined.title_lossless() && !joined.body_lossless());
        assert_eq!(joined.body(), "Ok  then");
        // A body over the cap is cut: not exact.
        let long = "x".repeat(MAX_BODY_CHARS + 1);
        assert!(!snap("t", Some(&long)).body_lossless());
        // A state saved before the flags were kept reads as not exact.
        let old: IssueSnapshot = serde_json::from_value(serde_json::json!({
            "title": "t", "body": "b", "open": true, "close_reason": null, "labels": [],
            "assignees": [], "milestone_number": null, "updated_at": "2026-01-01T00:00:00Z"
        }))
        .unwrap();
        assert!(!old.title_lossless() && !old.body_lossless());
    }

    #[test]
    fn an_issue_milestoned_later_is_opened_from_its_snapshot() {
        let mut malformed_fields = 0u32;
        let mut wire = issue("2026-01-01T00:00:00Z");
        wire.number = 4;
        wire.body = Some("Body".to_string());
        wire.labels = vec![crate::wire::WireLabel {
            name: "docs".to_string(),
        }];
        let (_, snapshot) = diff_issue(
            "example-org/demo-repo",
            &wire,
            None,
            &github_com(),
            &mut malformed_fields,
        )
        .unwrap();
        let mut state = SyncState::new();
        state
            .repos
            .entry("example-org/demo-repo".to_string())
            .or_default()
            .issue_snapshots
            .insert(4, snapshot.clone());
        let source = ExternalRef {
            system: ExternalSystem::Github,
            key: "Example-Org/Demo-Repo#4".to_string(),
            url: Some("https://github.com/example-org/demo-repo/issues/4".to_string()),
        };
        let milestone = ExternalRef {
            system: ExternalSystem::Github,
            key: "example-org/demo-repo#milestone:1".to_string(),
            url: None,
        };
        assert_eq!(
            state.opened_from_snapshot(&source, Some(&milestone)),
            Some(UpstreamChange::IssueOpened {
                source: source.clone(),
                at: GithubTimestamp::new("2026-01-01T00:00:00Z"),
                title: "Title".to_string(),
                body: "Body".to_string(),
                labels: vec!["docs".to_string()],
                milestone: Some(milestone.clone()),
            })
        );
        // Unknown issues, and closed ones, give nothing.
        let other = ExternalRef {
            key: "example-org/demo-repo#5".to_string(),
            ..source.clone()
        };
        assert_eq!(state.opened_from_snapshot(&other, None), None);
        let bad = ExternalRef {
            key: "example-org/demo-repo".to_string(),
            ..source.clone()
        };
        assert_eq!(state.opened_from_snapshot(&bad, None), None);
        let closed = IssueSnapshot {
            open: false,
            close_reason: Some(CloseReason::Completed),
            ..snapshot
        };
        state
            .repos
            .get_mut("example-org/demo-repo")
            .unwrap()
            .issue_snapshots
            .insert(4, closed);
        assert_eq!(state.opened_from_snapshot(&source, Some(&milestone)), None);
    }

    #[test]
    fn a_well_formed_issue_timestamp_diffs_normally() {
        let mut malformed_fields = 0u32;
        let result = diff_issue(
            "example-org/demo-repo",
            &issue("2026-01-01T00:00:00Z"),
            None,
            &github_com(),
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
                &github_com(),
                &mut malformed_fields
            )
            .is_none()
        );
        assert!(
            diff_issue(
                "example-org/demo-repo",
                &issue("2026-01-01T00:00:00.000Z"),
                None,
                &github_com(),
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
            &github_com(),
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
                &github_com(),
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
            &github_com(),
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
            &github_com(),
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
            &github_com(),
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
            &github_com(),
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
            &WebOrigin {
                host: "ghe.example.com".to_string(),
                port: 443,
            },
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
            &github_com(),
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
            &github_com(),
            &mut malformed_fields,
        )
        .expect("well-formed");
        assert_eq!(
            changes[0].source().url.as_deref(),
            Some("https://github.com/example-org/demo-repo/issues/1")
        );
    }

    #[test]
    fn r10_residual_an_html_url_with_userinfo_is_untrusted() {
        // Stream Q's r10-kept-link-with-userinfo regression (O30): `%75ser` is userinfo
        // ("user", percent-encoded) sitting before the real host — the URL parses to
        // `github.com`, same as `origin::trusted_next_url` refuses this for a `next` link.
        let mut malformed_fields = 0u32;
        let mut spoofed = issue("2026-01-01T00:00:00Z");
        spoofed.html_url =
            Some("https://%75ser@github.com/example-org/demo-repo/issues/1".to_string());
        diff_issue(
            "example-org/demo-repo",
            &spoofed,
            None,
            &github_com(),
            &mut malformed_fields,
        )
        .expect("the item itself is still processed");
        assert_eq!(malformed_fields, 1);
    }

    #[test]
    fn r10_residual_an_html_url_on_an_unexpected_port_is_untrusted() {
        // Stream Q's r10-kept-link-on-another-port regression (O30): the host string matches
        // but the port does not, so this is not the configured web origin either.
        let mut malformed_fields = 0u32;
        let mut on_another_port = issue("2026-01-01T00:00:00Z");
        on_another_port.html_url =
            Some("https://github.com:8443/example-org/demo-repo/issues/1".to_string());
        diff_issue(
            "example-org/demo-repo",
            &on_another_port,
            None,
            &github_com(),
            &mut malformed_fields,
        )
        .expect("the item itself is still processed");
        assert_eq!(malformed_fields, 1);
    }

    #[test]
    fn r10_residual_an_oversized_html_url_is_untrusted() {
        // Stream Q's r10-kept-link-without-a-cap regression (O30): a trusted scheme, host
        // and port do not also mean a reasonable length — a kept link is shown to a person.
        let mut malformed_fields = 0u32;
        let mut huge = issue("2026-01-01T00:00:00Z");
        huge.html_url = Some(format!("https://github.com/{}", "x".repeat(3_000)));
        diff_issue(
            "example-org/demo-repo",
            &huge,
            None,
            &github_com(),
            &mut malformed_fields,
        )
        .expect("the item itself is still processed");
        assert_eq!(malformed_fields, 1);
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
            &github_com(),
            &mut malformed_fields,
        )
        .expect("well-formed");
        assert_eq!(snapshot.assignees, vec!["abcd".to_string()]);
    }
}
