//! Top-level orchestration: [`sync`] turns a [`SyncState`] and a [`Transport`] into a new state
//! and the [`UpstreamChange`]s found. Pure in the sense the brief means it: the caller persists
//! the returned state and decides what to do with the changes; this function does no I/O beyond
//! the `Transport` it is given, and never touches the event log.

use crate::bounds::{Limits, MAX_REPORTED_URL_CHARS, cap_chars};
use crate::change::{UpstreamChange, diff_issue, diff_milestone, diff_pull, expected_web_host};
use crate::client::{GithubClient, Outcome};
use crate::state::{ListCache, RepoState, ResumeCursor, SyncState};
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
    /// How many items across the whole call failed to parse and were skipped entirely, plus how
    /// many individual malformed *fields* (currently: an `html_url` with an untrusted scheme —
    /// R10) were dropped and replaced with a safe default while the rest of their item was kept.
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

/// Percent-encodes `s` for use as one query-string value (RFC 3986 "unreserved" characters pass
/// through unescaped; everything else — including `:`, the only character GitHub's own normal
/// `YYYY-MM-DDTHH:MM:SSZ` timestamp shape needs it for — becomes `%XX` from its UTF-8 bytes). This
/// is general-purpose rather than special-cased to `:` alone so a `since` cursor stays correctly
/// encoded even if it is ever something other than GitHub's own well-formed shape (see
/// `GithubTimestamp::is_well_formed`).
fn encode_timestamp(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char);
            }
            _ => {
                out.push('%');
                out.push_str(&format!("{byte:02X}"));
            }
        }
    }
    out
}

enum ResourceResult {
    /// Changes found, how many items were skipped (or individual fields dropped — see
    /// [`SyncOutcome::malformed_skipped`]) as malformed, and — if a server-supplied
    /// `Link: rel="next"` outside the API base was ignored — a message to raise as an additional
    /// `SyncIssue` alongside these (otherwise successful) changes.
    Changes(Vec<UpstreamChange>, u32, Option<String>),
    RateLimited(RateLimited),
    Error(String),
}

/// Builds the `SyncIssue` message for a rejected `Link: rel="next"`. `url` is untrusted — it is
/// exactly what a server (or a proxy in front of it) sent — so it is hidden-character-stripped and
/// length-capped before going into a message a person reads (round 3 review nit).
fn blocked_link_message(url: &str) -> String {
    let safe = cap_chars(url, MAX_REPORTED_URL_CHARS);
    format!("ignored a paginated \"next\" link outside the configured API base: {safe}")
}

async fn sync_issues<T: Transport>(
    client: &GithubClient<'_, T>,
    repo: &str,
    repo_state: &mut RepoState,
    now_unix: i64,
    web_host: &str,
    limits: Limits,
) -> ResourceResult {
    let mut attempts = repo_state.secondary_backoff_attempts;
    let url = issues_url(client.api_base(), repo, repo_state.issues.since.as_ref());
    let result = client
        .list::<WireIssue>(
            url,
            &repo_state.issues,
            false,
            now_unix,
            &mut attempts,
            limits,
        )
        .await;
    repo_state.secondary_backoff_attempts = attempts;
    match result {
        Err(e) => ResourceResult::Error(e.to_string()),
        Ok(Outcome::RateLimited { until, secondary }) => {
            ResourceResult::RateLimited(RateLimited { until, secondary })
        }
        Ok(Outcome::Ok(list)) => {
            let mut changes = Vec::new();
            let mut malformed_timestamps = 0u32;
            let mut malformed_fields = 0u32;
            for issue in &list.items {
                // The issues endpoint also lists pull requests; those are synced separately.
                if issue.pull_request.is_some() {
                    continue;
                }
                let previous = repo_state.issue_snapshots.get(&issue.number);
                match diff_issue(repo, issue, previous, web_host, &mut malformed_fields) {
                    Some((mut found, snapshot)) => {
                        changes.append(&mut found);
                        repo_state.issue_snapshots.insert(issue.number, snapshot);
                    }
                    None => malformed_timestamps += 1,
                }
            }
            // Issues are read ascending with a server-side `since` filter, so a walk a cap cuts
            // short is self-healing: the next call's `since` is the last item this call actually
            // processed, and GitHub includes items at that same timestamp again rather than
            // skipping them. This is unlike pull requests (see `sync_pulls`), which have no
            // server-side filter to re-anchor on.
            if !list.not_modified {
                repo_state.issues.etag = list.etag;
                repo_state.issues.last_modified = list.last_modified;
                if let Some(max) = list.max_updated_at
                    && repo_state.issues.since.as_ref().is_none_or(|s| max > *s)
                {
                    repo_state.issues.since = Some(max);
                }
            }
            let warning = list.blocked_link.as_deref().map(blocked_link_message);
            ResourceResult::Changes(
                changes,
                list.malformed_skipped + malformed_timestamps + malformed_fields,
                warning,
            )
        }
    }
}

async fn sync_pulls<T: Transport>(
    client: &GithubClient<'_, T>,
    repo: &str,
    repo_state: &mut RepoState,
    now_unix: i64,
    web_host: &str,
    limits: Limits,
) -> ResourceResult {
    let mut attempts = repo_state.secondary_backoff_attempts;
    // Pull requests are listed newest-first with no server-side filter to resume through: a walk
    // a previous call's cap cut short must continue from exactly where it left off, not restart
    // from page 1 (which would just re-walk the same newest items and, if `since` had been
    // advanced to them, silently skip everything older that was never actually reached — see
    // brief review item 2). When resuming, conditional headers are dropped too: they were
    // captured against page 1, not this later page, and a stray 304 here would wrongly be read as
    // "nothing changed" for the whole resource.
    //
    // `list`'s own `max_updated_at`/`oldest_seen` are scoped to the one call (one page, or a few,
    // but never the whole walk once it spans several resumed calls): carry the running bounds
    // across calls here, so that once the walk does complete, the cursor it advances to reflects
    // the newest item across the *entire* walk, not just whichever page happened to be fetched by
    // the final call.
    let carried_newest = repo_state
        .pulls
        .resume
        .as_ref()
        .and_then(|r| r.newest_seen.clone());
    let carried_oldest = repo_state
        .pulls
        .resume
        .as_ref()
        .and_then(|r| r.oldest_seen.clone());
    let (url, cache) = match &repo_state.pulls.resume {
        Some(resume) => (
            resume.next_url.clone(),
            ListCache {
                etag: None,
                last_modified: None,
                since: repo_state.pulls.since.clone(),
                resume: None,
            },
        ),
        None => (pulls_url(client.api_base(), repo), repo_state.pulls.clone()),
    };
    let result = client
        .list::<WirePullRequest>(url, &cache, true, now_unix, &mut attempts, limits)
        .await;
    repo_state.secondary_backoff_attempts = attempts;
    match result {
        Err(e) => ResourceResult::Error(e.to_string()),
        Ok(Outcome::RateLimited { until, secondary }) => {
            ResourceResult::RateLimited(RateLimited { until, secondary })
        }
        Ok(Outcome::Ok(list)) => {
            let mut changes = Vec::new();
            let mut malformed_timestamps = 0u32;
            let mut malformed_fields = 0u32;
            for pr in &list.items {
                let previous = repo_state.pull_snapshots.get(&pr.number);
                match diff_pull(repo, pr, previous, web_host, &mut malformed_fields) {
                    Some((mut found, snapshot)) => {
                        changes.append(&mut found);
                        repo_state.pull_snapshots.insert(pr.number, snapshot);
                    }
                    None => malformed_timestamps += 1,
                }
            }
            if !list.not_modified {
                repo_state.pulls.etag = list.etag;
                repo_state.pulls.last_modified = list.last_modified;
                let newest_seen = newer(carried_newest, list.max_updated_at.clone());
                let oldest_seen = older(carried_oldest, list.oldest_seen.clone());
                if list.completed {
                    // The walk reached the old cursor (or ran out of pages): everything newer
                    // than the new cursor has now actually been seen, so it is safe to advance —
                    // using the bounds carried across the *whole* walk, not just this last call.
                    if let Some(max) = newest_seen
                        && repo_state.pulls.since.as_ref().is_none_or(|s| &max > s)
                    {
                        repo_state.pulls.since = Some(max);
                    }
                    repo_state.pulls.resume = None;
                } else if let Some(next_url) = list.resume_from.clone() {
                    // Cut short by a cap: keep the old cursor untouched and remember where to
                    // continue, rather than advancing `since` to "the newest seen", which would
                    // make the next call stop immediately and leave everything older unfetched.
                    repo_state.pulls.resume = Some(ResumeCursor {
                        next_url,
                        oldest_seen,
                        newest_seen,
                    });
                } else {
                    // Cut short by something that gets no resume pointer (a malformed page, or an
                    // untrusted `Link`): restart from the top next call.
                    repo_state.pulls.resume = None;
                }
            }
            let warning = list.blocked_link.as_deref().map(blocked_link_message);
            ResourceResult::Changes(
                changes,
                list.malformed_skipped + malformed_timestamps + malformed_fields,
                warning,
            )
        }
    }
}

/// The later of two optional timestamps (`None` loses to anything).
fn newer(a: Option<GithubTimestamp>, b: Option<GithubTimestamp>) -> Option<GithubTimestamp> {
    match (a, b) {
        (Some(a), Some(b)) => Some(if a > b { a } else { b }),
        (Some(x), None) | (None, Some(x)) => Some(x),
        (None, None) => None,
    }
}

/// The earlier of two optional timestamps (`None` loses to anything).
fn older(a: Option<GithubTimestamp>, b: Option<GithubTimestamp>) -> Option<GithubTimestamp> {
    match (a, b) {
        (Some(a), Some(b)) => Some(if a < b { a } else { b }),
        (Some(x), None) | (None, Some(x)) => Some(x),
        (None, None) => None,
    }
}

async fn sync_milestones<T: Transport>(
    client: &GithubClient<'_, T>,
    repo: &str,
    repo_state: &mut RepoState,
    now_unix: i64,
    now: &GithubTimestamp,
    web_host: &str,
    limits: Limits,
) -> ResourceResult {
    let mut attempts = repo_state.secondary_backoff_attempts;
    let url = milestones_url(client.api_base(), repo);
    let result = client
        .list::<WireMilestone>(
            url,
            &repo_state.milestones,
            false,
            now_unix,
            &mut attempts,
            limits,
        )
        .await;
    repo_state.secondary_backoff_attempts = attempts;
    match result {
        Err(e) => ResourceResult::Error(e.to_string()),
        Ok(Outcome::RateLimited { until, secondary }) => {
            ResourceResult::RateLimited(RateLimited { until, secondary })
        }
        Ok(Outcome::Ok(list)) => {
            let mut changes = Vec::new();
            let mut malformed_fields = 0u32;
            for milestone in &list.items {
                let previous = repo_state.milestone_snapshots.get(&milestone.number);
                let (mut found, snapshot) = diff_milestone(
                    repo,
                    milestone,
                    previous,
                    now,
                    web_host,
                    &mut malformed_fields,
                );
                changes.append(&mut found);
                repo_state
                    .milestone_snapshots
                    .insert(milestone.number, snapshot);
            }
            if !list.not_modified {
                repo_state.milestones.etag = list.etag;
                repo_state.milestones.last_modified = list.last_modified;
            }
            let warning = list.blocked_link.as_deref().map(blocked_link_message);
            ResourceResult::Changes(changes, list.malformed_skipped + malformed_fields, warning)
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
    sync_with_limits(state, transport, config, Limits::default()).await
}

/// `sync`'s actual implementation, parameterised over the page/item caps. Production code only
/// ever reaches this through `sync` (always [`Limits::default`], the real [`crate::bounds`]
/// constants); this crate's own tests call it directly with a tiny [`Limits`] to exercise
/// cap-triggered truncation and resume behaviour without multi-thousand-item fixtures.
async fn sync_with_limits<T: Transport>(
    state: SyncState,
    transport: &T,
    config: &SyncConfig,
    limits: Limits,
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
    // Derived once for the whole call (round 3 review item S-5): the web host every `html_url`
    // must match to be trusted. See `expected_web_host`'s doc for why this is not simply
    // `config.api_base` itself.
    let web_host = expected_web_host(config.api_base.as_deref());

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
            &web_host,
            limits,
        )
        .await;
        let results: Vec<(Resource, ResourceResult)> = match milestones {
            ResourceResult::RateLimited(rl) => {
                hit_limit = Some(rl);
                vec![(Resource::Milestones, ResourceResult::RateLimited(rl))]
            }
            other => {
                let mut results = vec![(Resource::Milestones, other)];
                let issues = sync_issues(
                    &client,
                    owner_repo,
                    &mut repo_state,
                    config.now_unix,
                    &web_host,
                    limits,
                )
                .await;
                match issues {
                    ResourceResult::RateLimited(rl) => {
                        hit_limit = Some(rl);
                        results.push((Resource::Issues, ResourceResult::RateLimited(rl)));
                    }
                    other => {
                        results.push((Resource::Issues, other));
                        let pulls = sync_pulls(
                            &client,
                            owner_repo,
                            &mut repo_state,
                            config.now_unix,
                            &web_host,
                            limits,
                        )
                        .await;
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
                ResourceResult::Changes(mut found, skipped, warning) => {
                    changes.append(&mut found);
                    malformed_skipped += skipped;
                    if let Some(message) = warning {
                        errors.push(SyncIssue {
                            repo: owner_repo.to_string(),
                            resource,
                            message,
                        });
                    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::{RecordedExchange, ReplayTransport};

    #[test]
    fn encode_timestamp_escapes_colons() {
        assert_eq!(
            encode_timestamp("2026-01-02T03:04:05Z"),
            "2026-01-02T03%3A04%3A05Z"
        );
    }

    #[test]
    fn encode_timestamp_passes_unreserved_characters_through_untouched() {
        assert_eq!(encode_timestamp("safe-._~Chars09"), "safe-._~Chars09");
    }

    #[test]
    fn encode_timestamp_escapes_arbitrary_non_unreserved_bytes() {
        // Not a shape `GithubTimestamp` actually sends, but this is deliberately a general
        // percent-encoder, not one special-cased to `:` alone (review item 3).
        assert_eq!(encode_timestamp("a b+c"), "a%20b%2Bc");
    }

    const MILESTONES_URL: &str = "https://api.github.com/repos/example-org/demo-repo/milestones?state=all&sort=due_on&direction=asc&per_page=100";
    const ISSUES_URL: &str = "https://api.github.com/repos/example-org/demo-repo/issues?state=all&sort=updated&direction=asc&per_page=100";
    const PULLS_URL: &str = "https://api.github.com/repos/example-org/demo-repo/pulls?state=all&sort=updated&direction=desc&per_page=100";

    fn empty_list(url: &str) -> RecordedExchange {
        RecordedExchange {
            method: "GET".to_string(),
            url: url.to_string(),
            request_headers: vec![],
            status: 200,
            response_headers: vec![],
            body: b"[]".to_vec(),
        }
    }

    fn pr_json(number: u64, updated_at: &str) -> String {
        format!(
            r#"{{"number":{number},"title":"PR {number}","state":"open","updated_at":"{updated_at}"}}"#
        )
    }

    fn page(url: &str, next: Option<&str>, body: String) -> RecordedExchange {
        let response_headers = match next {
            Some(n) => vec![("Link".to_string(), format!("<{n}>; rel=\"next\""))],
            None => vec![],
        };
        RecordedExchange {
            method: "GET".to_string(),
            url: url.to_string(),
            request_headers: vec![],
            status: 200,
            response_headers,
            body: body.into_bytes(),
        }
    }

    fn config() -> SyncConfig {
        SyncConfig {
            repos: vec![RepoRef::new("example-org/demo-repo").expect("valid repo")],
            token: AuthToken::new("ghp_test_token_not_real"),
            now_unix: 2_000_000_000,
            now: GithubTimestamp::new("2026-01-01T00:00:00Z"),
            api_base: None,
        }
    }

    /// Reproduces brief review item 2 end to end: a pull request listing spread across more pages
    /// than one call is allowed to fetch (`Limits::max_pages = 1`), newest page first. Three
    /// `sync_with_limits` calls, each handed the previous call's returned state, must together see
    /// every PR exactly once and must only ever advance `since` on the call that actually
    /// completes the walk — and even then, to the newest PR across the *whole* walk (PR 6), not
    /// just whichever page the final call happened to fetch (PRs 1 and 2).
    #[tokio::test]
    async fn pull_requests_eventually_all_appear_exactly_once_and_the_cursor_never_skips() {
        let page2_url = format!("{PULLS_URL}&page=2");
        let page3_url = format!("{PULLS_URL}&page=3");

        let page1 = page(
            PULLS_URL,
            Some(&page2_url),
            format!(
                "[{},{}]",
                pr_json(6, "2026-01-01T00:06:00Z"),
                pr_json(5, "2026-01-01T00:05:00Z")
            ),
        );
        let page2 = page(
            &page2_url,
            Some(&page3_url),
            format!(
                "[{},{}]",
                pr_json(4, "2026-01-01T00:04:00Z"),
                pr_json(3, "2026-01-01T00:03:00Z")
            ),
        );
        let page3 = page(
            &page3_url,
            None,
            format!(
                "[{},{}]",
                pr_json(2, "2026-01-01T00:02:00Z"),
                pr_json(1, "2026-01-01T00:01:00Z")
            ),
        );

        let exchanges = vec![
            empty_list(MILESTONES_URL),
            empty_list(ISSUES_URL),
            page1,
            empty_list(MILESTONES_URL),
            empty_list(ISSUES_URL),
            page2,
            empty_list(MILESTONES_URL),
            empty_list(ISSUES_URL),
            page3,
        ];
        let transport = ReplayTransport::from_exchanges(exchanges);
        let limits = Limits {
            max_pages: 1,
            max_items: 100,
        };

        let mut state = SyncState::new();
        let mut all_opened: Vec<String> = Vec::new();

        for call in 0..3 {
            let outcome = sync_with_limits(state, &transport, &config(), limits).await;
            assert!(
                outcome.errors.is_empty(),
                "call {call}: {:?}",
                outcome.errors
            );
            assert!(outcome.rate_limited.is_none(), "call {call}");
            for change in &outcome.changes {
                if let UpstreamChange::PullRequestOpened { source, .. } = change {
                    all_opened.push(source.key.clone());
                }
            }
            let repo = outcome
                .state
                .repos
                .get("example-org/demo-repo")
                .expect("repo state");
            match call {
                0 | 1 => {
                    assert!(
                        repo.pulls.since.is_none(),
                        "call {call}: the cursor must not advance mid-walk"
                    );
                    assert!(
                        repo.pulls.resume.is_some(),
                        "call {call}: a resume pointer must be set"
                    );
                }
                2 => {
                    assert!(repo.pulls.resume.is_none(), "the walk is now complete");
                    assert_eq!(
                        repo.pulls.since.as_ref().map(GithubTimestamp::as_str),
                        Some("2026-01-01T00:06:00Z"),
                        "the cursor must reflect the newest PR across the whole walk, not just \
                         the last page this call happened to fetch"
                    );
                }
                _ => unreachable!(),
            }
            state = outcome.state;
        }

        assert_eq!(all_opened.len(), 6, "{all_opened:?}");
        let mut sorted = all_opened.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            6,
            "every PR must be reported opened exactly once: {all_opened:?}"
        );
        for n in 1..=6 {
            assert!(
                all_opened.contains(&format!("example-org/demo-repo#{n}")),
                "PR {n} was never seen: {all_opened:?}"
            );
        }
    }
}
