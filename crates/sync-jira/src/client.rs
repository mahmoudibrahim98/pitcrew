//! The client core: search pagination over a [`Transport`], shared by both deployments through
//! [`Deployment`], plus `/myself` (identical on both, so it needs no deployment-specific
//! handling at all).
//!
//! Unlike `pitcrew_sync_github`'s client, there are no conditional requests here: Jira's search
//! has no `ETag`/`304` equivalent this crate uses, so incremental reads rely only on the JQL
//! `updated >=` cursor (see [`crate::jql`]).

use crate::auth::JiraAuth;
use crate::bounds::{
    CURSOR_SAFETY_MARGIN_HOURS, MAX_ITEMS_PER_SYNC, MAX_PAGE_BODY_BYTES, MAX_PAGES_PER_CALL,
    backoff_secs,
};
use crate::deployment::Deployment;
use crate::time::JiraTimestamp;
use crate::wire::WireIssue;
use jiff::Timestamp;
use pitcrew_sync_github::transport::{Method, Request, Transport, TransportError};
use std::collections::HashSet;

/// Errors the client core reports. None of these carry the credential.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// The transport itself failed.
    #[error(transparent)]
    Transport(#[from] TransportError),
    /// An HTTP status this client does not know how to handle (429 is handled separately).
    #[error("unexpected status {status} from {url}")]
    Status {
        /// The URL.
        url: String,
        /// The status code.
        status: u16,
    },
}

/// The token is rate-limited; try again no sooner than `until` (Unix seconds).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateLimited {
    /// Unix seconds.
    pub until: i64,
}

/// The result of a request that might have been rate-limited instead of answered.
#[derive(Debug)]
pub(crate) enum Outcome<T> {
    Ok(T),
    RateLimited(RateLimited),
}

/// Caps on one `search` call: how many pages to follow, and how many items to collect. Production
/// code always uses [`Limits::default`]; this crate's own tests override it to exercise
/// cap-triggered truncation without multi-thousand-item fixtures.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Limits {
    pub max_pages: usize,
    pub max_items: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_pages: MAX_PAGES_PER_CALL,
            max_items: MAX_ITEMS_PER_SYNC,
        }
    }
}

/// What to search and how: grouped into one struct (rather than separate `search` parameters) to
/// stay under clippy's argument-count lint, and because these genuinely travel together — `fields`
/// and `old_cursor_instant` only make sense alongside the `jql` they were built from.
#[derive(Clone, Copy)]
pub(crate) struct SearchQuery<'a> {
    pub jql: &'a str,
    pub fields: &'a [&'a str],
    /// The cursor `jql` was built from (already rendered into it, minus the safety margin;
    /// `None` on a first sync) — the **bare** instant, not margin-adjusted. `search` adds
    /// [`CURSOR_SAFETY_MARGIN_HOURS`] itself when deciding whether the item cap's bypass (see its
    /// doc) should still apply, the same margin [`crate::sync`] subtracts when rendering `jql`'s
    /// own `>=` bound — so both operations move outward from the one stored value, instead of one
    /// of them silently compounding the other's adjustment.
    pub old_cursor_instant: Option<Timestamp>,
}

/// One `search` call's outcome: successfully parsed issues, plus bookkeeping for the caller's
/// cursor.
#[derive(Debug)]
pub(crate) struct SearchResult {
    pub items: Vec<WireIssue>,
    pub malformed_skipped: u32,
    /// The latest `fields.updated`, as an instant, seen in this call across every page fetched.
    /// The caller renders this (see [`crate::time::account_minute`]) only when it next needs to
    /// build a query — never stored pre-rendered, so a later change to the account's own zone is
    /// reflected immediately rather than silently baked into a stale cursor.
    pub max_instant: Option<Timestamp>,
    /// `true` if the item cap's bypass (see [`JiraClient::search`]'s doc) was still engaged —
    /// every item collected so far was still at or before the cursor instant plus the safety
    /// margin — when `limits.max_pages` ran out. More updates share that window than one call's
    /// page budget can read, so this project did not make progress past it *this* call; the
    /// caller surfaces this as a `SyncIssue` rather than failing silently, and sets
    /// `ProjectState::resume_without_margin` so the *next* call resumes from exactly this call's
    /// cursor with no margin re-subtracted, guaranteeing it is not the same already-exhausted
    /// window again (round 3 review item S-3 — this used to be a genuine permanent-stall bug, not
    /// merely a slow one). No *mid-window page-walk* continuation (persisting `startAt`/
    /// `nextPageToken` itself) is implemented — see this crate's README for that narrower gap.
    pub stuck_window_exhausted: bool,
}

/// A Jira REST client over any [`Transport`], generic over the deployment ([`Deployment`]) each
/// call is made against — the client core itself (pagination, bounds, rate limits) does not
/// change between Cloud and Data Center, only how a page is requested and parsed.
pub struct JiraClient<'t, T: Transport> {
    transport: &'t T,
    auth: JiraAuth,
    api_base: String,
}

impl<T: Transport> std::fmt::Debug for JiraClient<'_, T> {
    // Manual: `transport` is generic and not required to be `Debug`; `auth` already redacts
    // itself.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JiraClient")
            .field("api_base", &self.api_base)
            .field("auth", &self.auth)
            .finish_non_exhaustive()
    }
}

impl<'t, T: Transport> JiraClient<'t, T> {
    /// Builds a client over `transport`, authenticating with `auth`, against `api_base` (the full
    /// REST root, e.g. `https://jira.example.com/rest/api/3`).
    #[must_use]
    pub fn new(transport: &'t T, auth: JiraAuth, api_base: impl Into<String>) -> Self {
        Self {
            transport,
            auth,
            api_base: api_base.into(),
        }
    }

    /// Reads the account's time zone from `/myself`. Identical on both deployments, so this takes
    /// no [`Deployment`] parameter.
    pub(crate) async fn myself(&self) -> Result<Option<String>, ClientError> {
        let request = Request {
            method: Method::Get,
            url: format!("{}/myself", self.api_base),
            headers: vec![
                ("Accept".to_string(), "application/json".to_string()),
                ("Authorization".to_string(), self.auth.header_value()),
            ],
            body: Vec::new(),
        };
        let url = request.url.clone();
        let response = self.transport.send(request).await?;
        if response.status != 200 {
            return Err(ClientError::Status {
                url,
                status: response.status,
            });
        }
        let myself: crate::wire::WireMyself = serde_json::from_slice(&response.body)
            .unwrap_or(crate::wire::WireMyself { time_zone: None });
        Ok(myself.time_zone)
    }

    /// Runs `query.jql`, following pagination through `deployment` up to `limits.max_pages`/
    /// `limits.max_items`, honouring a 429 rate limit.
    ///
    /// `query.old_cursor_instant` is the cursor `query.jql` was already built from (`None` on a
    /// first sync). When the item cap is reached but every item collected so far is still at or
    /// before that instant *plus* [`CURSOR_SAFETY_MARGIN_HOURS`], the cap is **not** enforced —
    /// pagination keeps going (still bounded by `limits.max_pages`) until an item past that
    /// threshold is seen, or the data runs out. Without this, a project with more updates in one
    /// window than `limits.max_items` would see its cursor get stuck there forever: every call
    /// would re-fetch the identical capped batch and never make progress (a livelock).
    /// Re-fetching already-seen items the rest of the time is harmless — diffing against the
    /// stored snapshot (see `crate::sync`) makes that a no-op. If the page budget *also* runs out
    /// before an item past the threshold is seen, [`SearchResult::stuck_window_exhausted`] is set
    /// — this one call could not resolve it, and the caller surfaces that visibly.
    pub(crate) async fn search<D: Deployment>(
        &self,
        deployment: &D,
        query: &SearchQuery<'_>,
        now_unix: i64,
        attempts: &mut u32,
        limits: Limits,
    ) -> Result<Outcome<SearchResult>, ClientError> {
        let SearchQuery {
            jql,
            fields,
            old_cursor_instant,
        } = *query;
        // The bypass threshold: `None` (first sync) never bypasses. `search_result.stuck_window_exhausted`
        // tracks whether the walk was still under this threshold when it ran out of page budget.
        let threshold = old_cursor_instant.and_then(|c| {
            c.checked_add(jiff::SignedDuration::from_hours(CURSOR_SAFETY_MARGIN_HOURS))
                .ok()
        });
        let mut page = deployment.first_page();
        let mut items = Vec::new();
        let mut malformed = 0u32;
        let mut max_instant: Option<Timestamp> = None;
        // Dedupes an issue appearing twice within this one call — e.g. offset-based pagination
        // (Data Center) can repeat or skip a row when an item is updated concurrently with the
        // walk shifting it across a page boundary. Across *calls*, the same overlap is handled by
        // diffing against the stored snapshot (see `crate::sync`), which is naturally a no-op when
        // nothing actually changed; this only guards one call's own page walk.
        let mut seen_this_call: HashSet<(String, String)> = HashSet::new();
        // Cleared by every path that ends the walk for a reason *other* than exhausting
        // `limits.max_pages` (a clean natural completion, a non-stuck cap hit, a malformed or
        // oversized page). Only staying `true` all the way to the end of the `for` loop means the
        // page budget itself was the limiting factor.
        let mut ran_out_of_pages = true;

        for _ in 0..limits.max_pages {
            let request =
                deployment.build_search_request(&self.api_base, &self.auth, jql, fields, &page);
            let url = request.url.clone();
            let response = self.transport.send(request).await?;

            if response.status == 429 {
                let retry_after = response
                    .header("retry-after")
                    .and_then(|v| v.trim().parse::<i64>().ok());
                let until = if let Some(retry_after) = retry_after {
                    now_unix + retry_after
                } else {
                    *attempts = attempts.saturating_add(1);
                    now_unix + backoff_secs(*attempts)
                };
                return Ok(Outcome::RateLimited(RateLimited { until }));
            }
            if response.status != 200 {
                return Err(ClientError::Status {
                    url,
                    status: response.status,
                });
            }
            *attempts = 0;

            if response.body.len() > MAX_PAGE_BODY_BYTES {
                // Oversized page: stop here (keeping whatever was already collected) rather than
                // parsing a huge document. There is no safe resume point for it, so the next
                // call's unchanged cursor simply re-walks from the top — see `crate::sync`.
                tracing::warn!(url = %url, len = response.body.len(), "oversized search response page, stopping this call");
                malformed += 1;
                ran_out_of_pages = false;
                break;
            }
            let Ok((raw_items, next)) = deployment.parse_search_page(&response.body) else {
                tracing::warn!(url = %url, "could not parse a search response page, stopping this call");
                malformed += 1;
                ran_out_of_pages = false;
                break;
            };

            let mut cap_hit = false;
            for value in raw_items {
                if items.len() >= limits.max_items {
                    // See this method's doc: only actually stop if the walk has already moved
                    // past the cursor-plus-margin threshold. `max_instant` reflects the most
                    // recently processed item (items arrive in ascending `updated` order), so
                    // this is exactly "has every item so far stayed within the stuck window".
                    //
                    // Scoped to just this check, not also reused to decide
                    // `SearchResult::stuck_window_exhausted` (see `still_stuck_at_end`, computed
                    // fresh after the whole loop): doing that had two bugs (round 3 review item
                    // S-3 and a related nit). First, a silent false negative — this only ever runs
                    // *inside* this branch, so a call whose page budget runs out without the item
                    // cap ever being reached (e.g. the server returns fewer than
                    // `limits.max_items` items across all of `limits.max_pages`) would never
                    // compute it at all, under-reporting a genuinely stuck window. Second, a false
                    // positive — a value computed here can go stale: if the very item that would
                    // finally cross the threshold turns out to be the last item this call ever
                    // reads, nothing re-runs this check afterward to notice the crossing.
                    let cap_bypass_engaged = match (max_instant, threshold) {
                        (Some(seen), Some(t)) => seen <= t,
                        _ => false,
                    };
                    if !cap_bypass_engaged {
                        cap_hit = true;
                        ran_out_of_pages = false;
                        break;
                    }
                }
                match serde_json::from_value::<WireIssue>(value) {
                    Ok(issue) => {
                        // Dedupe by issue id plus `updated`, as the brief asks: offset-based
                        // pagination (Data Center) can repeat a row when an item is updated
                        // concurrently with the walk shifting it across a page boundary.
                        if !seen_this_call.insert((issue.id.clone(), issue.fields.updated.clone()))
                        {
                            continue;
                        }
                        let ts = JiraTimestamp::new(&issue.fields.updated);
                        if let Some(instant) = ts.to_instant()
                            && max_instant.is_none_or(|m| instant > m)
                        {
                            max_instant = Some(instant);
                        }
                        items.push(issue);
                    }
                    Err(_) => {
                        tracing::debug!(url = %url, "skipped a malformed issue");
                        malformed += 1;
                    }
                }
            }
            if cap_hit {
                break;
            }
            match next {
                Some(next_page) => page = next_page,
                None => {
                    ran_out_of_pages = false;
                    break;
                }
            }
        }

        // Computed fresh here from the *final* `max_instant`, not from `cap_bypass_engaged`'s
        // last in-loop value — see that variable's doc for the two bugs reusing it caused (round
        // 3 review item S-3 and a related nit). `max_instant` reflects every item this call
        // actually processed (items arrive in ascending `updated` order, so the max is always the
        // most recent one read, regardless of which branch last updated it), so this is correct
        // regardless of whether the item cap ever actually triggered mid-loop.
        let still_stuck_at_end = match (max_instant, threshold) {
            (Some(seen), Some(t)) => seen <= t,
            _ => false,
        };

        Ok(Outcome::Ok(SearchResult {
            items,
            malformed_skipped: malformed,
            max_instant,
            stuck_window_exhausted: ran_out_of_pages && still_stuck_at_end,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deployment::JiraCloud;
    use pitcrew_sync_github::fixture::{RecordedExchange, ReplayTransport};

    fn exchange(
        url: &str,
        status: u16,
        headers: Vec<(&str, &str)>,
        body: &str,
    ) -> RecordedExchange {
        RecordedExchange {
            method: "GET".to_string(),
            url: url.to_string(),
            request_headers: vec![],
            status,
            response_headers: headers
                .into_iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            body: body.as_bytes().to_vec(),
        }
    }

    fn client(transport: &ReplayTransport) -> JiraClient<'_, ReplayTransport> {
        JiraClient::new(
            transport,
            JiraAuth::Bearer {
                token: "pat-test-not-real".to_string(),
            },
            "https://jira.example.com/rest/api/3",
        )
    }

    fn query<'a>(
        jql: &'a str,
        fields: &'a [&'a str],
        old_cursor_instant: Option<Timestamp>,
    ) -> SearchQuery<'a> {
        SearchQuery {
            jql,
            fields,
            old_cursor_instant,
        }
    }

    fn instant(s: &str) -> Timestamp {
        s.parse().expect("valid RFC 3339 instant")
    }

    #[test]
    fn client_error_display_never_carries_the_credential() {
        let err = ClientError::Status {
            url: "https://jira.example.com/rest/api/3/search/jql".into(),
            status: 500,
        };
        let shown = format!("{err}");
        assert!(!shown.to_lowercase().contains("bearer"));
        assert!(!shown.to_lowercase().contains("token"));
    }

    #[tokio::test]
    async fn a_429_with_retry_after_is_rate_limited_without_sleeping() {
        let url = "https://jira.example.com/rest/api/3/search/jql?jql=project%20in%20%28%22DEMO%22%29&maxResults=100&fields=summary";
        let transport = ReplayTransport::from_exchanges(vec![exchange(
            url,
            429,
            vec![("Retry-After", "30")],
            "",
        )]);
        let c = client(&transport);
        let mut attempts = 0u32;
        let Outcome::RateLimited(rl) = c
            .search(
                &JiraCloud,
                &query("project in (\"DEMO\")", &["summary"], None),
                2_000_000_000,
                &mut attempts,
                Limits::default(),
            )
            .await
            .expect("search")
        else {
            panic!("expected RateLimited");
        };
        assert_eq!(rl.until, 2_000_000_030);
    }

    #[tokio::test]
    async fn a_429_without_retry_after_backs_off_and_persists_attempts() {
        let url = "https://jira.example.com/rest/api/3/search/jql?jql=project%20in%20%28%22DEMO%22%29&maxResults=100&fields=summary";
        let mut attempts = 0u32;
        let mut deadlines = Vec::new();
        for _ in 0..3 {
            let transport = ReplayTransport::from_exchanges(vec![exchange(url, 429, vec![], "")]);
            let c = client(&transport);
            let Outcome::RateLimited(rl) = c
                .search(
                    &JiraCloud,
                    &query("project in (\"DEMO\")", &["summary"], None),
                    2_000_000_000,
                    &mut attempts,
                    Limits::default(),
                )
                .await
                .expect("search")
            else {
                panic!("expected RateLimited");
            };
            deadlines.push(rl.until);
        }
        assert!(deadlines[0] < deadlines[1]);
        assert!(deadlines[1] < deadlines[2]);
        assert_eq!(attempts, 3);
    }

    #[tokio::test]
    async fn a_non_rate_limit_error_status_is_a_client_error() {
        let url = "https://jira.example.com/rest/api/3/search/jql?jql=project%20in%20%28%22DEMO%22%29&maxResults=100&fields=summary";
        let transport = ReplayTransport::from_exchanges(vec![exchange(
            url,
            401,
            vec![],
            r#"{"errorMessages":["Unauthorized"]}"#,
        )]);
        let c = client(&transport);
        let mut attempts = 0u32;
        let err = c
            .search(
                &JiraCloud,
                &query("project in (\"DEMO\")", &["summary"], None),
                2_000_000_000,
                &mut attempts,
                Limits::default(),
            )
            .await
            .expect_err("401 is not a rate limit");
        assert!(matches!(err, ClientError::Status { status: 401, .. }));
    }

    #[tokio::test]
    async fn an_item_cap_mid_page_truncates_without_following_further_pages() {
        let url = "https://jira.example.com/rest/api/3/search/jql?jql=project%20in%20%28%22DEMO%22%29&maxResults=100&fields=summary";
        let body = serde_json::json!({
            "issues": [
                {"id":"1","key":"DEMO-1","fields":{"summary":"a","status":{"name":"To Do","statusCategory":{"key":"new"}},"issuetype":{"name":"Story"},"updated":"2026-01-01T00:01:00.000+0000"}},
                {"id":"2","key":"DEMO-2","fields":{"summary":"b","status":{"name":"To Do","statusCategory":{"key":"new"}},"issuetype":{"name":"Story"},"updated":"2026-01-01T00:02:00.000+0000"}},
            ],
            "nextPageToken": "page2",
        });
        let transport =
            ReplayTransport::from_exchanges(vec![exchange(url, 200, vec![], &body.to_string())]);
        let c = client(&transport);
        let mut attempts = 0u32;
        let limits = Limits {
            max_pages: 10,
            max_items: 1,
        };
        let Outcome::Ok(result) = c
            .search(
                &JiraCloud,
                &query("project in (\"DEMO\")", &["summary"], None),
                0,
                &mut attempts,
                limits,
            )
            .await
            .expect("search")
        else {
            panic!("expected Ok");
        };
        assert_eq!(result.items.len(), 1, "stopped right at the item cap");
        assert!(
            !result.stuck_window_exhausted,
            "no cursor: nothing to be stuck on"
        );
        assert_eq!(
            transport.remaining(),
            0,
            "the second page was never requested"
        );
    }

    #[tokio::test]
    async fn the_item_cap_is_bypassed_while_every_item_is_still_within_the_margin_of_the_cursor() {
        // Three issues sit within the 1-hour safety margin of the old cursor, with a cap of 2:
        // without the bypass, this call would stop after 2 items, the cursor would stay within
        // that same window (since the newest item seen is still within it), and the next call
        // would re-issue an identical query and hit the identical cap forever — a livelock. A
        // fourth issue sits two hours later, past the margin, which must still end the walk once
        // reached.
        let url = "https://jira.example.com/rest/api/3/search/jql?jql=project%20in%20%28%22DEMO%22%29%20AND%20updated%20%3E%3D%20%222026-01-01%2000%3A05%22&maxResults=100&fields=summary";
        let body = serde_json::json!({
            "issues": [
                {"id":"1","key":"DEMO-1","fields":{"summary":"a","status":{"name":"To Do","statusCategory":{"key":"new"}},"issuetype":{"name":"Story"},"updated":"2026-01-01T00:05:00.000+0000"}},
                {"id":"2","key":"DEMO-2","fields":{"summary":"b","status":{"name":"To Do","statusCategory":{"key":"new"}},"issuetype":{"name":"Story"},"updated":"2026-01-01T00:05:30.000+0000"}},
                {"id":"3","key":"DEMO-3","fields":{"summary":"c","status":{"name":"To Do","statusCategory":{"key":"new"}},"issuetype":{"name":"Story"},"updated":"2026-01-01T00:05:45.000+0000"}},
                {"id":"4","key":"DEMO-4","fields":{"summary":"d","status":{"name":"To Do","statusCategory":{"key":"new"}},"issuetype":{"name":"Story"},"updated":"2026-01-01T02:00:00.000+0000"}},
            ],
        });
        let transport =
            ReplayTransport::from_exchanges(vec![exchange(url, 200, vec![], &body.to_string())]);
        let c = client(&transport);
        let mut attempts = 0u32;
        let limits = Limits {
            max_pages: 10,
            max_items: 2,
        };
        let cursor = instant("2026-01-01T00:05:00Z");
        let Outcome::Ok(result) = c
            .search(
                &JiraCloud,
                &query(
                    "project in (\"DEMO\") AND updated >= \"2026-01-01 00:05\"",
                    &["summary"],
                    Some(cursor),
                ),
                0,
                &mut attempts,
                limits,
            )
            .await
            .expect("search")
        else {
            panic!("expected Ok");
        };
        // All 4 items came through — the cap (2) was exceeded because the first 3 were all within
        // the cursor's 1-hour margin, and the walk only stopped once DEMO-4 (two hours later, past
        // the margin) was reached.
        assert_eq!(
            result
                .items
                .iter()
                .map(|i| i.key.as_str())
                .collect::<Vec<_>>(),
            vec!["DEMO-1", "DEMO-2", "DEMO-3", "DEMO-4"]
        );
        assert_eq!(result.max_instant, Some(instant("2026-01-01T02:00:00Z")));
        assert!(
            !result.stuck_window_exhausted,
            "the walk reached past the window on its own; the page budget was never the limit"
        );
    }

    #[tokio::test]
    async fn the_page_budget_running_out_while_still_stuck_is_reported_not_silent() {
        // Two pages, two items apiece, every item within the cursor's margin, and only 2 pages of
        // budget: the walk never gets a chance to find an item past the stuck window before the
        // page budget itself runs out. This must be visible to the caller (stuck_window_exhausted),
        // not just a silent truncation.
        let url1 = "https://jira.example.com/rest/api/3/search/jql?jql=project%20in%20%28%22DEMO%22%29%20AND%20updated%20%3E%3D%20%222026-01-01%2000%3A05%22&maxResults=100&fields=summary";
        let url2 = format!("{url1}&nextPageToken=page2");
        let page1 = serde_json::json!({
            "issues": [
                {"id":"1","key":"DEMO-1","fields":{"summary":"a","status":{"name":"To Do","statusCategory":{"key":"new"}},"issuetype":{"name":"Story"},"updated":"2026-01-01T00:05:00.000+0000"}},
                {"id":"2","key":"DEMO-2","fields":{"summary":"b","status":{"name":"To Do","statusCategory":{"key":"new"}},"issuetype":{"name":"Story"},"updated":"2026-01-01T00:05:10.000+0000"}},
            ],
            "nextPageToken": "page2",
        });
        let page2 = serde_json::json!({
            "issues": [
                {"id":"3","key":"DEMO-3","fields":{"summary":"c","status":{"name":"To Do","statusCategory":{"key":"new"}},"issuetype":{"name":"Story"},"updated":"2026-01-01T00:05:20.000+0000"}},
                {"id":"4","key":"DEMO-4","fields":{"summary":"d","status":{"name":"To Do","statusCategory":{"key":"new"}},"issuetype":{"name":"Story"},"updated":"2026-01-01T00:05:30.000+0000"}},
            ],
            "nextPageToken": "page3",
        });
        let transport = ReplayTransport::from_exchanges(vec![
            exchange(url1, 200, vec![], &page1.to_string()),
            exchange(&url2, 200, vec![], &page2.to_string()),
        ]);
        let c = client(&transport);
        let mut attempts = 0u32;
        let limits = Limits {
            max_pages: 2,
            max_items: 2,
        };
        let cursor = instant("2026-01-01T00:05:00Z");
        let Outcome::Ok(result) = c
            .search(
                &JiraCloud,
                &query(
                    "project in (\"DEMO\") AND updated >= \"2026-01-01 00:05\"",
                    &["summary"],
                    Some(cursor),
                ),
                0,
                &mut attempts,
                limits,
            )
            .await
            .expect("search")
        else {
            panic!("expected Ok");
        };
        assert_eq!(result.items.len(), 4, "both pages were fully read");
        assert!(
            result.stuck_window_exhausted,
            "the page budget ran out before any item past the cursor's margin was seen"
        );
        assert_eq!(
            transport.remaining(),
            0,
            "a third page was never requested — the page cap, not more data, stopped the walk"
        );
    }

    #[tokio::test]
    async fn stuck_is_reported_even_when_the_item_cap_never_fires_mid_loop() {
        // Round 3 review item S-3's silent false negative: the old in-loop `still_stuck` was only
        // ever assigned inside the `items.len() >= limits.max_items` branch. With a generous item
        // cap that branch never fires at all — here, 2 pages of 2 items each (4 total), every one
        // within the cursor's margin, `max_items: 100` (never reached), `max_pages: 2` (reached,
        // with a third page still pending). The old code left `still_stuck` at its initial `false`
        // and silently failed to report this as stuck.
        let url1 = "https://jira.example.com/rest/api/3/search/jql?jql=project%20in%20%28%22DEMO%22%29%20AND%20updated%20%3E%3D%20%222026-01-01%2000%3A05%22&maxResults=100&fields=summary";
        let url2 = format!("{url1}&nextPageToken=page2");
        let page1 = serde_json::json!({
            "issues": [
                {"id":"1","key":"DEMO-1","fields":{"summary":"a","status":{"name":"To Do","statusCategory":{"key":"new"}},"issuetype":{"name":"Story"},"updated":"2026-01-01T00:05:00.000+0000"}},
                {"id":"2","key":"DEMO-2","fields":{"summary":"b","status":{"name":"To Do","statusCategory":{"key":"new"}},"issuetype":{"name":"Story"},"updated":"2026-01-01T00:05:10.000+0000"}},
            ],
            "nextPageToken": "page2",
        });
        let page2 = serde_json::json!({
            "issues": [
                {"id":"3","key":"DEMO-3","fields":{"summary":"c","status":{"name":"To Do","statusCategory":{"key":"new"}},"issuetype":{"name":"Story"},"updated":"2026-01-01T00:05:20.000+0000"}},
                {"id":"4","key":"DEMO-4","fields":{"summary":"d","status":{"name":"To Do","statusCategory":{"key":"new"}},"issuetype":{"name":"Story"},"updated":"2026-01-01T00:05:30.000+0000"}},
            ],
            // A third page genuinely exists server-side; the page budget, not the data, stops us.
            "nextPageToken": "page3",
        });
        let transport = ReplayTransport::from_exchanges(vec![
            exchange(url1, 200, vec![], &page1.to_string()),
            exchange(&url2, 200, vec![], &page2.to_string()),
        ]);
        let c = client(&transport);
        let mut attempts = 0u32;
        let limits = Limits {
            max_pages: 2,
            max_items: 100,
        };
        let cursor = instant("2026-01-01T00:05:00Z");
        let Outcome::Ok(result) = c
            .search(
                &JiraCloud,
                &query(
                    "project in (\"DEMO\") AND updated >= \"2026-01-01 00:05\"",
                    &["summary"],
                    Some(cursor),
                ),
                0,
                &mut attempts,
                limits,
            )
            .await
            .expect("search")
        else {
            panic!("expected Ok");
        };
        assert_eq!(result.items.len(), 4);
        assert!(
            result.stuck_window_exhausted,
            "the item cap never fired, but the page budget still ran out while every item \
             stayed within the cursor's margin — this must not be silently dropped"
        );
    }

    #[tokio::test]
    async fn stuck_window_exhausted_is_not_a_false_positive_when_the_last_item_crosses_the_threshold()
     {
        // The related nit: the old in-loop `still_stuck` could hold a *stale* `true`, set just
        // before the one item that actually escaped the margin was processed, if that item turned
        // out to be the very last one this call ever read (no later item re-ran the check to
        // notice the crossing). Page 1 (3 items, filling the item cap exactly, with no item left
        // over in that page to trigger the cap check) all sit within the margin; page 2 is a
        // single item two hours later — past the margin, i.e. a genuine crossing — and itself
        // claims a further page exists, so the page budget (not the data) ends the walk right
        // after processing it.
        let url1 = "https://jira.example.com/rest/api/3/search/jql?jql=project%20in%20%28%22DEMO%22%29%20AND%20updated%20%3E%3D%20%222026-01-01%2000%3A05%22&maxResults=100&fields=summary";
        let url2 = format!("{url1}&nextPageToken=page2");
        let page1 = serde_json::json!({
            "issues": [
                {"id":"1","key":"DEMO-1","fields":{"summary":"a","status":{"name":"To Do","statusCategory":{"key":"new"}},"issuetype":{"name":"Story"},"updated":"2026-01-01T00:05:00.000+0000"}},
                {"id":"2","key":"DEMO-2","fields":{"summary":"b","status":{"name":"To Do","statusCategory":{"key":"new"}},"issuetype":{"name":"Story"},"updated":"2026-01-01T00:05:10.000+0000"}},
                {"id":"3","key":"DEMO-3","fields":{"summary":"c","status":{"name":"To Do","statusCategory":{"key":"new"}},"issuetype":{"name":"Story"},"updated":"2026-01-01T00:05:20.000+0000"}},
            ],
            "nextPageToken": "page2",
        });
        let page2 = serde_json::json!({
            "issues": [
                {"id":"4","key":"DEMO-4","fields":{"summary":"d","status":{"name":"To Do","statusCategory":{"key":"new"}},"issuetype":{"name":"Story"},"updated":"2026-01-01T02:00:00.000+0000"}},
            ],
            // Claims a third page too, so the page budget (not natural completion) ends the walk.
            "nextPageToken": "page3",
        });
        let transport = ReplayTransport::from_exchanges(vec![
            exchange(url1, 200, vec![], &page1.to_string()),
            exchange(&url2, 200, vec![], &page2.to_string()),
        ]);
        let c = client(&transport);
        let mut attempts = 0u32;
        let limits = Limits {
            max_pages: 2,
            max_items: 3,
        };
        let cursor = instant("2026-01-01T00:05:00Z");
        let Outcome::Ok(result) = c
            .search(
                &JiraCloud,
                &query(
                    "project in (\"DEMO\") AND updated >= \"2026-01-01 00:05\"",
                    &["summary"],
                    Some(cursor),
                ),
                0,
                &mut attempts,
                limits,
            )
            .await
            .expect("search")
        else {
            panic!("expected Ok");
        };
        assert_eq!(
            result.items.len(),
            4,
            "the crossing item (DEMO-4) was still read"
        );
        assert_eq!(result.max_instant, Some(instant("2026-01-01T02:00:00Z")));
        assert!(
            !result.stuck_window_exhausted,
            "DEMO-4 crossed the margin — real progress was made, even though it was also the \
             last item this call read before the page budget ran out"
        );
    }

    #[tokio::test]
    async fn the_item_cap_still_applies_with_no_old_cursor() {
        // On a first sync (no cursor yet) the bypass must not apply: there is nothing to be
        // "stuck on", so the ordinary cap behaviour (stop, resume next call from the top) is
        // both correct and expected.
        let url = "https://jira.example.com/rest/api/3/search/jql?jql=project%20in%20%28%22DEMO%22%29&maxResults=100&fields=summary";
        let body = serde_json::json!({
            "issues": [
                {"id":"1","key":"DEMO-1","fields":{"summary":"a","status":{"name":"To Do","statusCategory":{"key":"new"}},"issuetype":{"name":"Story"},"updated":"2026-01-01T00:05:00.000+0000"}},
                {"id":"2","key":"DEMO-2","fields":{"summary":"b","status":{"name":"To Do","statusCategory":{"key":"new"}},"issuetype":{"name":"Story"},"updated":"2026-01-01T00:05:30.000+0000"}},
                {"id":"3","key":"DEMO-3","fields":{"summary":"c","status":{"name":"To Do","statusCategory":{"key":"new"}},"issuetype":{"name":"Story"},"updated":"2026-01-01T00:05:45.000+0000"}},
            ],
        });
        let transport =
            ReplayTransport::from_exchanges(vec![exchange(url, 200, vec![], &body.to_string())]);
        let c = client(&transport);
        let mut attempts = 0u32;
        let limits = Limits {
            max_pages: 10,
            max_items: 2,
        };
        let Outcome::Ok(result) = c
            .search(
                &JiraCloud,
                &query("project in (\"DEMO\")", &["summary"], None),
                0,
                &mut attempts,
                limits,
            )
            .await
            .expect("search")
        else {
            panic!("expected Ok");
        };
        assert_eq!(result.items.len(), 2, "the ordinary cap still applies");
        assert!(!result.stuck_window_exhausted);
    }
}
