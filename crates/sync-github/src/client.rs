//! The client core: REST v3 over a [`Transport`], with pagination, conditional requests and
//! rate-limit handling.

use crate::bounds::{MAX_ITEMS_PER_SYNC, MAX_PAGE_BODY_BYTES, MAX_PAGES_PER_CALL, backoff_secs};
use crate::link_header::next_link;
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
pub(crate) struct ListResult<Item> {
    pub items: Vec<Item>,
    pub max_updated_at: Option<GithubTimestamp>,
    pub malformed_skipped: u32,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub not_modified: bool,
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

    /// Lists one resource, following `Link: rel="next"` up to [`MAX_PAGES_PER_CALL`] and at most
    /// [`MAX_ITEMS_PER_SYNC`] items, honouring conditional requests and rate limits.
    ///
    /// When `stop_at_cursor` is true (pull requests, whose list endpoint has no `since`
    /// parameter), an item whose `updated_at` is not after `cache.since` ends the whole listing:
    /// everything from here on (the rest of this page and all later pages) is older still,
    /// because the list is sorted by `updated` descending.
    pub(crate) async fn list<Item: DeserializeOwned>(
        &self,
        first_url: String,
        cache: &ListCache,
        stop_at_cursor: bool,
        now_unix: i64,
        attempts: &mut u32,
    ) -> Result<Outcome<ListResult<Item>>, ClientError> {
        let mut url = first_url;
        let mut items = Vec::new();
        let mut malformed = 0u32;
        let mut max_updated: Option<GithubTimestamp> = None;
        let mut etag = cache.etag.clone();
        let mut last_modified = cache.last_modified.clone();

        for _page in 0..MAX_PAGES_PER_CALL {
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
                let until = if let Some(retry_after) = raw.retry_after {
                    now_unix + retry_after
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

            if raw.body.len() > MAX_PAGE_BODY_BYTES {
                malformed += 1;
                break;
            }
            let Ok(values) = serde_json::from_slice::<Vec<serde_json::Value>>(&raw.body) else {
                malformed += 1;
                break;
            };

            let mut stop = false;
            for value in values {
                if items.len() >= MAX_ITEMS_PER_SYNC {
                    stop = true;
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
                    break;
                }
                match serde_json::from_value::<Item>(value) {
                    Ok(item) => {
                        if let Some(u) = updated {
                            let ts = GithubTimestamp::new(u);
                            if max_updated.as_ref().is_none_or(|m| &ts > m) {
                                max_updated = Some(ts);
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
                break;
            }
            match raw.link.as_deref().and_then(next_link) {
                Some(next) => url = next,
                None => break,
            }
        }

        Ok(Outcome::Ok(ListResult {
            items,
            max_updated_at: max_updated,
            malformed_skipped: malformed,
            etag,
            last_modified,
            not_modified: false,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
