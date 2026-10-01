//! Top-level orchestration: [`sync`] turns a [`SyncState`] and a [`Transport`] into a new state
//! and the [`UpstreamChange`]s found. Pure in the sense the brief means it: the caller persists
//! the returned state and decides what to do with the changes; this function does no I/O beyond
//! the `Transport` it is given, and never touches the event log.

use crate::change::{UpstreamChange, diff_issue, diff_milestone, diff_pull};
use crate::client::{GithubClient, Outcome};
use crate::state::{RepoState, SyncState};
use crate::time::GithubTimestamp;
use crate::transport::{AuthToken, Transport};
use crate::wire::{WireIssue, WireMilestone, WirePullRequest};

/// A repository to sync, `owner/repo`. GitHub owner and repository names are restricted to ASCII
/// letters, digits, `-`, `_`, `.`; this is enforced here because the value is spliced directly
/// into a URL path, with no percent-encoding step in this crate.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RepoRef(String);

/// `value` was not `owner/repo` with GitHub's allowed characters.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("not a valid \"owner/repo\": {0:?}")]
pub struct InvalidRepoRef(pub String);

impl RepoRef {
    /// Validates and wraps `owner/repo`.
    pub fn new(value: impl Into<String>) -> Result<Self, InvalidRepoRef> {
        let value = value.into();
        let valid = |s: &str| {
            !s.is_empty()
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        };
        match value.split_once('/') {
            Some((owner, repo)) if valid(owner) && valid(repo) => Ok(Self(value)),
            _ => Err(InvalidRepoRef(value)),
        }
    }

    /// The `owner/repo` text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// What to sync, and the inputs that keep `sync` a pure function of its arguments: it reads no
/// clock and creates no token itself.
#[derive(Debug)]
pub struct SyncConfig {
    /// Repositories to sync, in order.
    pub repos: Vec<RepoRef>,
    /// The token sent as `Authorization: Bearer <token>` on every request.
    pub token: AuthToken,
    /// Wall-clock time as Unix seconds, used only to turn a `retry-after` duration or an
    /// exponential backoff into an absolute `RateLimited.until`.
    pub now_unix: i64,
    /// Wall-clock time in GitHub's timestamp shape, used only as the "when" of a milestone change
    /// (the milestones endpoint reports no `updated_at`).
    pub now: GithubTimestamp,
    /// Overrides the API root (GitHub Enterprise Server). `None` uses `api.github.com`.
    pub api_base: Option<String>,
}

/// The token is rate-limited; try again no sooner than `until`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateLimited {
    /// Unix seconds.
    pub until: i64,
    /// A secondary (abuse-detection) limit rather than the primary hourly quota.
    pub secondary: bool,
}

/// Which list endpoint an error or a skipped item came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resource {
    /// The issues list.
    Issues,
    /// The pull requests list.
    PullRequests,
    /// The milestones list.
    Milestones,
}

/// A non-fatal problem hit while syncing one repository's resource.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncIssue {
    /// `owner/repo`.
    pub repo: String,
    /// Which resource.
    pub resource: Resource,
    /// A short, non-sensitive description (never includes the token).
    pub message: String,
}

/// Everything one `sync` call produced.
#[derive(Debug)]
pub struct SyncOutcome {
    /// The new state; the caller persists this for next time.
    pub state: SyncState,
    /// What changed upstream, across every repository that was reached before any rate limit.
    pub changes: Vec<UpstreamChange>,
    /// Set when a rate limit stopped the sync before every repository was reached. Nothing later
    /// in `config.repos` (in iteration order) was contacted.
    pub rate_limited: Option<RateLimited>,
    /// Non-fatal problems (a resource whose request failed outright). A repository that hit one
    /// of these still tries its other resources.
    pub errors: Vec<SyncIssue>,
    /// How many items across the whole call failed to parse and were skipped.
    pub malformed_skipped: u32,
}

fn issues_url(api_base: &str, repo: &str, since: Option<&GithubTimestamp>) -> String {
    let mut url =
        format!("{api_base}/repos/{repo}/issues?state=all&sort=updated&direction=asc&per_page=100");
    if let Some(since) = since {
        url.push_str("&since=");
        url.push_str(&encode_timestamp(since.as_str()));
    }
    url
}

fn pulls_url(api_base: &str, repo: &str) -> String {
    format!("{api_base}/repos/{repo}/pulls?state=all&sort=updated&direction=desc&per_page=100")
}

fn milestones_url(api_base: &str, repo: &str) -> String {
    format!("{api_base}/repos/{repo}/milestones?state=all&sort=due_on&direction=asc&per_page=100")
}

/// The only character RFC 3339 timestamps need encoded in a query string.
fn encode_timestamp(s: &str) -> String {
    s.replace(':', "%3A")
}

enum ResourceResult {
    Changes(Vec<UpstreamChange>, u32),
    RateLimited(RateLimited),
    Error(String),
}

async fn sync_issues<T: Transport>(
    client: &GithubClient<'_, T>,
    repo: &str,
    repo_state: &mut RepoState,
    now_unix: i64,
) -> ResourceResult {
    let mut attempts = repo_state.secondary_backoff_attempts;
    let url = issues_url(client.api_base(), repo, repo_state.issues.since.as_ref());
    let result = client
        .list::<WireIssue>(url, &repo_state.issues, false, now_unix, &mut attempts)
        .await;
    repo_state.secondary_backoff_attempts = attempts;
    match result {
        Err(e) => ResourceResult::Error(e.to_string()),
        Ok(Outcome::RateLimited { until, secondary }) => {
            ResourceResult::RateLimited(RateLimited { until, secondary })
        }
        Ok(Outcome::Ok(list)) => {
            let mut changes = Vec::new();
            for issue in &list.items {
                // The issues endpoint also lists pull requests; those are synced separately.
                if issue.pull_request.is_some() {
                    continue;
                }
                let previous = repo_state.issue_snapshots.get(&issue.number);
                let (mut found, snapshot) = diff_issue(repo, issue, previous);
                changes.append(&mut found);
                repo_state.issue_snapshots.insert(issue.number, snapshot);
            }
            if !list.not_modified {
                repo_state.issues.etag = list.etag;
                repo_state.issues.last_modified = list.last_modified;
                if let Some(max) = list.max_updated_at
                    && repo_state.issues.since.as_ref().is_none_or(|s| max > *s)
                {
                    repo_state.issues.since = Some(max);
                }
            }
            ResourceResult::Changes(changes, list.malformed_skipped)
        }
    }
}

async fn sync_pulls<T: Transport>(
    client: &GithubClient<'_, T>,
    repo: &str,
    repo_state: &mut RepoState,
    now_unix: i64,
) -> ResourceResult {
    let mut attempts = repo_state.secondary_backoff_attempts;
    let url = pulls_url(client.api_base(), repo);
    let result = client
        .list::<WirePullRequest>(url, &repo_state.pulls, true, now_unix, &mut attempts)
        .await;
    repo_state.secondary_backoff_attempts = attempts;
    match result {
        Err(e) => ResourceResult::Error(e.to_string()),
        Ok(Outcome::RateLimited { until, secondary }) => {
            ResourceResult::RateLimited(RateLimited { until, secondary })
        }
        Ok(Outcome::Ok(list)) => {
            let mut changes = Vec::new();
            for pr in &list.items {
                let previous = repo_state.pull_snapshots.get(&pr.number);
                let (mut found, snapshot) = diff_pull(repo, pr, previous);
                changes.append(&mut found);
                repo_state.pull_snapshots.insert(pr.number, snapshot);
            }
            if !list.not_modified {
                repo_state.pulls.etag = list.etag;
                repo_state.pulls.last_modified = list.last_modified;
                if let Some(max) = list.max_updated_at
                    && repo_state.pulls.since.as_ref().is_none_or(|s| max > *s)
                {
                    repo_state.pulls.since = Some(max);
                }
            }
            ResourceResult::Changes(changes, list.malformed_skipped)
        }
    }
}

async fn sync_milestones<T: Transport>(
    client: &GithubClient<'_, T>,
    repo: &str,
    repo_state: &mut RepoState,
    now_unix: i64,
    now: &GithubTimestamp,
) -> ResourceResult {
    let mut attempts = repo_state.secondary_backoff_attempts;
    let url = milestones_url(client.api_base(), repo);
    let result = client
        .list::<WireMilestone>(url, &repo_state.milestones, false, now_unix, &mut attempts)
        .await;
    repo_state.secondary_backoff_attempts = attempts;
    match result {
        Err(e) => ResourceResult::Error(e.to_string()),
        Ok(Outcome::RateLimited { until, secondary }) => {
            ResourceResult::RateLimited(RateLimited { until, secondary })
        }
        Ok(Outcome::Ok(list)) => {
            let mut changes = Vec::new();
            for milestone in &list.items {
                let previous = repo_state.milestone_snapshots.get(&milestone.number);
                let (mut found, snapshot) = diff_milestone(repo, milestone, previous, now);
                changes.append(&mut found);
                repo_state
                    .milestone_snapshots
                    .insert(milestone.number, snapshot);
            }
            if !list.not_modified {
                repo_state.milestones.etag = list.etag;
                repo_state.milestones.last_modified = list.last_modified;
            }
            ResourceResult::Changes(changes, list.malformed_skipped)
        }
    }
}

/// Syncs every repository in `config.repos`, in order, stopping as soon as any resource is
/// rate-limited (GitHub's rate limits are account-wide, so there is no point trying the next
/// repository). Resources already synced before that point keep their results.
pub async fn sync<T: Transport>(
    state: SyncState,
    transport: &T,
    config: &SyncConfig,
) -> SyncOutcome {
    let mut state = state;
    let mut changes = Vec::new();
    let mut errors = Vec::new();
    let mut malformed_skipped = 0u32;
    let mut rate_limited = None;

    let mut client = GithubClient::new(transport, config.token.clone());
    if let Some(base) = &config.api_base {
        client = client.with_api_base(base.clone());
    }

    for repo in &config.repos {
        let owner_repo = repo.as_str();
        let mut repo_state = state.repos.remove(owner_repo).unwrap_or_default();

        // Each resource is awaited only once the previous one is known not to have hit a rate
        // limit: a real transport must never be asked for more once told to back off.
        let mut hit_limit = None;
        let milestones = sync_milestones(
            &client,
            owner_repo,
            &mut repo_state,
            config.now_unix,
            &config.now,
        )
        .await;
        let results: Vec<(Resource, ResourceResult)> = match milestones {
            ResourceResult::RateLimited(rl) => {
                hit_limit = Some(rl);
                vec![(Resource::Milestones, ResourceResult::RateLimited(rl))]
            }
            other => {
                let mut results = vec![(Resource::Milestones, other)];
                let issues =
                    sync_issues(&client, owner_repo, &mut repo_state, config.now_unix).await;
                match issues {
                    ResourceResult::RateLimited(rl) => {
                        hit_limit = Some(rl);
                        results.push((Resource::Issues, ResourceResult::RateLimited(rl)));
                    }
                    other => {
                        results.push((Resource::Issues, other));
                        let pulls =
                            sync_pulls(&client, owner_repo, &mut repo_state, config.now_unix).await;
                        if let ResourceResult::RateLimited(rl) = &pulls {
                            hit_limit = Some(*rl);
                        }
                        results.push((Resource::PullRequests, pulls));
                    }
                }
                results
            }
        };

        for (resource, result) in results {
            match result {
                ResourceResult::Changes(mut found, skipped) => {
                    changes.append(&mut found);
                    malformed_skipped += skipped;
                }
                ResourceResult::Error(message) => {
                    errors.push(SyncIssue {
                        repo: owner_repo.to_string(),
                        resource,
                        message,
                    });
                }
                ResourceResult::RateLimited(_) => {}
            }
        }

        state.repos.insert(owner_repo.to_string(), repo_state);
        if let Some(rl) = hit_limit {
            rate_limited = Some(rl);
            break;
        }
    }

    SyncOutcome {
        state,
        changes,
        rate_limited,
        errors,
        malformed_skipped,
    }
}
