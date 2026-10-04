//! The transport seam.
//!
//! This crate never depends on an HTTP client or a TLS stack: the real HTTPS transport is a
//! dependency decision the integrator makes later (see the brief). Callers hand the client core a
//! [`Transport`] impl; tests use the recorded-fixture transports in [`crate::fixture`].
//!
//! The auth token is threaded through as a header value on each [`Request`] and is never shown in
//! `Debug`, errors or logs: [`AuthToken`]'s `Debug` impl always prints `AuthToken(***)`, and
//! [`Request`]'s `Debug` impl redacts the `Authorization` header.

use std::fmt;
use std::future::Future;

/// HTTP methods. The sync only ever sends `GET`; the others are for an approved outward write
/// ([`crate::write`], and `pitcrew_sync_jira::write`), which the hub sends only after a person's
/// approval.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Method {
    /// `GET`.
    Get,
    /// `POST`: create an issue, a comment, or a Jira transition.
    Post,
    /// `PATCH`: change a GitHub issue.
    Patch,
    /// `PUT`: change a Jira issue.
    Put,
}

impl Method {
    /// The method name as GitHub (and HTTP) expects it.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Method::Get => "GET",
            Method::Post => "POST",
            Method::Patch => "PATCH",
            Method::Put => "PUT",
        }
    }
}

/// A bearer token for the GitHub API. Holds the value, but never shows it: there is no `Display`
/// impl, and `Debug` always prints `AuthToken(***)`.
#[derive(Clone)]
pub struct AuthToken(String);

impl AuthToken {
    /// Wraps a token value.
    #[must_use]
    pub fn new(token: impl Into<String>) -> Self {
        Self(token.into())
    }

    /// The `Authorization` header value to send. This is the only place the real value is
    /// exposed; callers must not log or print the result.
    #[must_use]
    pub fn header_value(&self) -> String {
        format!("Bearer {}", self.0)
    }
}

impl fmt::Debug for AuthToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AuthToken(***)")
    }
}

/// A header name considered sensitive enough to redact from `Debug` output.
fn is_sensitive_header(name: &str) -> bool {
    name.eq_ignore_ascii_case("authorization")
}

/// Headers with any sensitive value replaced by `***`, for safe `Debug` printing.
fn redacted_headers(headers: &[(String, String)]) -> Vec<(String, String)> {
    headers
        .iter()
        .map(|(k, v)| {
            if is_sensitive_header(k) {
                (k.clone(), "***".to_string())
            } else {
                (k.clone(), v.clone())
            }
        })
        .collect()
}

/// One HTTP request. `Debug` redacts the `Authorization` header and never prints the body, only
/// its length, since a body could in principle carry sensitive content.
#[derive(Clone)]
pub struct Request {
    /// Method.
    pub method: Method,
    /// Full URL, including query string. Never carries the token (ADR-0006: never a query
    /// string).
    pub url: String,
    /// Header name/value pairs.
    pub headers: Vec<(String, String)>,
    /// Body bytes: empty for a `GET`, JSON for a write.
    pub body: Vec<u8>,
}

impl fmt::Debug for Request {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Request")
            .field("method", &self.method)
            .field("url", &self.url)
            .field("headers", &redacted_headers(&self.headers))
            .field("body_len", &self.body.len())
            .finish()
    }
}

impl Request {
    /// Looks up a header by name, case-insensitively.
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// A response. `Debug` never prints the body, only its length: responses are server-controlled
/// and can be large.
#[derive(Clone)]
pub struct Response {
    /// HTTP status code.
    pub status: u16,
    /// Header name/value pairs, as received.
    pub headers: Vec<(String, String)>,
    /// Body bytes.
    pub body: Vec<u8>,
}

impl fmt::Debug for Response {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Response")
            .field("status", &self.status)
            .field("headers", &self.headers)
            .field("body_len", &self.body.len())
            .finish()
    }
}

impl Response {
    /// Looks up a header by name, case-insensitively.
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// Errors a [`Transport`] impl may report. The real HTTPS transport (added later) maps its own
/// errors into this; implementations must not put the token or other header values in `reason`.
#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    /// The request could not be sent, or the response could not be read.
    #[error("request to {url} failed: {reason}")]
    Failed {
        /// The URL (never carries the token).
        url: String,
        /// A short, non-sensitive description.
        reason: String,
    },
}

/// Sends one request and awaits its response. The real HTTPS transport is a dependency decision
/// for later; this brief ships only recorded-fixture transports for tests (see
/// [`crate::fixture`]).
///
/// Returning `impl Future` (rather than a boxed future via `#[async_trait]`) keeps this crate free
/// of any async-trait dependency; callers are generic over `T: Transport` rather than holding a
/// `dyn Transport`.
pub trait Transport: Send + Sync {
    /// Sends `request` and returns its response, or a transport-level error (not an HTTP error
    /// status, which is a normal [`Response`]).
    fn send(
        &self,
        request: Request,
    ) -> impl Future<Output = Result<Response, TransportError>> + Send;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_token_debug_never_shows_the_value() {
        let token = AuthToken::new("ghp_supersecretvalue");
        assert_eq!(format!("{token:?}"), "AuthToken(***)");
    }

    #[test]
    fn request_debug_redacts_authorization_only() {
        let request = Request {
            method: Method::Get,
            url: "https://api.github.com/repos/example-org/demo-repo/issues".to_string(),
            headers: vec![
                (
                    "Authorization".to_string(),
                    "Bearer ghp_supersecretvalue".to_string(),
                ),
                (
                    "Accept".to_string(),
                    "application/vnd.github+json".to_string(),
                ),
            ],
            body: Vec::new(),
        };
        let shown = format!("{request:?}");
        assert!(!shown.contains("ghp_supersecretvalue"));
        assert!(shown.contains("application/vnd.github+json"));
        assert!(shown.contains("***"));
    }

    #[test]
    fn header_lookup_is_case_insensitive() {
        let request = Request {
            method: Method::Get,
            url: "https://api.github.com".to_string(),
            headers: vec![("ETag".to_string(), "\"abc\"".to_string())],
            body: Vec::new(),
        };
        assert_eq!(request.header("etag"), Some("\"abc\""));
        assert_eq!(request.header("missing"), None);
    }
}
