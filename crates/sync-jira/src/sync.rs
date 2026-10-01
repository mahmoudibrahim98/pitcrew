//! Top-level orchestration: [`sync`] turns a [`SyncState`] and a [`Transport`] into a new state
//! and the [`UpstreamChange`]s found — the same contract as `pitcrew_sync_github::sync::sync`:
//! pure beyond the `Transport` it is given, the caller persists the returned state, and this
//! function never touches the event log.

use crate::auth::JiraAuth;
use crate::bounds::CURSOR_SAFETY_MARGIN_HOURS;
use crate::change::{UpstreamChange, diff_epic, diff_issue};
use crate::client::{JiraClient, Limits, Outcome, SearchQuery};
use crate::deployment::Deployment;
use crate::jql::{InvalidProjectRef, ProjectRef, incremental_query};
use crate::state::SyncState;
use crate::time::{account_minute, resolve_account_zone};
use jiff::{SignedDuration, Timestamp};
use pitcrew_sync_github::transport::Transport;

pub use crate::client::RateLimited;

/// The fields every search asks for, fetched in full on both deployments regardless of
/// `epic_link_field` — "fetch only the fields you use" (see the brief).
const BASE_FIELDS: &[&str] = &[
    "summary",
    "description",
    "status",
    "resolution",
    "labels",
    "assignee",
    "parent",
    "issuetype",
    "updated",
];

/// What to sync, and the inputs that keep `sync` a pure function of its arguments: it reads no
/// clock itself.
#[derive(Debug)]
pub struct SyncConfig {
    /// Projects to sync, in order. Each gets its own JQL query (`project in ("<key>") AND
    /// updated >= …`, the shape the brief asks for — a one-element `in (...)`) and its own
    /// cursor/snapshots in [`SyncState`], rather than one query spanning every project: a project
    /// added later starts its own first full sync without disturbing the others' cursors, and the
    /// per-project resource/error reporting (see [`SyncIssue::project`]) stays precise. This
    /// mirrors `pitcrew_sync_github::SyncConfig::repos`'s per-repository shape.
    pub projects: Vec<ProjectRef>,
    /// Credentials: `JiraAuth::Basic` for Cloud, `JiraAuth::Bearer` for Data Center.
    pub auth: JiraAuth,
    /// The full REST API root, e.g. `https://jira.example.com/rest/api/3` (Cloud) or
    /// `https://jira.example.com/rest/api/2` (Data Center).
    pub api_base: String,
    /// The site root used to build browsable `ExternalRef` urls, e.g.
    /// `https://jira.example.com`. Jira's REST responses carry no browsable URL field (unlike
    /// GitHub's `html_url`), so this is supplied separately from `api_base`.
    pub site_base: String,
    /// The custom field id holding the epic link on a classic (non-next-gen) Data Center project,
    /// e.g. `customfield_10008`. Modern Jira Cloud reports this through `fields.parent` instead,
    /// which needs no configuration; see [`crate::wire::WireFields::epic_key`].
    pub epic_link_field: Option<String>,
    /// Wall-clock time as Unix seconds, used only to turn a `Retry-After` duration or an
    /// exponential backoff into an absolute `RateLimited.until`.
    pub now_unix: i64,
}

/// Which resource a non-fatal problem came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resource {
    /// `/myself` (the account time zone).
    Myself,
    /// A project's issue search.
    Search,
}

/// A non-fatal problem hit while syncing one project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncIssue {
    /// The project key, or empty for a problem not specific to one project (`/myself`).
    pub project: String,
    /// Which resource.
    pub resource: Resource,
    /// A short, non-sensitive description (never includes the credential).
    pub message: String,
}

/// Everything one `sync` call produced.
#[derive(Debug)]
pub struct SyncOutcome {
    /// The new state; the caller persists this for next time.
    pub state: SyncState,
    /// What changed upstream, across every project reached before any rate limit.
    pub changes: Vec<UpstreamChange>,
    /// Set when a rate limit stopped the sync before every project was reached. Nothing later in
    /// `config.projects` (in iteration order) was contacted.
    pub rate_limited: Option<RateLimited>,
    /// Non-fatal problems. A project that hits one still tries to persist whatever it already
    /// collected this call.
    pub errors: Vec<SyncIssue>,
    /// How many items across the whole call failed to parse, or carried a malformed timestamp,
    /// and were skipped.
    pub malformed_skipped: u32,
}

fn fields_for(config: &SyncConfig) -> Vec<&str> {
    let mut fields = BASE_FIELDS.to_vec();
    if let Some(custom) = &config.epic_link_field {
        fields.push(custom.as_str());
    }
    fields
}

/// Syncs every project in `config.projects`, in order, stopping as soon as any is rate-limited
/// (a Jira token's rate limit applies to the whole site, so there is no point trying the next
/// project). Projects already synced before that point keep their results.
pub async fn sync<T: Transport, D: Deployment>(
    state: SyncState,
    transport: &T,
    deployment: &D,
    config: &SyncConfig,
) -> SyncOutcome {
    sync_with_limits(state, transport, deployment, config, Limits::default()).await
}

/// `sync`'s actual implementation, parameterised over the page/item caps — see
/// `pitcrew_sync_github::sync`'s identical split for why.
async fn sync_with_limits<T: Transport, D: Deployment>(
    mut state: SyncState,
    transport: &T,
    deployment: &D,
    config: &SyncConfig,
    limits: Limits,
) -> SyncOutcome {
    let mut changes = Vec::new();
    let mut errors = Vec::new();
    let mut malformed_skipped = 0u32;
    let mut rate_limited = None;

    let client = JiraClient::new(transport, config.auth.clone(), config.api_base.clone());

    // Read fresh on every call, not cached: see `SyncState::timezone`'s doc for why a stale zone
    // is unsafe here.
    let resolved_zone = match client.myself().await {
        Ok(tz) => {
            state.timezone = tz;
            resolve_account_zone(state.timezone.as_deref())
        }
        Err(e) => {
            errors.push(SyncIssue {
                project: String::new(),
                resource: Resource::Myself,
                message: e.to_string(),
            });
            // Round 3 review item S-1: do NOT fall back to using `state.timezone`'s cached value
            // to render *this* call's queries. The account's real zone can have moved since the
            // last successful read (e.g. Tokyo to New York); rendering at a stale cached zone
            // that is now *behind* the true one narrows or inverts the cursor's safety margin and
            // can skip real updates. UTC-12 is behind every real zone, so rendering at it can only
            // widen the window (more overlap re-read, never a skip). `state.timezone` itself is
            // left untouched, so a later call is unaffected by this one's failure once `/myself`
            // succeeds again.
            resolve_account_zone(None)
        }
    };

    let fields = fields_for(config);
    if resolved_zone.fell_back {
        errors.push(SyncIssue {
            project: String::new(),
            resource: Resource::Myself,
            message: "could not resolve the Jira account's time zone; using a conservative \
                       UTC-12 fallback, which makes the sync cursor less precise"
                .to_string(),
        });
    }
    let zone = resolved_zone.zone;
    let margin = SignedDuration::from_hours(CURSOR_SAFETY_MARGIN_HOURS);

    for project in &config.projects {
        let key = project.as_str().to_string();
        let mut project_state = state.projects.remove(&key).unwrap_or_default();
        let mut attempts = project_state.secondary_backoff_attempts;

        // The stored cursor is RFC 3339 instant text (see `ProjectState::cursor`'s doc). A value
        // that fails to parse — including the older pre-rendered local-time format, which this
        // crate can no longer safely reinterpret — is treated exactly like no cursor at all: one
        // fresh full sync for this project, rather than risk guessing at it.
        let cursor_instant: Option<Timestamp> =
            project_state.cursor.as_deref().and_then(|s| s.parse().ok());
        // Rendered `margin` earlier than the real cursor: covers tzdata drift between this
        // crate's bundled database and Jira's own, and any residual zone-resolution imprecision.
        // Skipped entirely when `resume_without_margin` is set (round 3 review item S-3): the
        // previous call reported `stuck_window_exhausted`, and re-subtracting the margin here
        // would just pull the query back into the same already-exhausted window, making the stall
        // permanent — see `ProjectState::resume_without_margin`'s doc.
        let query_instant = if project_state.resume_without_margin {
            cursor_instant
        } else {
            cursor_instant.and_then(|c| c.checked_sub(margin).ok())
        };
        let cursor_text = query_instant.map(|i| account_minute(i, &zone));
        let jql = incremental_query(project, cursor_text.as_deref());
        let query = SearchQuery {
            jql: &jql,
            fields: &fields,
            old_cursor_instant: cursor_instant,
        };

        let result = client
            .search(deployment, &query, config.now_unix, &mut attempts, limits)
            .await;
        project_state.secondary_backoff_attempts = attempts;

        match result {
            Err(e) => errors.push(SyncIssue {
                project: key.clone(),
                resource: Resource::Search,
                message: e.to_string(),
            }),
            Ok(Outcome::RateLimited(rl)) => {
                state.projects.insert(key, project_state);
                rate_limited = Some(rl);
                break;
            }
            Ok(Outcome::Ok(search_result)) => {
                malformed_skipped += search_result.malformed_skipped;
                for issue in &search_result.items {
                    if issue.is_epic() {
                        let previous = project_state.epic_snapshots.get(&issue.key);
                        match diff_epic(&config.site_base, issue, previous) {
                            Some((mut found, snapshot)) => {
                                changes.append(&mut found);
                                project_state
                                    .epic_snapshots
                                    .insert(issue.key.clone(), snapshot);
                            }
                            None => malformed_skipped += 1,
                        }
                    } else {
                        let previous = project_state.issue_snapshots.get(&issue.key);
                        match diff_issue(
                            &config.site_base,
                            issue,
                            previous,
                            config.epic_link_field.as_deref(),
                        ) {
                            Some((mut found, snapshot)) => {
                                changes.append(&mut found);
                                project_state
                                    .issue_snapshots
                                    .insert(issue.key.clone(), snapshot);
                            }
                            None => malformed_skipped += 1,
                        }
                    }
                }
                project_state.resume_without_margin = search_result.stuck_window_exhausted;
                if search_result.stuck_window_exhausted {
                    errors.push(SyncIssue {
                        project: key.clone(),
                        resource: Resource::Search,
                        message: "more updates share the sync cursor's window than one call's \
                                   page budget can read; the next sync resumes from exactly where \
                                   this one stopped (no added safety margin) to guarantee progress"
                            .to_string(),
                    });
                }
                // Advance the cursor to the newest instant actually seen. This is unconditional
                // (not gated on "every page was fetched"), the same way sync-github's *issues*
                // cursor is: it is always safe to advance to the newest item actually processed,
                // because the walk is ascending with a server-side `updated >=` filter — anything
                // older than what this call saw is guaranteed either already synced, or still
                // `>=` the *old* cursor and so still matched by this same JQL next time. A cap
                // cutting the walk short is *not* automatically "next call repeats the same query
                // and makes a little more progress" the way that sentence implies: the margin
                // subtracted when rendering a query (above) would normally pull the next call's
                // query right back into this same already-exhausted window, making the stall
                // permanent rather than merely slow — that is exactly what
                // `project_state.resume_without_margin`, just set above, exists to prevent (round
                // 3 review item S-3; this comment itself was the "misleading" one review flagged).
                if let Some(candidate) = search_result.max_instant
                    && cursor_instant.is_none_or(|c| candidate > c)
                {
                    project_state.cursor = Some(candidate.to_string());
                }
            }
        }
        state.projects.insert(key, project_state);
    }

    SyncOutcome {
        state,
        changes,
        rate_limited,
        errors,
        malformed_skipped,
    }
}

/// Validates a batch of raw project keys, for callers building a [`SyncConfig`] from
/// configuration. Not used by `sync` itself (which only ever sees already-valid [`ProjectRef`]s);
/// a convenience for callers.
pub fn validate_projects(
    keys: impl IntoIterator<Item = impl Into<String>>,
) -> Result<Vec<ProjectRef>, InvalidProjectRef> {
    keys.into_iter().map(ProjectRef::new).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deployment::JiraCloud;
    use pitcrew_sync_github::fixture::{RecordedExchange, ReplayTransport};

    fn config(projects: Vec<&str>) -> SyncConfig {
        SyncConfig {
            projects: projects
                .into_iter()
                .map(|p| ProjectRef::new(p).expect("valid"))
                .collect(),
            auth: JiraAuth::Bearer {
                token: "pat-test-not-real".to_string(),
            },
            api_base: "https://jira.example.com/rest/api/3".to_string(),
            site_base: "https://jira.example.com".to_string(),
            epic_link_field: None,
            now_unix: 2_000_000_000,
        }
    }

    fn myself_exchange() -> RecordedExchange {
        RecordedExchange {
            method: "GET".to_string(),
            url: "https://jira.example.com/rest/api/3/myself".to_string(),
            request_headers: vec![],
            status: 200,
            response_headers: vec![],
            body: br#"{"timeZone":"UTC"}"#.to_vec(),
        }
    }

    fn empty_search(url: &str) -> RecordedExchange {
        RecordedExchange {
            method: "GET".to_string(),
            url: url.to_string(),
            request_headers: vec![],
            status: 200,
            response_headers: vec![],
            body: br#"{"issues":[]}"#.to_vec(),
        }
    }

    #[tokio::test]
    async fn myself_is_read_fresh_on_every_sync() {
        // Round 2 review item B1: a cached zone combined with an instant-based cursor silently
        // reintroduces the skip if the account's profile zone ever changes between syncs, so
        // `/myself` is read on every call now — both calls here need their own fixture.
        let search_url = "https://jira.example.com/rest/api/3/search/jql?jql=project%20in%20%28%22DEMO%22%29%20ORDER%20BY%20updated%20ASC%2C%20key%20ASC&maxResults=100&fields=summary%2Cdescription%2Cstatus%2Cresolution%2Clabels%2Cassignee%2Cparent%2Cissuetype%2Cupdated";
        let transport =
            ReplayTransport::from_exchanges(vec![myself_exchange(), empty_search(search_url)]);
        let outcome = sync(
            SyncState::new(),
            &transport,
            &JiraCloud,
            &config(vec!["DEMO"]),
        )
        .await;
        assert!(outcome.errors.is_empty(), "{:?}", outcome.errors);
        assert_eq!(outcome.state.timezone.as_deref(), Some("UTC"));
        assert_eq!(transport.remaining(), 0);

        // Second call: /myself is requested again — if it were not, the transport would error on
        // an unmatched request for the search (which needs the zone resolved first).
        let transport2 =
            ReplayTransport::from_exchanges(vec![myself_exchange(), empty_search(search_url)]);
        let outcome2 = sync(
            outcome.state,
            &transport2,
            &JiraCloud,
            &config(vec!["DEMO"]),
        )
        .await;
        assert!(outcome2.errors.is_empty(), "{:?}", outcome2.errors);
        assert_eq!(transport2.remaining(), 0);
    }

    #[tokio::test]
    async fn a_stuck_window_resumes_with_no_margin_and_makes_progress_next_call() {
        // Round 3 review item S-3: without `resume_without_margin`, a call reporting
        // `stuck_window_exhausted` would advance its cursor only to the newest instant it
        // actually read (still inside the old margin window), and the *next* call would then
        // subtract the safety margin again when rendering its query — pulling the window's start
        // right back to (or before) where it already was, making the stall permanent rather than
        // merely slow. This proves the second call instead queries from exactly where the first
        // stopped, with no margin re-subtracted, and that real progress follows.
        let zone = jiff::tz::TimeZone::UTC;
        let project_ref = ProjectRef::new("DEMO").expect("valid");
        let limits = Limits {
            max_pages: 1,
            max_items: 10,
        };

        // --- Call 1: a project with a cursor from a prior sync gets stuck — two items, both
        // within the 1-hour margin, and a `nextPageToken` showing a real further page exists,
        // which the 1-page budget never reaches. ---
        let old_cursor: Timestamp = "2026-01-01T00:05:00Z".parse().expect("valid instant");
        let margin = SignedDuration::from_hours(CURSOR_SAFETY_MARGIN_HOURS);
        let query1_instant = old_cursor.checked_sub(margin).expect("no underflow");
        let cursor1_text = account_minute(query1_instant, &zone);
        let jql1 = incremental_query(&project_ref, Some(cursor1_text.as_str()));
        let search_url1 = JiraCloud
            .build_search_request(
                "https://jira.example.com/rest/api/3",
                &config(vec!["DEMO"]).auth,
                &jql1,
                BASE_FIELDS,
                &crate::deployment::PageState::Cloud {
                    next_page_token: None,
                },
            )
            .url;
        let page1 = serde_json::json!({
            "issues": [
                {"id":"1","key":"DEMO-1","fields":{"summary":"a","status":{"name":"To Do","statusCategory":{"key":"new"}},"issuetype":{"name":"Story"},"updated":"2026-01-01T00:10:00.000+0000"}},
                {"id":"2","key":"DEMO-2","fields":{"summary":"b","status":{"name":"To Do","statusCategory":{"key":"new"}},"issuetype":{"name":"Story"},"updated":"2026-01-01T00:20:00.000+0000"}},
            ],
            "nextPageToken": "page2",
        });
        let transport1 = ReplayTransport::from_exchanges(vec![
            myself_exchange(),
            RecordedExchange {
                method: "GET".to_string(),
                url: search_url1,
                request_headers: vec![],
                status: 200,
                response_headers: vec![],
                body: page1.to_string().into_bytes(),
            },
        ]);

        let mut state = SyncState::new();
        state.projects.insert(
            "DEMO".to_string(),
            crate::state::ProjectState {
                cursor: Some(old_cursor.to_string()),
                ..Default::default()
            },
        );

        let outcome1 = sync_with_limits(
            state,
            &transport1,
            &JiraCloud,
            &config(vec!["DEMO"]),
            limits,
        )
        .await;
        assert!(
            outcome1
                .errors
                .iter()
                .any(|e| e.resource == Resource::Search),
            "the stuck window must be surfaced: {:?}",
            outcome1.errors
        );
        let project1 = outcome1.state.projects.get("DEMO").expect("project state");
        assert!(
            project1.resume_without_margin,
            "the next call must resume without the margin"
        );
        assert_eq!(project1.cursor.as_deref(), Some("2026-01-01T00:20:00Z"));

        // --- Call 2: must query from the cursor itself, not cursor-minus-margin. If it used the
        // margin again (the bug this fixes), it would query from "2025-12-31 23:20" instead,
        // which the fixture below provides no response for — the call would error on an
        // unmatched request, which is exactly what would catch a regression here. ---
        let cursor2: Timestamp = "2026-01-01T00:20:00Z".parse().expect("valid instant");
        let cursor2_text = account_minute(cursor2, &zone);
        let jql2 = incremental_query(&project_ref, Some(cursor2_text.as_str()));
        let search_url2 = JiraCloud
            .build_search_request(
                "https://jira.example.com/rest/api/3",
                &config(vec!["DEMO"]).auth,
                &jql2,
                BASE_FIELDS,
                &crate::deployment::PageState::Cloud {
                    next_page_token: None,
                },
            )
            .url;
        let page2 = serde_json::json!({
            "issues": [
                {"id":"3","key":"DEMO-3","fields":{"summary":"c","status":{"name":"To Do","statusCategory":{"key":"new"}},"issuetype":{"name":"Story"},"updated":"2026-01-01T03:00:00.000+0000"}},
            ],
        });
        let transport2 = ReplayTransport::from_exchanges(vec![
            myself_exchange(),
            RecordedExchange {
                method: "GET".to_string(),
                url: search_url2,
                request_headers: vec![],
                status: 200,
                response_headers: vec![],
                body: page2.to_string().into_bytes(),
            },
        ]);
        let outcome2 = sync_with_limits(
            outcome1.state,
            &transport2,
            &JiraCloud,
            &config(vec!["DEMO"]),
            limits,
        )
        .await;
        assert!(outcome2.errors.is_empty(), "{:?}", outcome2.errors);
        assert_eq!(
            transport2.remaining(),
            0,
            "the no-margin query must have matched the fixture"
        );
        let created: Vec<&str> = outcome2
            .changes
            .iter()
            .filter_map(|c| match c {
                UpstreamChange::IssueCreated { source, .. } => Some(source.key.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(created, vec!["DEMO-3"], "{:#?}", outcome2.changes);
        let project2 = outcome2.state.projects.get("DEMO").expect("project state");
        assert!(
            !project2.resume_without_margin,
            "progress was made; the next call no longer needs to skip the margin"
        );
        assert_eq!(project2.cursor.as_deref(), Some("2026-01-01T03:00:00Z"));
    }

    #[tokio::test]
    async fn a_myself_fetch_failure_renders_this_calls_query_at_utc_minus_twelve_not_the_cached_zone()
     {
        // Round 3 review item S-1: a `/myself` failure must NOT fall back to the last known-good
        // *cached* zone to render that call's own query. The account's real zone can have moved
        // since the last successful read (e.g. Tokyo to New York); rendering at a stale cached
        // zone that is now behind the true one narrows (or inverts) the cursor's safety margin and
        // can skip real updates. This renders the failed call's query at the conservative UTC-12
        // fallback instead — always behind every real zone, so it can only widen the window, never
        // skip — while still keeping the cached zone itself in `state.timezone` untouched, for the
        // *next* call to use once `/myself` succeeds again (asserted below).
        //
        // The first call resolves and caches "UTC", and sees one issue, establishing a cursor.
        let search_url = "https://jira.example.com/rest/api/3/search/jql?jql=project%20in%20%28%22DEMO%22%29%20ORDER%20BY%20updated%20ASC%2C%20key%20ASC&maxResults=100&fields=summary%2Cdescription%2Cstatus%2Cresolution%2Clabels%2Cassignee%2Cparent%2Cissuetype%2Cupdated";
        let first_page = serde_json::json!({
            "issues": [{
                "id": "1",
                "key": "DEMO-1",
                "fields": {
                    "summary": "a",
                    "status": {"name": "To Do", "statusCategory": {"key": "new"}},
                    "issuetype": {"name": "Story"},
                    "updated": "2026-01-01T00:05:00.000+0000",
                },
            }],
        });
        let transport = ReplayTransport::from_exchanges(vec![
            myself_exchange(),
            RecordedExchange {
                method: "GET".to_string(),
                url: search_url.to_string(),
                request_headers: vec![],
                status: 200,
                response_headers: vec![],
                body: first_page.to_string().into_bytes(),
            },
        ]);
        let outcome = sync(
            SyncState::new(),
            &transport,
            &JiraCloud,
            &config(vec!["DEMO"]),
        )
        .await;
        assert_eq!(outcome.state.timezone.as_deref(), Some("UTC"));
        let project = outcome.state.projects.get("DEMO").expect("project state");
        assert_eq!(project.cursor.as_deref(), Some("2026-01-01T00:05:00Z"));

        // Second call: /myself fails. The margin-adjusted cursor (1 hour earlier) is
        // "2025-12-31T23:05:00Z"; rendered at UTC-12 (not the cached "UTC"), that is
        // "2025-12-31 11:05" — had this used the stale cached "UTC" zone instead, it would have
        // queried from "2025-12-31 23:05", a different query this test provides no fixture for.
        let query = incremental_query(
            &ProjectRef::new("DEMO").expect("valid"),
            Some("2025-12-31 11:05"),
        );
        let second_url = JiraCloud
            .build_search_request(
                "https://jira.example.com/rest/api/3",
                &config(vec!["DEMO"]).auth,
                &query,
                BASE_FIELDS,
                &crate::deployment::PageState::Cloud {
                    next_page_token: None,
                },
            )
            .url;

        let failing_myself = RecordedExchange {
            method: "GET".to_string(),
            url: "https://jira.example.com/rest/api/3/myself".to_string(),
            request_headers: vec![],
            status: 500,
            response_headers: vec![],
            body: Vec::new(),
        };
        let transport2 =
            ReplayTransport::from_exchanges(vec![failing_myself, empty_search(&second_url)]);
        let outcome2 = sync(
            outcome.state,
            &transport2,
            &JiraCloud,
            &config(vec!["DEMO"]),
        )
        .await;
        // Two distinct issues this call: the /myself fetch itself failing, and (as a consequence)
        // this call rendering at the conservative UTC-12 fallback.
        assert_eq!(
            outcome2
                .errors
                .iter()
                .filter(|e| e.resource == Resource::Myself)
                .count(),
            2,
            "{:?}",
            outcome2.errors
        );
        // The stale-but-still-valid "UTC" is kept in state, not wiped out — it is only this one
        // call's own *query* that must not use it.
        assert_eq!(outcome2.state.timezone.as_deref(), Some("UTC"));
        assert_eq!(
            transport2.remaining(),
            0,
            "the search must have used the UTC-12 fallback for this call, not the cached UTC zone"
        );
    }

    #[test]
    fn validate_projects_rejects_an_invalid_key() {
        assert!(validate_projects(["DEMO", "bad key"]).is_err());
        assert!(validate_projects(["DEMO", "OTHER"]).is_ok());
    }
}
