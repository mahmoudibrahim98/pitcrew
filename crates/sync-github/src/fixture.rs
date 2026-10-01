//! Recorded-fixture transports: how tests exercise the client core with no network.
//!
//! The on-disk format is deliberately simple text, human-readable and diffable: one block per
//! request/response exchange, separated by a marker line.
//!
//! ```text
//! GET https://api.github.com/repos/example-org/demo-repo/issues?state=all HTTP/1.1
//! Accept: application/vnd.github+json
//!
//! HTTP/1.1 200 OK
//! ETag: "abc123"
//!
//! [{"number":1,"title":"Demo issue","state":"open","updated_at":"2026-01-01T00:00:00Z"}]
//! ### pitcrew-github-fixture ###
//! GET https://api.github.com/repos/example-org/demo-repo/issues?state=all&since=... HTTP/1.1
//! ...
//! ```
//!
//! Response bodies are always written on a single line (minified JSON, or empty).
//! [`ReplayTransport`] answers each request from the first not-yet-used block whose method and
//! URL match, so a fixture file's blocks can be listed in the order a test expects them to be
//! requested. [`RecordingTransport`] wraps a real transport (supplied by the caller — this crate
//! adds none) to capture new fixtures; it scrubs `Authorization` and refuses to run at all unless
//! `PITCREW_RECORD_GITHUB_FIXTURES` is set, so it can never activate in CI.

use crate::transport::{Request, Response, Transport, TransportError};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const SEPARATOR: &str = "### pitcrew-github-fixture ###";

/// One recorded request/response exchange.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordedExchange {
    /// `"GET"`.
    pub method: String,
    /// The full URL requested.
    pub url: String,
    /// Request headers, as sent (with `Authorization`'s value already scrubbed if recorded).
    pub request_headers: Vec<(String, String)>,
    /// Response status code.
    pub status: u16,
    /// Response headers.
    pub response_headers: Vec<(String, String)>,
    /// Response body.
    pub body: Vec<u8>,
}

/// Errors reading or parsing a fixture file.
#[derive(Debug, thiserror::Error)]
pub enum FixtureError {
    /// The file could not be read.
    #[error("fixture I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// The text was not in the expected shape.
    #[error("malformed fixture at block {index}: {reason}")]
    Malformed {
        /// 0-based block index.
        index: usize,
        /// What was wrong.
        reason: String,
    },
}

fn malformed(index: usize, reason: impl Into<String>) -> FixtureError {
    FixtureError::Malformed {
        index,
        reason: reason.into(),
    }
}

fn parse_header_line(line: &str, index: usize) -> Result<(String, String), FixtureError> {
    let (name, value) = line
        .split_once(':')
        .ok_or_else(|| malformed(index, format!("header line without a colon: {line:?}")))?;
    Ok((name.trim().to_string(), value.trim().to_string()))
}

fn parse_block(block: &str, index: usize) -> Result<RecordedExchange, FixtureError> {
    let mut lines = block.lines();
    let request_line = lines
        .next()
        .ok_or_else(|| malformed(index, "empty block"))?;
    let mut parts = request_line.splitn(3, ' ');
    let method = parts
        .next()
        .ok_or_else(|| malformed(index, "missing method"))?
        .to_string();
    let url = parts
        .next()
        .ok_or_else(|| malformed(index, "missing url"))?
        .to_string();

    let mut request_headers = Vec::new();
    for line in lines.by_ref() {
        if line.is_empty() {
            break;
        }
        request_headers.push(parse_header_line(line, index)?);
    }

    let status_line = lines
        .next()
        .ok_or_else(|| malformed(index, "missing status line"))?;
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| malformed(index, format!("bad status line: {status_line:?}")))?;

    let mut response_headers = Vec::new();
    for line in lines.by_ref() {
        if line.is_empty() {
            break;
        }
        response_headers.push(parse_header_line(line, index)?);
    }

    let body: String = lines.collect::<Vec<_>>().join("\n");
    Ok(RecordedExchange {
        method,
        url,
        request_headers,
        status,
        response_headers,
        body: body.into_bytes(),
    })
}

/// Parses a fixture file's text into its exchanges, in file order.
pub fn parse_fixture(text: &str) -> Result<Vec<RecordedExchange>, FixtureError> {
    text.split(SEPARATOR)
        .map(str::trim)
        .filter(|b| !b.is_empty())
        .enumerate()
        .map(|(i, block)| parse_block(block, i))
        .collect()
}

/// Renders exchanges back to the on-disk text format.
#[must_use]
pub fn format_fixture(exchanges: &[RecordedExchange]) -> String {
    let mut out = String::new();
    for (i, ex) in exchanges.iter().enumerate() {
        if i > 0 {
            out.push_str(SEPARATOR);
            out.push('\n');
        }
        out.push_str(&ex.method);
        out.push(' ');
        out.push_str(&ex.url);
        out.push_str(" HTTP/1.1\n");
        for (k, v) in &ex.request_headers {
            out.push_str(k);
            out.push_str(": ");
            out.push_str(v);
            out.push('\n');
        }
        out.push('\n');
        out.push_str(&format!("HTTP/1.1 {}\n", ex.status));
        for (k, v) in &ex.response_headers {
            out.push_str(k);
            out.push_str(": ");
            out.push_str(v);
            out.push('\n');
        }
        out.push('\n');
        out.push_str(&String::from_utf8_lossy(&ex.body));
        out.push('\n');
    }
    out
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// A [`Transport`] answered entirely from a recorded fixture: no network, ever.
///
/// Each `send` takes the first not-yet-used exchange whose method and URL match the request
/// (removed once used, so a fixture can list the same URL twice for two different calls, such as
/// a normal `GET` followed by a conditional one that gets a `304`).
#[derive(Debug)]
pub struct ReplayTransport {
    remaining: Mutex<Vec<RecordedExchange>>,
    seen: Mutex<Vec<Request>>,
}

impl ReplayTransport {
    /// Loads and parses a fixture file.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, FixtureError> {
        let text = fs::read_to_string(path)?;
        Self::from_text(&text)
    }

    /// Parses fixture text directly (handy for small inline tests).
    pub fn from_text(text: &str) -> Result<Self, FixtureError> {
        Ok(Self::from_exchanges(parse_fixture(text)?))
    }

    /// Wraps already-parsed exchanges.
    #[must_use]
    pub fn from_exchanges(exchanges: Vec<RecordedExchange>) -> Self {
        Self {
            remaining: Mutex::new(exchanges),
            seen: Mutex::new(Vec::new()),
        }
    }

    /// How many recorded exchanges have not yet been consumed. Tests use this to assert a fixture
    /// was fully used (or deliberately was not).
    #[must_use]
    pub fn remaining(&self) -> usize {
        lock(&self.remaining).len()
    }

    /// Every request handed to `send`, in order. Tests use this to check the client sent the
    /// right conditional headers (`If-None-Match`, `If-Modified-Since`), not just that it handled
    /// the response correctly.
    #[must_use]
    pub fn requests_sent(&self) -> Vec<Request> {
        lock(&self.seen).clone()
    }
}

impl Transport for ReplayTransport {
    async fn send(&self, request: Request) -> Result<Response, TransportError> {
        lock(&self.seen).push(request.clone());
        let mut guard = lock(&self.remaining);
        let method = request.method.as_str();
        let position = guard
            .iter()
            .position(|e| e.method == method && e.url == request.url);
        match position {
            Some(i) => {
                let exchange = guard.remove(i);
                Ok(Response {
                    status: exchange.status,
                    headers: exchange.response_headers,
                    body: exchange.body,
                })
            }
            None => Err(TransportError::Failed {
                url: request.url,
                reason: "no recorded fixture matches this request".to_string(),
            }),
        }
    }
}

/// Refused to record: `PITCREW_RECORD_GITHUB_FIXTURES` was not set. This is the only way
/// [`RecordingTransport::new`] can fail, and it is deliberate: it keeps recording from ever
/// activating in CI, where that variable is never set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("set PITCREW_RECORD_GITHUB_FIXTURES=1 to enable recording (never set this in CI)")]
pub struct RecordingDisabled;

/// Wraps any [`Transport`] (a real HTTPS one, supplied by the caller — this crate depends on
/// none) to capture a new fixture file while passing every request through unchanged.
/// `Authorization` is scrubbed before anything is written.
pub struct RecordingTransport<'t, T: Transport> {
    inner: &'t T,
    path: PathBuf,
    recorded: Mutex<Vec<RecordedExchange>>,
}

impl<T: Transport> std::fmt::Debug for RecordingTransport<'_, T> {
    // Manual: `inner` is generic and not required to be `Debug`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RecordingTransport")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl<'t, T: Transport> RecordingTransport<'t, T> {
    /// Wraps `inner`, writing exchanges to `path` as they happen. Fails unless
    /// `PITCREW_RECORD_GITHUB_FIXTURES` is set in the environment.
    pub fn new(inner: &'t T, path: impl Into<PathBuf>) -> Result<Self, RecordingDisabled> {
        if std::env::var_os("PITCREW_RECORD_GITHUB_FIXTURES").is_none() {
            return Err(RecordingDisabled);
        }
        Ok(Self::new_enabled(inner, path))
    }

    /// Builds a recorder without checking the environment. Only `pub(crate)`, so the only way to
    /// get one from outside this crate is through `new`, which enforces the env-var gate.
    pub(crate) fn new_enabled(inner: &'t T, path: impl Into<PathBuf>) -> Self {
        Self {
            inner,
            path: path.into(),
            recorded: Mutex::new(Vec::new()),
        }
    }
}

fn scrub(headers: &[(String, String)]) -> Vec<(String, String)> {
    headers
        .iter()
        .map(|(k, v)| {
            if k.eq_ignore_ascii_case("authorization") {
                (k.clone(), "***".to_string())
            } else {
                (k.clone(), v.clone())
            }
        })
        .collect()
}

impl<'t, T: Transport> Transport for RecordingTransport<'t, T> {
    async fn send(&self, request: Request) -> Result<Response, TransportError> {
        let method = request.method.as_str().to_string();
        let url = request.url.clone();
        let request_headers = scrub(&request.headers);
        let response = self.inner.send(request).await?;

        let exchange = RecordedExchange {
            method,
            url,
            request_headers,
            status: response.status,
            response_headers: response.headers.clone(),
            body: response.body.clone(),
        };
        let mut guard = lock(&self.recorded);
        guard.push(exchange);
        if let Err(e) = fs::write(&self.path, format_fixture(&guard)) {
            tracing::warn!(path = %self.path.display(), error = %e, "could not write recorded fixture");
        }
        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::Method;

    fn issue_body() -> &'static str {
        r#"[{"number":1,"title":"Demo issue","state":"open","updated_at":"2026-01-01T00:00:00Z"}]"#
    }

    fn sample_text() -> String {
        format!(
            "GET https://api.github.com/repos/example-org/demo-repo/issues?state=all HTTP/1.1\n\
             Accept: application/vnd.github+json\n\
             \n\
             HTTP/1.1 200 OK\n\
             ETag: \"abc123\"\n\
             \n\
             {}\n",
            issue_body()
        )
    }

    #[test]
    fn parses_one_exchange() {
        let exchanges = parse_fixture(&sample_text()).expect("parse");
        assert_eq!(exchanges.len(), 1);
        let e = &exchanges[0];
        assert_eq!(e.method, "GET");
        assert_eq!(
            e.url,
            "https://api.github.com/repos/example-org/demo-repo/issues?state=all"
        );
        assert_eq!(e.status, 200);
        assert_eq!(
            e.response_headers,
            vec![("ETag".to_string(), "\"abc123\"".to_string())]
        );
        assert_eq!(e.body, issue_body().as_bytes());
    }

    #[test]
    fn round_trips_through_format_and_parse() {
        let original = parse_fixture(&sample_text()).expect("parse");
        let rendered = format_fixture(&original);
        let reparsed = parse_fixture(&rendered).expect("reparse");
        assert_eq!(original, reparsed);
    }

    #[test]
    fn several_blocks_are_separated() {
        let two = format!("{}{}\n{}", sample_text(), SEPARATOR, sample_text());
        let exchanges = parse_fixture(&two).expect("parse");
        assert_eq!(exchanges.len(), 2);
    }

    #[test]
    fn a_missing_status_line_is_malformed() {
        let bad = "GET https://api.github.com/x HTTP/1.1\n\n";
        assert!(matches!(
            parse_fixture(bad),
            Err(FixtureError::Malformed { .. })
        ));
    }

    #[tokio::test]
    async fn replay_answers_from_the_matching_recorded_exchange() {
        let transport = ReplayTransport::from_text(&sample_text()).expect("load");
        let request = Request {
            method: Method::Get,
            url: "https://api.github.com/repos/example-org/demo-repo/issues?state=all".to_string(),
            headers: vec![],
            body: vec![],
        };
        let response = transport.send(request).await.expect("send");
        assert_eq!(response.status, 200);
        assert_eq!(response.header("etag"), Some("\"abc123\""));
        assert_eq!(transport.remaining(), 0);
    }

    #[tokio::test]
    async fn an_unmatched_request_is_an_error() {
        let transport = ReplayTransport::from_exchanges(vec![]);
        let request = Request {
            method: Method::Get,
            url: "https://api.github.com/nope".to_string(),
            headers: vec![],
            body: vec![],
        };
        assert!(transport.send(request).await.is_err());
    }

    #[tokio::test]
    async fn recording_refuses_without_the_env_var() {
        // No test in this crate sets PITCREW_RECORD_GITHUB_FIXTURES, so it is absent here; that
        // is the whole point of the gate (see `new_enabled` below for testing the rest of the
        // behaviour without touching process-global environment state).
        assert!(std::env::var_os("PITCREW_RECORD_GITHUB_FIXTURES").is_none());
        let inner = ReplayTransport::from_exchanges(vec![]);
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(RecordingTransport::new(&inner, dir.path().join("out.fixture")).is_err());
    }

    #[tokio::test]
    async fn recording_scrubs_authorization_and_writes_a_replayable_file() {
        let sample = sample_text();
        let inner = ReplayTransport::from_text(&sample).expect("load");
        let dir = tempfile::tempdir().expect("tempdir");
        let out = dir.path().join("out.fixture");
        let recorder = RecordingTransport::new_enabled(&inner, &out);
        let request = Request {
            method: Method::Get,
            url: "https://api.github.com/repos/example-org/demo-repo/issues?state=all".to_string(),
            headers: vec![(
                "Authorization".to_string(),
                "Bearer ghp_supersecretvalue".to_string(),
            )],
            body: vec![],
        };
        recorder.send(request).await.expect("send");

        let written = fs::read_to_string(&out).expect("read");
        assert!(!written.contains("ghp_supersecretvalue"));
        let replayed = ReplayTransport::load(&out).expect("load recorded");
        assert_eq!(replayed.remaining(), 1);
    }
}
