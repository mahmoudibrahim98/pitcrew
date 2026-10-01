//! The client core: REST v3 over a [`Transport`], with pagination, conditional requests and
//! rate-limit handling.

use crate::bounds::{Limits, MAX_PAGE_BODY_BYTES, SECONDARY_BACKOFF_CAP_SECS, backoff_secs};
use crate::link_header::next_link;
use crate::origin::trusted_next_url;
use crate::state::ListCache;
use crate::time::GithubTimestamp;
use crate::transport::{AuthToken, Method, Request, Response, Transport, TransportError};
use serde::de::DeserializeOwned;

/// The GitHub REST API root. Overridable (`with_api_base`) for GitHub Enterprise Server.
pub const DEFAULT_API_BASE: &str = "https://api.github.com";

/// The pinned API version sent on every request. Bumping this is a deliberate, tested decision,
/// not a drive-by edit.
pub const API_VERSION: &str = "2022-11-28";

/// Errors the client core reports. None of these carry the token.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// The transport itself failed.
    #[error(transparent)]
    Transport(#[from] TransportError),
    /// An HTTP status this client does not know how to handle (rate limits and 304 are handled
    /// separately).
    #[error("unexpected status {status} from {url}")]
    Status {
        /// The URL.
        url: String,
        /// The status code.
        status: u16,
    },
}

/// The result of a request that might have been rate-limited instead of answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome<T> {
    /// The call went through.
    Ok(T),
    /// The token is rate-limited; try again no sooner than `until` (Unix seconds). Never sleep on
    /// this: return it to the caller.
    RateLimited {
        /// Unix seconds.
        until: i64,
        /// Whether this was a secondary (abuse-detection) limit rather than the primary hourly
        /// quota.
        secondary: bool,
    },
}

/// One page listing's outcome: successfully parsed items, plus the caches to store for next time.
#[derive(Debug)]
pub(crate) struct ListResult<Item> {
    pub items: Vec<Item>,
    pub max_updated_at: Option<GithubTimestamp>,
    pub malformed_skipped: u32,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub not_modified: bool,
    /// Whether this walk reached a genuine stopping point: for a `stop_at_cursor` listing (pull
    /// requests), the old cursor, or running out of pages; for any listing, simply running out of
    /// pages. `false` means a page/item cap, a malformed page, or an untrusted `Link` cut the walk
    /// short. Listings that don't use `stop_at_cursor` (issues, milestones) can ignore this: their
    /// server-side `since` filter (or, for milestones, the lack of any cursor at all) makes a
    /// truncated walk self-healing on the next call, so only the `stop_at_cursor` caller
    /// ([`crate::sync`]'s pull request sync) needs to gate its cursor advance on it.
    pub completed: bool,
    /// Set only when a page or item cap (not a malformed page, not an untrusted link) truncated
    /// the walk: where to resume pagination next call, instead of starting over from page 1.
    pub resume_from: Option<String>,
    /// The oldest well-formed `updated_at` processed in this call, for a resume cursor's
    /// diagnostics.
    pub oldest_seen: Option<GithubTimestamp>,
    /// Set when a `Link: rel="next"` outside the configured API base was ignored. The caller
    /// raises this as a [`crate::sync::SyncIssue`]: a server (or a proxy in front of it) handing
    /// back a pagination link to an unexpected host is worth a person's attention, not just a
    /// debug log, even though the items already collected are still returned normally. This is
    /// the raw, untrusted link text; the caller sanitises and caps it before putting it in a
    /// message (see `crate::sync::blocked_link_message`).
    pub blocked_link: Option<String>,
}

struct RawResponse {
    status: u16,
    body: Vec<u8>,
    etag: Option<String>,
    last_modified: Option<String>,
    link: Option<String>,
    ratelimit_remaining: Option<i64>,
    ratelimit_reset: Option<i64>,
    retry_after: Option<i64>,
}

fn parse_int_header(response: &Response, name: &str) -> Option<i64> {
    response.header(name).and_then(|v| v.trim().parse().ok())
}

impl RawResponse {
    fn capture(response: Response) -> Self {
        Self {
            status: response.status,
            etag: response.header("etag").map(str::to_string),
            last_modified: response.header("last-modified").map(str::to_string),
            link: response.header("link").map(str::to_string),
            ratelimit_remaining: parse_int_header(&response, "x-ratelimit-remaining"),
            ratelimit_reset: parse_int_header(&response, "x-ratelimit-reset"),
            retry_after: parse_int_header(&response, "retry-after"),
            body: response.body,
        }
    }
}

/// A GitHub REST client over any [`Transport`]. Generic over `T` (rather than `dyn Transport`) so
/// `Transport::send` can return `impl Future` without an `async-trait` dependency.
pub struct GithubClient<'t, T: Transport> {
    transport: &'t T,
    token: AuthToken,
    api_base: String,
}

impl<T: Transport> std::fmt::Debug for GithubClient<'_, T> {
    // Manual: `transport` is generic and not required to be `Debug`; `token` already redacts
    // itself.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GithubClient")
            .field("api_base", &self.api_base)
            .field("token", &self.token)
            .finish_non_exhaustive()
    }
}

impl<'t, T: Transport> GithubClient<'t, T> {
    /// Builds a client over `transport`, authenticating with `token`.
    #[must_use]
    pub fn new(transport: &'t T, token: AuthToken) -> Self {
        Self {
            transport,
            token,
            api_base: DEFAULT_API_BASE.to_string(),
        }
    }

    /// Overrides the API root (GitHub Enterprise Server).
    #[must_use]
    pub fn with_api_base(mut self, base: impl Into<String>) -> Self {
        self.api_base = base.into();
        self
    }

    /// The API root this client sends requests to.
    #[must_use]
    pub fn api_base(&self) -> &str {
        &self.api_base
    }

    async fn get(
        &self,
        url: &str,
        etag: Option<&str>,
        last_modified: Option<&str>,
    ) -> Result<RawResponse, ClientError> {
        let mut headers = vec![
            (
                "Accept".to_string(),
                "application/vnd.github+json".to_string(),
            ),
            ("X-GitHub-Api-Version".to_string(), API_VERSION.to_string()),
            ("Authorization".to_string(), self.token.header_value()),
        ];
        if let Some(e) = etag {
            headers.push(("If-None-Match".to_string(), e.to_string()));
        }
        if let Some(lm) = last_modified {
            headers.push(("If-Modified-Since".to_string(), lm.to_string()));
        }
        let request = Request {
            method: Method::Get,
            url: url.to_string(),
            headers,
            body: Vec::new(),
        };
        let response = self.transport.send(request).await?;
        Ok(RawResponse::capture(response))
    }

    /// Lists one resource, following `Link: rel="next"` up to `limits.max_pages` and at most
    /// `limits.max_items` items, honouring conditional requests and rate limits.
    ///
    /// When `stop_at_cursor` is true (pull requests, whose list endpoint has no `since`
    /// parameter), an item whose `updated_at` is not after `cache.since` ends the whole listing:
    /// everything from here on (the rest of this page and all later pages) is older still,
    /// because the list is sorted by `updated` descending. Reaching that point, or running out of
    /// pages, is a *natural* end (`ListResult::completed = true`); a page/item cap or an untrusted
    /// `Link` cutting the walk short is not (`completed = false`), because the cursor this walk
    /// would otherwise advance to (the newest item seen) does not mean "everything newer than this
    /// has been read" when the walk never reached its old boundary — see `ListResult::completed`.
    pub(crate) async fn list<Item: DeserializeOwned>(
        &self,
        first_url: String,
        cache: &ListCache,
        stop_at_cursor: bool,
        now_unix: i64,
        attempts: &mut u32,
        limits: Limits,
    ) -> Result<Outcome<ListResult<Item>>, ClientError> {
        let mut url = first_url;
        let mut items = Vec::new();
        let mut malformed = 0u32;
        let mut max_updated: Option<GithubTimestamp> = None;
        let mut min_updated: Option<GithubTimestamp> = None;
        let mut etag = cache.etag.clone();
        let mut last_modified = cache.last_modified.clone();
        // `true` once the walk reaches a genuine stopping point (see the doc comment above).
        let mut completed = false;
        // Set only when a page/item cap truncates the walk, to where the next call should resume.
        let mut resume_from: Option<String> = None;
        // Set when an untrusted `Link: rel="next"` was ignored, for the caller to raise as a
        // `SyncIssue`.
        let mut blocked_link: Option<String> = None;
        // Set on a truncation that must *not* get an automatic resume pointer from the post-loop
        // check below (a page-level failure, or an untrusted link): both would just hit the same
        // outcome again were the client to retry them directly, so the next call restarts from the
        // top instead. Kept separate from `malformed` (below), which also counts individual
        // malformed *items* within an otherwise normally-completing page — those must not suppress
        // a legitimate resume pointer from a later page-count truncation in the same call.
        let mut blocked_from_resuming = false;

        for _page in 0..limits.max_pages {
            let raw = self
                .get(&url, etag.as_deref(), last_modified.as_deref())
                .await?;

            // Checked before looking at status: these headers are rejection signals only when
            // paired with a non-2xx status. A 200 reporting zero remaining still carries data this
            // request earned; only the *next* request need wait, and that one will hit this branch
            // itself (GitHub answers it with 403 once the quota is spent).
            if raw.status == 403 || raw.status == 429 {
                if raw.ratelimit_remaining == Some(0)
                    && let Some(reset) = raw.ratelimit_reset
                {
                    return Ok(Outcome::RateLimited {
                        until: reset,
                        secondary: false,
                    });
                }
                // A 403/429 is a rate limit only when something actually says so: a `retry-after`
                // duration, or a body naming a secondary (abuse-detection) limit. Anything else —
                // a revoked token, a missing scope, a plain permissions error — must surface as a
                // `ClientError` (a `SyncIssue` to the caller), not be retried forever as if it
                // would ever clear on its own.
                if raw.retry_after.is_none() && !body_mentions_rate_limit(&raw.body) {
                    return Err(ClientError::Status {
                        url,
                        status: raw.status,
                    });
                }
                let until = if let Some(retry_after) = raw.retry_after {
                    // R28: a server's `retry-after` is an untrusted `i64` — a hostile value near
                    // `i64::MAX` must not overflow `now_unix + retry_after` (a panic with overflow
                    // checks, a deadline wrapped into the past in release), and an enormous but
                    // in-range value must not be trusted outright either. Clamp to the same
                    // range this crate ever itself backs off for, then add with saturation.
                    now_unix.saturating_add(retry_after.clamp(0, SECONDARY_BACKOFF_CAP_SECS))
                } else {
                    *attempts = attempts.saturating_add(1);
                    now_unix + backoff_secs(*attempts)
                };
                return Ok(Outcome::RateLimited {
                    until,
                    secondary: true,
                });
            }
            if raw.status == 304 {
                return Ok(Outcome::Ok(ListResult {
                    items,
                    max_updated_at: None,
                    malformed_skipped: 0,
                    etag: cache.etag.clone(),
                    last_modified: cache.last_modified.clone(),
                    not_modified: true,
                    completed: true,
                    resume_from: None,
                    oldest_seen: None,
                    blocked_link: None,
                }));
            }
            if raw.status != 200 {
                return Err(ClientError::Status {
                    url,
                    status: raw.status,
                });
            }
            *attempts = 0;
            etag = raw.etag.clone().or(etag);
            last_modified = raw.last_modified.clone().or(last_modified);

            // An oversized or unparsable page stops pagination for this call (keeping whatever was
            // already collected) rather than parsing a huge or garbled document. This is
            // deliberate, not a truncation the caller should treat as "caught up": `completed`
            // stays `false`, so a `stop_at_cursor` caller won't advance its cursor past items it
            // never actually saw. There is no resume pointer for it (unlike a cap): if the page is
            // genuinely malformed, resuming from it would just hit the same failure again, so the
            // next call starts over from the top instead.
            if raw.body.len() > MAX_PAGE_BODY_BYTES {
                malformed += 1;
                blocked_from_resuming = true;
                break;
            }
            let Ok(values) = serde_json::from_slice::<Vec<serde_json::Value>>(&raw.body) else {
                malformed += 1;
                blocked_from_resuming = true;
                break;
            };

            let mut stop = false;
            let mut cap_hit = false;
            for value in values {
                if items.len() >= limits.max_items {
                    stop = true;
                    cap_hit = true;
                    break;
                }
                let updated = value
                    .get("updated_at")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                if stop_at_cursor
                    && let (Some(u), Some(since)) = (&updated, &cache.since)
                    && u.as_str() <= since.as_str()
                {
                    stop = true;
                    completed = true;
                    break;
                }
                match serde_json::from_value::<Item>(value) {
                    Ok(item) => {
                        // A malformed `updated_at` must never drive the cursor: an item is only
                        // folded into `max_updated`/`min_updated` once its timestamp is confirmed
                        // well-formed (the item itself is still kept — [`crate::change`]'s diffing
                        // separately drops it and counts it as malformed, since that is also where
                        // its snapshot would otherwise be stored).
                        if let Some(u) = updated {
                            let ts = GithubTimestamp::new(u);
                            if ts.is_well_formed() {
                                if max_updated.as_ref().is_none_or(|m| &ts > m) {
                                    max_updated = Some(ts.clone());
                                }
                                if min_updated.as_ref().is_none_or(|m| &ts < m) {
                                    min_updated = Some(ts);
                                }
                            }
                        }
                        items.push(item);
                    }
                    Err(_) => {
                        malformed += 1;
                        tracing::debug!(url = %url, "skipped a malformed item");
                    }
                }
            }
            if stop {
                if cap_hit {
                    // The cap landed before this page was fully read: the items after the cut
                    // were never even looked at, so there is no reliable resume point. Resuming
                    // from this same page would just hit the identical cut again (no progress);
                    // resuming from the next page would skip this page's unread tail (data loss).
                    // The next call restarts from the top instead. In production this cannot
                    // actually happen — `MAX_ITEMS_PER_SYNC` is an exact multiple of GitHub's own
                    // `per_page` — so this only guards against a single page claiming far more
                    // items than GitHub would ever genuinely send.
                    blocked_from_resuming = true;
                }
                break;
            }
            match raw.link.as_deref().and_then(next_link) {
                // Compared against `self.api_base` (round 3 review item B-1: GitHub rewrites the
                // path in several endpoints' first `next` link — see `origin::trusted_next_url`'s
                // doc) — `next` must share its scheme, host and effective port, with a path under
                // the API base's own. On a match, the *parsed* URL is what gets requested next
                // (round 3 item S-2), not the original `next` text, so the request actually sent
                // can never diverge from what this check approved.
                Some(next) => match trusted_next_url(&next, &self.api_base) {
                    Some(parsed) => url = parsed.as_str().to_string(),
                    None => {
                        // Never follow an untrusted `Link: rel="next"`: it would send the
                        // `Authorization` header (attached in `get`, above) to whatever host
                        // answered, or walk the request to an unexpected path. This is left
                        // `completed = false` with no resume pointer: resuming from the current
                        // (already fully processed) page would just hit the same untrusted link
                        // again, so the next call restarts from the top instead. It is also
                        // reported back as `blocked_link`, for the caller to raise as a visible
                        // `SyncIssue` rather than just a log line.
                        tracing::warn!(
                            url = %next,
                            api_base = %self.api_base,
                            "ignored a Link: rel=\"next\" outside the configured API base"
                        );
                        blocked_link = Some(next);
                        blocked_from_resuming = true;
                        break;
                    }
                },
                None => {
                    completed = true;
                    break;
                }
            }
            if items.len() >= limits.max_items {
                // This page was processed in full (the per-item check above never fired), and it
                // happened to fill the item budget exactly at its last item. `url` already holds
                // the next page (just set above) if the server sent one; either way, fetching
                // further is this call's choice to make, not a requirement — stop here and treat
                // it exactly like running out of `limits.max_pages` (see the comment below the
                // loop): a clean, unread page boundary to resume from, never a cut mid-page.
                break;
            }
        }
        // If the loop above ran out of `limits.max_pages` iterations without an explicit `break`,
        // the last iteration's trailing `url = next` assignment (the trusted-link arm, above) left
        // `url` holding the next page to fetch, and that whole page was fully processed (a
        // mid-page item cap would already have set `blocked_from_resuming` and broken out above).
        // That is a page-count truncation exactly like the end-of-page item cap just above: treat
        // it the same way.
        if !completed && resume_from.is_none() && !blocked_from_resuming {
            resume_from = Some(url);
        }

        Ok(Outcome::Ok(ListResult {
            items,
            max_updated_at: max_updated,
            malformed_skipped: malformed,
            etag,
            last_modified,
            not_modified: false,
            completed,
            resume_from,
            oldest_seen: min_updated,
            blocked_link,
        }))
    }
}

/// GitHub's abuse/secondary-rate-limit responses carry a `message` naming it explicitly (e.g. "You
/// have exceeded a secondary rate limit..."). This looks only at that JSON field, never the raw
/// body: scanning the whole body would also match the string appearing incidentally elsewhere
/// (a `documentation_url`, an issue title echoed back in a validation error, …), wrongly treating
/// an unrelated error as a rate limit that will clear on its own.
fn body_mentions_rate_limit(body: &[u8]) -> bool {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(body) else {
        return false;
    };
    let Some(message) = value.get("message").and_then(serde_json::Value::as_str) else {
        return false;
    };
    let lower = message.to_ascii_lowercase();
    lower.contains("rate limit") || lower.contains("abuse")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::{RecordedExchange, ReplayTransport};

    #[test]
    fn client_error_display_never_carries_the_token() {
        let err = ClientError::Status {
            url: "https://api.github.com/repos/example-org/demo-repo".into(),
            status: 500,
        };
        let shown = format!("{err}");
        assert!(!shown.to_lowercase().contains("bearer"));
        assert!(!shown.to_lowercase().contains("token"));
    }

    #[derive(serde::Deserialize, Debug)]
    struct TestItem {
        #[allow(dead_code)]
        number: u64,
        #[allow(dead_code)]
        updated_at: String,
    }

    const URL: &str = "https://api.github.com/repos/example-org/demo-repo/pulls?state=all&sort=updated&direction=desc&per_page=100";

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

    fn client_for(transport: &ReplayTransport) -> GithubClient<'_, ReplayTransport> {
        GithubClient::new(transport, AuthToken::new("ghp_test_token_not_real"))
    }

    #[tokio::test]
    async fn item_cap_mid_page_truncates_with_no_resume_pointer() {
        // In production this cannot happen (`MAX_ITEMS_PER_SYNC` is an exact multiple of
        // GitHub's own `per_page`); this exercises it anyway as a defensive backstop against a
        // single page claiming far more items than GitHub would genuinely send. There is no safe
        // resume point mid-page (see `ListCache::resume`'s doc), so the next call must restart
        // from the top rather than risk either re-processing forever or skipping this page's
        // unread tail.
        let body = "[\
            {\"number\":1,\"updated_at\":\"2026-01-01T00:01:00Z\"},\
            {\"number\":2,\"updated_at\":\"2026-01-01T00:02:00Z\"},\
            {\"number\":3,\"updated_at\":\"2026-01-01T00:03:00Z\"},\
            {\"number\":4,\"updated_at\":\"2026-01-01T00:04:00Z\"},\
            {\"number\":5,\"updated_at\":\"2026-01-01T00:05:00Z\"}]";
        let transport = ReplayTransport::from_exchanges(vec![exchange(URL, 200, vec![], body)]);
        let client = client_for(&transport);
        let cache = ListCache::default();
        let mut attempts = 0u32;
        let limits = Limits {
            max_pages: 10,
            max_items: 3,
        };
        let Outcome::Ok(result) = client
            .list::<TestItem>(URL.to_string(), &cache, true, 0, &mut attempts, limits)
            .await
            .expect("list")
        else {
            panic!("expected Ok");
        };
        assert_eq!(result.items.len(), 3, "stopped right at the item cap");
        assert!(
            !result.completed,
            "the walk never reached the old cursor or ran out of pages"
        );
        assert!(
            result.resume_from.is_none(),
            "items 4 and 5 on this same page were never looked at: no safe resume point exists"
        );
        assert_eq!(
            result.oldest_seen.as_ref().map(GithubTimestamp::as_str),
            Some("2026-01-01T00:01:00Z")
        );
    }

    #[tokio::test]
    async fn item_cap_reached_exactly_at_a_page_boundary_resumes_from_the_next_page() {
        // The realistic (production) shape: the cap lands exactly when a page finishes, not
        // mid-page, so this call got everything on this page and can cleanly resume at the next.
        let next_url = format!("{URL}&page=2");
        let page1 = exchange(
            URL,
            200,
            vec![("Link", &format!("<{next_url}>; rel=\"next\""))],
            "[{\"number\":1,\"updated_at\":\"2026-01-01T00:01:00Z\"},\
              {\"number\":2,\"updated_at\":\"2026-01-01T00:02:00Z\"}]",
        );
        let transport = ReplayTransport::from_exchanges(vec![page1]);
        let client = client_for(&transport);
        let cache = ListCache::default();
        let mut attempts = 0u32;
        let limits = Limits {
            max_pages: 10,
            max_items: 2,
        };
        let Outcome::Ok(result) = client
            .list::<TestItem>(URL.to_string(), &cache, true, 0, &mut attempts, limits)
            .await
            .expect("list")
        else {
            panic!("expected Ok");
        };
        assert_eq!(result.items.len(), 2, "the whole page was processed");
        assert!(
            !result.completed,
            "the cap chose to stop here, not the data running out"
        );
        assert_eq!(
            result.resume_from.as_deref(),
            Some(next_url.as_str()),
            "page 2 was never fetched, so resuming must pick it up, not re-read page 1"
        );
    }

    #[tokio::test]
    async fn page_cap_exhausted_after_a_full_page_resumes_from_the_next_page() {
        let next_url = format!("{URL}&page=2");
        let page1 = exchange(
            URL,
            200,
            vec![("Link", &format!("<{next_url}>; rel=\"next\""))],
            "[{\"number\":1,\"updated_at\":\"2026-01-01T00:01:00Z\"},\
              {\"number\":2,\"updated_at\":\"2026-01-01T00:02:00Z\"}]",
        );
        let transport = ReplayTransport::from_exchanges(vec![page1]);
        let client = client_for(&transport);
        let cache = ListCache::default();
        let mut attempts = 0u32;
        let limits = Limits {
            max_pages: 1,
            max_items: 100,
        };
        let Outcome::Ok(result) = client
            .list::<TestItem>(URL.to_string(), &cache, true, 0, &mut attempts, limits)
            .await
            .expect("list")
        else {
            panic!("expected Ok");
        };
        assert_eq!(result.items.len(), 2, "the whole first page was processed");
        assert!(
            !result.completed,
            "there was a next page this call never reached"
        );
        assert_eq!(result.resume_from.as_deref(), Some(next_url.as_str()));
    }

    #[tokio::test]
    async fn reaching_the_old_cursor_is_a_natural_completion() {
        let body = "[{\"number\":2,\"updated_at\":\"2026-01-01T00:05:00Z\"},\
                      {\"number\":1,\"updated_at\":\"2026-01-01T00:01:00Z\"}]";
        let transport = ReplayTransport::from_exchanges(vec![exchange(URL, 200, vec![], body)]);
        let client = client_for(&transport);
        let cache = ListCache {
            since: Some(GithubTimestamp::new("2026-01-01T00:01:00Z")),
            ..Default::default()
        };
        let mut attempts = 0u32;
        let Outcome::Ok(result) = client
            .list::<TestItem>(
                URL.to_string(),
                &cache,
                true,
                0,
                &mut attempts,
                Limits::default(),
            )
            .await
            .expect("list")
        else {
            panic!("expected Ok");
        };
        assert!(result.completed);
        assert!(result.resume_from.is_none());
        assert_eq!(
            result.items.len(),
            1,
            "the item at the old cursor stops the walk before it is parsed"
        );
    }

    #[tokio::test]
    async fn no_next_link_is_a_natural_completion() {
        let transport = ReplayTransport::from_exchanges(vec![exchange(URL, 200, vec![], "[]")]);
        let client = client_for(&transport);
        let cache = ListCache::default();
        let mut attempts = 0u32;
        let Outcome::Ok(result) = client
            .list::<TestItem>(
                URL.to_string(),
                &cache,
                true,
                0,
                &mut attempts,
                Limits::default(),
            )
            .await
            .expect("list")
        else {
            panic!("expected Ok");
        };
        assert!(result.completed);
        assert!(result.resume_from.is_none());
    }

    #[tokio::test]
    async fn an_untrusted_next_link_is_never_requested() {
        let page1 = exchange(
            URL,
            200,
            vec![("Link", "<https://attacker.example/steal>; rel=\"next\"")],
            "[{\"number\":1,\"updated_at\":\"2026-01-01T00:01:00Z\"}]",
        );
        let transport = ReplayTransport::from_exchanges(vec![page1]);
        let client = client_for(&transport);
        let cache = ListCache::default();
        let mut attempts = 0u32;
        let Outcome::Ok(result) = client
            .list::<TestItem>(
                URL.to_string(),
                &cache,
                true,
                0,
                &mut attempts,
                Limits::default(),
            )
            .await
            .expect("list")
        else {
            panic!("expected Ok");
        };
        assert_eq!(
            result.items.len(),
            1,
            "the trusted page's own items are kept"
        );
        assert!(!result.completed);
        assert!(
            result.resume_from.is_none(),
            "resuming from the current page would just hit the same untrusted link again"
        );
        assert_eq!(
            result.blocked_link.as_deref(),
            Some("https://attacker.example/steal"),
            "the caller must be able to raise this as a visible SyncIssue"
        );
        let sent = transport.requests_sent();
        assert_eq!(sent.len(), 1, "only the trusted page was ever requested");
        assert!(!sent[0].url.contains("attacker"));
        assert_eq!(
            transport.remaining(),
            0,
            "no fixture for the attacker host was consumed (none was provided)"
        );
    }

    #[tokio::test]
    async fn a_plain_403_with_no_rate_limit_signal_is_a_client_error() {
        let transport = ReplayTransport::from_exchanges(vec![exchange(
            URL,
            403,
            vec![],
            r#"{"message":"Bad credentials"}"#,
        )]);
        let client = client_for(&transport);
        let cache = ListCache::default();
        let mut attempts = 0u32;
        let err = client
            .list::<TestItem>(
                URL.to_string(),
                &cache,
                true,
                0,
                &mut attempts,
                Limits::default(),
            )
            .await
            .expect_err("a bare 403 with no rate-limit signal must not be a rate limit");
        assert!(matches!(err, ClientError::Status { status: 403, .. }));
    }

    #[tokio::test]
    async fn r28_a_hostile_retry_after_is_capped_not_overflowed() {
        // Stream Q's open-r28-retry-after-overflow regression: `retry-after: 9223372036854775807`
        // (i64::MAX) used to overflow `now_unix + retry_after` — a panic with overflow checks, a
        // deadline wrapped into the past in release, so the very next call would retry at once
        // instead of actually backing off.
        let transport = ReplayTransport::from_exchanges(vec![exchange(
            URL,
            429,
            vec![("Retry-After", "9223372036854775807")],
            "",
        )]);
        let client = client_for(&transport);
        let cache = ListCache::default();
        let mut attempts = 0u32;
        let now_unix = 1_790_755_200;
        let Outcome::RateLimited { until, secondary } = client
            .list::<TestItem>(
                URL.to_string(),
                &cache,
                true,
                now_unix,
                &mut attempts,
                Limits::default(),
            )
            .await
            .expect("list")
        else {
            panic!("expected RateLimited");
        };
        assert!(secondary);
        assert!(
            until <= now_unix + SECONDARY_BACKOFF_CAP_SECS,
            "a hostile retry-after must be capped, not trusted outright: {until}"
        );
        assert!(
            until > now_unix,
            "a hostile retry-after must not wrap into a deadline already in the past: {until}"
        );
    }

    #[tokio::test]
    async fn a_403_naming_a_secondary_limit_in_its_body_is_rate_limited_without_retry_after() {
        let transport = ReplayTransport::from_exchanges(vec![exchange(
            URL,
            403,
            vec![("X-RateLimit-Remaining", "5")],
            r#"{"message":"You have exceeded a secondary rate limit. Please wait and retry."}"#,
        )]);
        let client = client_for(&transport);
        let cache = ListCache::default();
        let mut attempts = 0u32;
        let Outcome::RateLimited { secondary, .. } = client
            .list::<TestItem>(
                URL.to_string(),
                &cache,
                true,
                0,
                &mut attempts,
                Limits::default(),
            )
            .await
            .expect("list")
        else {
            panic!("expected RateLimited");
        };
        assert!(secondary);
    }

    #[test]
    fn body_mentions_rate_limit_only_checks_the_message_field() {
        // A rate-limit-shaped word elsewhere in the body (not the `message` field) must not count:
        // otherwise an unrelated error whose other fields happen to echo back user-supplied text
        // containing "rate limit" would be mistaken for a secondary limit that will clear on its
        // own, rather than surfaced as the real error it is.
        let body = br#"{"message":"Bad credentials","documentation_url":"https://docs.github.com/rate-limit-troubleshooting"}"#;
        assert!(!body_mentions_rate_limit(body));
    }

    #[test]
    fn body_mentions_rate_limit_recognises_the_real_shape() {
        let body = br#"{"message":"You have exceeded a secondary rate limit. Please wait."}"#;
        assert!(body_mentions_rate_limit(body));
    }

    #[test]
    fn body_mentions_rate_limit_rejects_non_json() {
        assert!(!body_mentions_rate_limit(
            b"plain text mentioning a rate limit"
        ));
    }

    #[tokio::test]
    async fn a_malformed_updated_at_is_excluded_from_the_cursor_but_the_item_still_parses() {
        let body = "[\
            {\"number\":1,\"updated_at\":\"2026-01-01T00:01:00Z\"},\
            {\"number\":2,\"updated_at\":\"not-a-timestamp\"},\
            {\"number\":3,\"updated_at\":\"2026-01-01T00:03:00Z\"}]";
        let transport = ReplayTransport::from_exchanges(vec![exchange(URL, 200, vec![], body)]);
        let client = client_for(&transport);
        let cache = ListCache::default();
        let mut attempts = 0u32;
        let Outcome::Ok(result) = client
            .list::<TestItem>(
                URL.to_string(),
                &cache,
                false,
                0,
                &mut attempts,
                Limits::default(),
            )
            .await
            .expect("list")
        else {
            panic!("expected Ok");
        };
        assert_eq!(
            result.items.len(),
            3,
            "an item with a malformed timestamp still parses; only the cursor ignores it"
        );
        assert_eq!(
            result.max_updated_at.as_ref().map(GithubTimestamp::as_str),
            Some("2026-01-01T00:03:00Z")
        );
        assert_eq!(
            result.oldest_seen.as_ref().map(GithubTimestamp::as_str),
            Some("2026-01-01T00:01:00Z")
        );
    }
}
