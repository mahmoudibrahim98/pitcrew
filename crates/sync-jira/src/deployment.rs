//! Two deployments behind one trait.
//!
//! - **Jira Cloud** ([`JiraCloud`]): REST v3, `GET .../search/jql`, `nextPageToken` pagination.
//! - **Jira Data Center** ([`JiraDataCenter`]): REST v2, `GET .../search`, `startAt` pagination.
//!
//! Both are read with `GET`: Jira Cloud's `/rest/api/3/search/jql` accepts a request body as query
//! parameters as well as JSON, and this crate's queries always fit (`jql`, `fields` and
//! `nextPageToken` are all bounded — see [`crate::jql`] and [`crate::bounds`]). That keeps every
//! request this crate ever sends a `GET`, so it reuses `pitcrew_sync_github::transport::Method`
//! (which has only a `Get` variant — "this brief is read-only") exactly as it is, rather than
//! widening that already-reviewed enum for a Jira-specific need.
//!
//! [`Deployment`] is the seam `crate::client::JiraClient` is generic over, the same way
//! `pitcrew_sync_github`'s client is generic over `Transport`: production code only ever
//! constructs [`JiraCloud`] or [`JiraDataCenter`], but nothing here closes the trait off to a
//! third implementation (a self-hosted fork with its own pagination quirks, say).

use crate::auth::JiraAuth;
use crate::bounds::MAX_RESULTS_PER_PAGE;
use pitcrew_sync_github::transport::{Method, Request};
use serde_json::Value;

/// Where one page's pagination left off. Opaque outside a [`Deployment`] impl and
/// `crate::client::JiraClient`, which only ever threads it from [`Deployment::first_page`]
/// through to the next call's [`Deployment::build_search_request`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PageState {
    /// Jira Cloud: the token to resume from, or `None` for the first page.
    Cloud {
        /// Resume token.
        next_page_token: Option<String>,
    },
    /// Jira Data Center: the zero-based result offset.
    DataCenter {
        /// Offset.
        start_at: u64,
    },
}

/// What distinguishes the two deployments: how to ask for one page of a search, and how to read
/// the next page (if any) out of its response.
pub trait Deployment {
    /// The `PageState` a first call starts from.
    fn first_page(&self) -> PageState;

    /// Builds the request for one page of `jql`, asking only for `fields`.
    fn build_search_request(
        &self,
        api_base: &str,
        auth: &JiraAuth,
        jql: &str,
        fields: &[&str],
        page: &PageState,
    ) -> Request;

    /// Parses a response body into its raw issue values and the next page to fetch (`None` once
    /// this was the last page). `Err` means the page itself did not parse as this deployment's
    /// search-response shape at all (not an individual malformed issue within it, which
    /// `crate::client::JiraClient::search` handles item-by-item).
    fn parse_search_page(
        &self,
        body: &[u8],
    ) -> Result<(Vec<Value>, Option<PageState>), serde_json::Error>;
}

fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn get_request(api_base: &str, path: &str, auth: &JiraAuth, params: &[(&str, String)]) -> Request {
    let mut url = format!("{api_base}{path}");
    for (i, (name, value)) in params.iter().enumerate() {
        url.push(if i == 0 { '?' } else { '&' });
        url.push_str(name);
        url.push('=');
        url.push_str(&percent_encode(value));
    }
    Request {
        method: Method::Get,
        url,
        headers: vec![
            ("Accept".to_string(), "application/json".to_string()),
            ("Authorization".to_string(), auth.header_value()),
        ],
        body: Vec::new(),
    }
}

/// Jira Cloud: REST v3, Basic auth (account e-mail + API token), `GET .../search/jql` with
/// `nextPageToken` pagination.
#[derive(Debug, Clone, Copy, Default)]
pub struct JiraCloud;

impl Deployment for JiraCloud {
    fn first_page(&self) -> PageState {
        PageState::Cloud {
            next_page_token: None,
        }
    }

    fn build_search_request(
        &self,
        api_base: &str,
        auth: &JiraAuth,
        jql: &str,
        fields: &[&str],
        page: &PageState,
    ) -> Request {
        let mut params = vec![
            ("jql", jql.to_string()),
            ("maxResults", MAX_RESULTS_PER_PAGE.to_string()),
            ("fields", fields.join(",")),
        ];
        if let PageState::Cloud {
            next_page_token: Some(token),
        } = page
        {
            params.push(("nextPageToken", token.clone()));
        }
        get_request(api_base, "/search/jql", auth, &params)
    }

    fn parse_search_page(
        &self,
        body: &[u8],
    ) -> Result<(Vec<Value>, Option<PageState>), serde_json::Error> {
        let page: crate::wire::CloudSearchPage = serde_json::from_slice(body)?;
        let next = page.next_page_token.map(|token| PageState::Cloud {
            next_page_token: Some(token),
        });
        Ok((page.issues, next))
    }
}

/// Jira Data Center: REST v2, a Bearer personal access token, `GET .../search` with `startAt`
/// pagination.
///
/// **Known limitation:** `startAt` is a raw numeric offset into a query re-executed fresh on
/// every page request, not a cursor over a fixed snapshot. If the underlying result set changes
/// between two page fetches *within one paginated walk* — an issue's `updated` moves it across
/// the page boundary, or it enters or leaves the filtered set entirely — an item can in rare
/// cases be skipped or repeated in that one call. [`crate::jql::incremental_query`]'s `key ASC`
/// tie-break removes the most common source of such reordering (two issues tied on the exact same
/// `updated` instant sorting differently between requests), but it cannot remove reordering caused
/// by a genuine concurrent write landing mid-walk — there is no `startAt`-based fix for that
/// within the REST v2 search API itself. In practice this self-heals: a skipped item's `updated`
/// is at or after the old cursor, so it is still `>= cursor` and gets picked up again by the very
/// next sync call (see `crate::sync`'s "self-healing cursor" reasoning). [`JiraCloud`]'s
/// `nextPageToken` is not a raw offset and does not share this limitation.
#[derive(Debug, Clone, Copy, Default)]
pub struct JiraDataCenter;

impl Deployment for JiraDataCenter {
    fn first_page(&self) -> PageState {
        PageState::DataCenter { start_at: 0 }
    }

    fn build_search_request(
        &self,
        api_base: &str,
        auth: &JiraAuth,
        jql: &str,
        fields: &[&str],
        page: &PageState,
    ) -> Request {
        let start_at = match page {
            PageState::DataCenter { start_at } => *start_at,
            PageState::Cloud { .. } => 0, // never mixed by a correctly-constructed caller
        };
        let params = vec![
            ("jql", jql.to_string()),
            ("startAt", start_at.to_string()),
            ("maxResults", MAX_RESULTS_PER_PAGE.to_string()),
            ("fields", fields.join(",")),
        ];
        get_request(api_base, "/search", auth, &params)
    }

    fn parse_search_page(
        &self,
        body: &[u8],
    ) -> Result<(Vec<Value>, Option<PageState>), serde_json::Error> {
        let page: crate::wire::DataCenterSearchPage = serde_json::from_slice(body)?;
        let fetched_so_far = page.start_at + page.issues.len() as u64;
        let next = (fetched_so_far < page.total).then_some(PageState::DataCenter {
            start_at: fetched_so_far,
        });
        Ok((page.issues, next))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn auth() -> JiraAuth {
        JiraAuth::Bearer {
            token: "pat-test-not-real".to_string(),
        }
    }

    #[test]
    fn cloud_first_page_has_no_token() {
        assert_eq!(
            JiraCloud.first_page(),
            PageState::Cloud {
                next_page_token: None
            }
        );
    }

    #[test]
    fn cloud_request_omits_next_page_token_on_the_first_page() {
        let request = JiraCloud.build_search_request(
            "https://jira.example.com/rest/api/3",
            &auth(),
            "project in (\"DEMO\")",
            &["summary"],
            &JiraCloud.first_page(),
        );
        assert!(!request.url.contains("nextPageToken"));
        assert!(
            request
                .url
                .starts_with("https://jira.example.com/rest/api/3/search/jql?")
        );
    }

    #[test]
    fn cloud_request_includes_the_token_once_resuming() {
        let page = PageState::Cloud {
            next_page_token: Some("abc".to_string()),
        };
        let request = JiraCloud.build_search_request(
            "https://jira.example.com/rest/api/3",
            &auth(),
            "project in (\"DEMO\")",
            &["summary"],
            &page,
        );
        assert!(request.url.contains("nextPageToken=abc"));
    }

    #[test]
    fn cloud_page_with_no_next_token_is_the_last_page() {
        let body = br#"{"issues":[{"id":"1"}]}"#;
        let (issues, next) = JiraCloud.parse_search_page(body).expect("parses");
        assert_eq!(issues.len(), 1);
        assert_eq!(next, None);
    }

    #[test]
    fn cloud_page_with_a_next_token_resumes_from_it() {
        let body = br#"{"issues":[],"nextPageToken":"abc"}"#;
        let (_, next) = JiraCloud.parse_search_page(body).expect("parses");
        assert_eq!(
            next,
            Some(PageState::Cloud {
                next_page_token: Some("abc".to_string())
            })
        );
    }

    #[test]
    fn data_center_first_page_starts_at_zero() {
        assert_eq!(
            JiraDataCenter.first_page(),
            PageState::DataCenter { start_at: 0 }
        );
    }

    #[test]
    fn data_center_resumes_at_the_fetched_offset_when_more_remain() {
        let body = br#"{"startAt":0,"total":3,"issues":[{"id":"1"},{"id":"2"}]}"#;
        let (issues, next) = JiraDataCenter.parse_search_page(body).expect("parses");
        assert_eq!(issues.len(), 2);
        assert_eq!(next, Some(PageState::DataCenter { start_at: 2 }));
    }

    #[test]
    fn data_center_reports_no_next_page_once_total_is_reached() {
        let body = br#"{"startAt":2,"total":3,"issues":[{"id":"3"}]}"#;
        let (_, next) = JiraDataCenter.parse_search_page(body).expect("parses");
        assert_eq!(next, None);
    }

    #[test]
    fn percent_encode_escapes_jql_operators_and_spaces() {
        let request = JiraDataCenter.build_search_request(
            "https://jira.example.com/rest/api/2",
            &auth(),
            "project in (\"DEMO\") AND updated >= \"2026-01-02 03:04\"",
            &["summary"],
            &JiraDataCenter.first_page(),
        );
        assert!(!request.url.contains(' '), "{}", request.url);
        assert!(!request.url.contains('"'), "{}", request.url);
    }

    #[test]
    fn search_requests_never_carry_the_credential_in_the_url() {
        let request = JiraCloud.build_search_request(
            "https://jira.example.com/rest/api/3",
            &auth(),
            "project in (\"DEMO\")",
            &["summary"],
            &JiraCloud.first_page(),
        );
        assert!(!request.url.contains("pat-test-not-real"));
        assert!(request.headers.iter().any(
            |(k, v)| k.eq_ignore_ascii_case("authorization") && v.contains("pat-test-not-real")
        ));
    }
}
