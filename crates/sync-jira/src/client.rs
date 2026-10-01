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
use crate::time::JiraTimestamp;
use crate::wire::WireIssue;
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

/// One `search` call's outcome: successfully parsed issues, plus bookkeeping for the caller's
/// cursor.
#[derive(Debug)]
pub(crate) struct SearchResult {
    pub items: Vec<WireIssue>,
    pub malformed_skipped: u32,
    /// The latest well-formed `fields.updated` seen in this call, across every page fetched.
    pub max_updated: Option<JiraTimestamp>,
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
        let myself: crate::wire::WireMyself =
            serde_json::from_slice(&response.body).unwrap_or(crate::wire::WireMyself { time_zone: None });
        Ok(myself.time_zone)
    }

    /// Runs `jql`, following pagination through `deployment` up to `limits.max_pages`/
    /// `limits.max_items`, honouring a 429 rate limit.
    pub(crate) async fn search<D: Deployment>(
        &self,
        deployment: &D,
        jql: &str,
        fields: &[&str],
        now_unix: i64,
        attempts: &mut u32,
        limits: Limits,
    ) -> Result<Outcome<SearchResult>, ClientError> {
        let mut page = deployment.first_page();
        let mut items = Vec::new();
        let mut malformed = 0u32;
        let mut max_updated: Option<JiraTimestamp> = None;
        // Dedupes an issue appearing twice within this one call — e.g. offset-based pagination
        // (Data Center) can repeat or skip a row when an item is updated concurrently with the
        // walk shifting it across a page boundary. Across *calls*, the same overlap is handled by
        // diffing against the stored snapshot (see `crate::sync`), which is naturally a no-op when
        // nothing actually changed; this only guards one call's own page walk.
        let mut seen_this_call: HashSet<(String, String)> = HashSet::new();

        for _ in 0..limits.max_pages {
            let request = deployment.build_search_request(&self.api_base, &self.auth, jql, fields, &page);
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
                    cap_hit = true;
                    break;
                }
                match serde_json::from_value::<WireIssue>(value) {
                    Ok(issue) => {
                        // Dedupe by issue id plus `updated`, as the brief asks: offset-based
                        // pagination (Data Center) can repeat a row when an item is updated
                        // concurrently with the walk shifting it across a page boundary.
                        if !seen_this_call.insert((issue.id.clone(), issue.fields.updated.clone())) {
                            continue;
                        }
                        let ts = JiraTimestamp::new(&issue.fields.updated);
                        if ts.is_well_formed() && max_updated.as_ref().is_none_or(|m| &ts > m) {
                            max_updated = Some(ts);
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
            max_updated,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deployment::JiraCloud;
    use pitcrew_sync_github::fixture::{RecordedExchange, ReplayTransport};

    fn exchange(url: &str, status: u16, headers: Vec<(&str, &str)>, body: &str) -> RecordedExchange {
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
                "project in (\"DEMO\")",
                &["summary"],
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
                    "project in (\"DEMO\")",
                    &["summary"],
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
                "project in (\"DEMO\")",
                &["summary"],
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
        let transport = ReplayTransport::from_exchanges(vec![exchange(
            url,
            200,
            vec![],
            &body.to_string(),
        )]);
        let c = client(&transport);
        let mut attempts = 0u32;
        let limits = Limits {
            max_pages: 10,
            max_items: 1,
        };
        let Outcome::Ok(result) = c
            .search(
                &JiraCloud,
                "project in (\"DEMO\")",
                &["summary"],
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
        assert_eq!(transport.remaining(), 0, "the second page was never requested");
    }
}
