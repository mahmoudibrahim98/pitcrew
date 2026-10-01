//! The client core: search pagination over a [`Transport`], shared by both deployments through
//! [`Deployment`], plus `/myself` (identical on both, so it needs no deployment-specific
//! handling at all).
//!
//! Unlike `pitcrew_sync_github`'s client, there are no conditional requests here: Jira's search
//! has no `ETag`/`304` equivalent this crate uses, so incremental reads rely only on the JQL
//! `updated >=` cursor (see [`crate::jql`]).

use crate::auth::JiraAuth;
use crate::bounds::{MAX_ITEMS_PER_SYNC, MAX_PAGE_BODY_BYTES, MAX_PAGES_PER_CALL, backoff_secs};
use crate::deployment::Deployment;
use crate::time::{JiraTimestamp, account_minute};
use crate::wire::WireIssue;
use jiff::tz::TimeZone;
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

/// What to search and how: grouped into one struct (rather than four separate `search` parameters)
/// to stay under clippy's argument-count lint, and because the four genuinely travel together —
/// `fields` and `old_cursor_minute` only make sense alongside the `jql` they were built from, and
/// `zone` only matters for converting items read *from* that same search.
#[derive(Clone, Copy)]
pub(crate) struct SearchQuery<'a> {
    pub jql: &'a str,
    pub fields: &'a [&'a str],
    /// The searching account's own time zone — see [`crate::time::account_minute`].
    pub zone: &'a TimeZone,
    /// The cursor `jql` was built from (`None` on a first sync). See `search`'s doc for why this
    /// is needed even though it is also baked into `jql` itself.
    pub old_cursor_minute: Option<&'a str>,
}

/// One `search` call's outcome: successfully parsed issues, plus bookkeeping for the caller's
/// cursor.
#[derive(Debug)]
pub(crate) struct SearchResult {
    pub items: Vec<WireIssue>,
    pub malformed_skipped: u32,
    /// The latest `updated`, converted to the account's zone and floored to the minute (see
    /// [`crate::time::account_minute`]), seen in this call across every page fetched. Already in
    /// the exact text [`crate::jql::incremental_query`] needs for the next call's cursor.
    pub max_minute: Option<String>,
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
    /// `query.zone` is the searching account's own time zone, used to convert each item's
    /// `updated` into the account-local minute the caller's cursor is built from (see
    /// [`crate::time::account_minute`]). `query.old_cursor_minute` is the cursor `query.jql` was
    /// already built from (`None` on a first sync): when the item cap is reached but every item
    /// collected so far still shares that exact minute, the cap is **not** enforced — pagination
    /// keeps going (still bounded by `limits.max_pages`) until an item past that minute is seen,
    /// or the data runs out. Without this, a project with more updates in one minute than
    /// `limits.max_items` would see its cursor get stuck on that minute forever: every call would
    /// re-fetch the identical capped batch and never make progress (a livelock). Re-fetching
    /// already-seen items the rest of the time is harmless — diffing against the stored snapshot
    /// (see `crate::sync`) makes that a no-op.
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
            zone,
            old_cursor_minute,
        } = *query;
        let mut page = deployment.first_page();
        let mut items = Vec::new();
        let mut malformed = 0u32;
        let mut max_minute: Option<String> = None;
        // Dedupes an issue appearing twice within this one call — e.g. offset-based pagination
        // (Data Center) can repeat or skip a row when an item is updated concurrently with the
        // walk shifting it across a page boundary. Across *calls*, the same overlap is handled by
        // diffing against the stored snapshot (see `crate::sync`), which is naturally a no-op when
        // nothing actually changed; this only guards one call's own page walk.
        let mut seen_this_call: HashSet<(String, String)> = HashSet::new();

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
                break;
            }
            let Ok((raw_items, next)) = deployment.parse_search_page(&response.body) else {
                tracing::warn!(url = %url, "could not parse a search response page, stopping this call");
                malformed += 1;
                break;
            };

            let mut cap_hit = false;
            for value in raw_items {
                if items.len() >= limits.max_items {
                    // See this method's doc: only actually stop if the walk has already moved
                    // past the old cursor's minute. `max_minute` reflects the most recently
                    // processed item (items arrive in ascending `updated` order), so this is
                    // exactly "has every item so far stayed within the cursor minute".
                    let still_at_cursor_minute = match (max_minute.as_deref(), old_cursor_minute) {
                        (Some(seen), Some(cursor)) => seen == cursor,
                        _ => false,
                    };
                    if !still_at_cursor_minute {
                        cap_hit = true;
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
                        if let Some(instant) = ts.to_instant() {
                            let minute = account_minute(instant, zone);
                            if max_minute.as_deref().is_none_or(|m| minute.as_str() > m) {
                                max_minute = Some(minute);
                            }
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
                None => break,
            }
        }

        Ok(Outcome::Ok(SearchResult {
            items,
            malformed_skipped: malformed,
            max_minute,
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
        zone: &'a TimeZone,
        old_cursor_minute: Option<&'a str>,
    ) -> SearchQuery<'a> {
        SearchQuery {
            jql,
            fields,
            zone,
            old_cursor_minute,
        }
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
        let zone = TimeZone::UTC;
        let mut attempts = 0u32;
        let Outcome::RateLimited(rl) = c
            .search(
                &JiraCloud,
                &query("project in (\"DEMO\")", &["summary"], &zone, None),
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
            let zone = TimeZone::UTC;
            let Outcome::RateLimited(rl) = c
                .search(
                    &JiraCloud,
                    &query("project in (\"DEMO\")", &["summary"], &zone, None),
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
        let zone = TimeZone::UTC;
        let mut attempts = 0u32;
        let err = c
            .search(
                &JiraCloud,
                &query("project in (\"DEMO\")", &["summary"], &zone, None),
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
        let zone = TimeZone::UTC;
        let mut attempts = 0u32;
        let limits = Limits {
            max_pages: 10,
            max_items: 1,
        };
        let Outcome::Ok(result) = c
            .search(
                &JiraCloud,
                &query("project in (\"DEMO\")", &["summary"], &zone, None),
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
        assert_eq!(
            transport.remaining(),
            0,
            "the second page was never requested"
        );
    }

    #[tokio::test]
    async fn the_item_cap_is_bypassed_while_every_item_is_still_at_the_old_cursor_minute() {
        // Three issues share the exact same minute as the old cursor, with a cap of 2: without
        // the bypass, this call would stop after 2 items, the cursor would stay on that same
        // minute (since the newest item seen is still that minute), and the next call would
        // re-issue an identical query and hit the identical cap forever — a livelock. A fourth
        // issue sits one minute later, which must still end the walk once reached.
        let url = "https://jira.example.com/rest/api/3/search/jql?jql=project%20in%20%28%22DEMO%22%29%20AND%20updated%20%3E%3D%20%222026-01-01%2000%3A05%22&maxResults=100&fields=summary";
        let body = serde_json::json!({
            "issues": [
                {"id":"1","key":"DEMO-1","fields":{"summary":"a","status":{"name":"To Do","statusCategory":{"key":"new"}},"issuetype":{"name":"Story"},"updated":"2026-01-01T00:05:00.000+0000"}},
                {"id":"2","key":"DEMO-2","fields":{"summary":"b","status":{"name":"To Do","statusCategory":{"key":"new"}},"issuetype":{"name":"Story"},"updated":"2026-01-01T00:05:30.000+0000"}},
                {"id":"3","key":"DEMO-3","fields":{"summary":"c","status":{"name":"To Do","statusCategory":{"key":"new"}},"issuetype":{"name":"Story"},"updated":"2026-01-01T00:05:45.000+0000"}},
                {"id":"4","key":"DEMO-4","fields":{"summary":"d","status":{"name":"To Do","statusCategory":{"key":"new"}},"issuetype":{"name":"Story"},"updated":"2026-01-01T00:06:00.000+0000"}},
            ],
        });
        let transport =
            ReplayTransport::from_exchanges(vec![exchange(url, 200, vec![], &body.to_string())]);
        let c = client(&transport);
        let zone = TimeZone::UTC;
        let mut attempts = 0u32;
        let limits = Limits {
            max_pages: 10,
            max_items: 2,
        };
        let Outcome::Ok(result) = c
            .search(
                &JiraCloud,
                &query(
                    "project in (\"DEMO\") AND updated >= \"2026-01-01 00:05\"",
                    &["summary"],
                    &zone,
                    Some("2026-01-01 00:05"),
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
        // All 4 items came through — the cap (2) was exceeded because the first 3 all shared the
        // cursor minute, and the walk only stopped once DEMO-4 (a later minute) was reached.
        assert_eq!(
            result
                .items
                .iter()
                .map(|i| i.key.as_str())
                .collect::<Vec<_>>(),
            vec!["DEMO-1", "DEMO-2", "DEMO-3", "DEMO-4"]
        );
        assert_eq!(result.max_minute.as_deref(), Some("2026-01-01 00:06"));
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
        let zone = TimeZone::UTC;
        let mut attempts = 0u32;
        let limits = Limits {
            max_pages: 10,
            max_items: 2,
        };
        let Outcome::Ok(result) = c
            .search(
                &JiraCloud,
                &query("project in (\"DEMO\")", &["summary"], &zone, None),
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
    }
}
