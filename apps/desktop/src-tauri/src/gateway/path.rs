//! The gateway's path checks. A request path is `/v1/…` with an optional `?query`; it has no `..`
//! segment, no `//`, no `\`, no `#` and no control characters. A socket path is one of the two
//! WebSocket routes. Anything else is `invalid`, before any connection is made.

use super::error::GatewayError;

/// The longest path accepted, query included, in bytes.
pub const MAX_PATH: usize = 8 * 1024;

/// What a socket path opens, which sets its send limit (API v1's own limits).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SocketKind {
    /// `/v1/stream`: 4 KiB per client message.
    Stream,
    /// `/v1/sessions/{id}/terminal`: 1 MiB per client message.
    Terminal,
}

impl SocketKind {
    /// The largest message the webview may send on this socket.
    #[must_use]
    pub const fn send_limit(self) -> usize {
        match self {
            Self::Stream => 4 * 1024,
            Self::Terminal => 1024 * 1024,
        }
    }

    /// For logs.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Stream => "stream",
            Self::Terminal => "terminal",
        }
    }
}

/// Checks a request path and returns it unchanged: the query, if any, is passed on as it is.
///
/// # Errors
/// `invalid`, saying which rule the path breaks.
pub fn check_request_path(path: &str) -> Result<&str, GatewayError> {
    if path.len() > MAX_PATH {
        return Err(GatewayError::invalid(format!(
            "the path is over {MAX_PATH} bytes"
        )));
    }
    if !path.starts_with("/v1/") {
        return Err(GatewayError::invalid("the path must start with /v1/"));
    }
    if path.chars().any(char::is_control) {
        return Err(GatewayError::invalid(
            "the path must not hold control characters",
        ));
    }
    if path.contains('\\') {
        return Err(GatewayError::invalid("the path must not hold a backslash"));
    }
    if path.contains('#') {
        return Err(GatewayError::invalid(
            "the path must not hold a fragment (#)",
        ));
    }
    if !path.bytes().all(|b| b.is_ascii_graphic()) {
        return Err(GatewayError::invalid(
            "the path must be percent-encoded ASCII without spaces",
        ));
    }
    let route = route_of(path);
    if route.contains("//") {
        return Err(GatewayError::invalid("the path must not hold //"));
    }
    if route.split('/').any(is_dot_segment) {
        return Err(GatewayError::invalid(
            "the path must not hold . or .. segments",
        ));
    }
    if path.parse::<http::uri::PathAndQuery>().is_err() {
        return Err(GatewayError::invalid(
            "the path is not a valid request path",
        ));
    }
    Ok(path)
}

/// Checks a socket path: `/v1/stream` or `/v1/sessions/{id}/terminal`, with their queries.
///
/// # Errors
/// `invalid` for any other path, or one that fails [`check_request_path`].
pub fn check_socket_path(path: &str) -> Result<SocketKind, GatewayError> {
    check_request_path(path)?;
    let route = route_of(path);
    if route == "/v1/stream" {
        return Ok(SocketKind::Stream);
    }
    if let Some(id) = route
        .strip_prefix("/v1/sessions/")
        .and_then(|rest| rest.strip_suffix("/terminal"))
        && is_id(id)
    {
        return Ok(SocketKind::Terminal);
    }
    Err(GatewayError::invalid(
        "sockets open only /v1/stream and /v1/sessions/{id}/terminal",
    ))
}

/// Whether `path` names an integration's credential route, `/v1/integrations/{id}/credential`
/// (letter case and percent-escapes aside): only `gateway_integration_credential` sends there, so
/// `gateway_request` refuses it (desktop-gateway.md, "Integration credentials").
#[must_use]
pub fn is_credential_route(path: &str) -> bool {
    let segments: Vec<String> = route_of(path).split('/').map(decoded_lower).collect();
    segments.len() >= 5
        && segments.get(1).is_some_and(|s| s == "v1")
        && segments.get(2).is_some_and(|s| s == "integrations")
        && segments.get(4).is_some_and(|s| s == "credential")
}

/// A path segment with its `%XX` escapes decoded (bytes, lossily), lower-cased.
fn decoded_lower(segment: &str) -> String {
    let bytes = segment.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| char::from(b).to_digit(16);
        match (
            bytes.get(i),
            bytes.get(i + 1).copied().and_then(hex),
            bytes.get(i + 2).copied().and_then(hex),
        ) {
            (Some(b'%'), Some(high), Some(low)) => {
                out.push(u8::try_from(high * 16 + low).unwrap_or(b'%'));
                i += 3;
            }
            (Some(b), _, _) => {
                out.push(*b);
                i += 1;
            }
            (None, _, _) => break,
        }
    }
    String::from_utf8_lossy(&out).to_lowercase()
}

/// An integration's id in a path: a bare ULID or a display id such as `int_…`.
#[must_use]
pub fn is_integration_id(id: &str) -> bool {
    is_id(id)
}

/// The path without its query.
#[must_use]
pub fn route_of(path: &str) -> &str {
    path.split_once('?').map_or(path, |(route, _)| route)
}

/// `.` or `..`, also when percent-encoded (`%2e`).
fn is_dot_segment(segment: &str) -> bool {
    let decoded = segment.replace("%2e", ".").replace("%2E", ".");
    decoded == "." || decoded == ".."
}

/// A session id in a path: a bare ULID or a display id such as `ses_…`.
fn is_id(id: &str) -> bool {
    (1..=64).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway::error::ErrorCode;

    #[test]
    fn request_paths() {
        let ok = [
            "/v1/workspace",
            "/v1/tasks?project=01J&status=todo&status=done",
            "/v1/tasks/PAP-4",
            "/v1/sessions/01J9ZQ/transcript?before=10&limit=200",
            "/v1/events?before=5",
            "/v1/tasks?q=a..b",
            "/v1/x?cwd=%2Fhome%2Fsam",
            "/v1/a.b/c..d",
            "/v1/",
        ];
        for path in ok {
            assert_eq!(check_request_path(path), Ok(path), "{path}");
        }
        let bad = [
            ("", "start"),
            ("/v2/tasks", "start"),
            ("v1/tasks", "start"),
            ("/v1", "start"),
            ("/V1/tasks", "start"),
            ("http://evil/v1/tasks", "start"),
            ("//evil/v1/tasks", "start"),
            ("/v1/../admin", ". or .."),
            ("/v1/tasks/..", ". or .."),
            ("/v1/%2e%2e/admin", ". or .."),
            ("/v1/.%2E/admin", ". or .."),
            ("/v1/./tasks", ". or .."),
            ("/v1//tasks", "//"),
            ("/v1/tasks//", "//"),
            ("/v1/tasks\\x", "backslash"),
            ("/v1/tasks?cwd=C:\\x", "backslash"),
            ("/v1/tasks#frag", "fragment"),
            ("/v1/tasks?a=1#frag", "fragment"),
            ("/v1/tasks\0", "control"),
            ("/v1/tasks\n", "control"),
            ("/v1/tasks\r\nHost: evil", "control"),
            ("/v1/tasks\u{7f}", "control"),
            ("/v1/tasks\u{85}", "control"),
            ("/v1/tasks?x=\t", "control"),
            ("/v1/tasks x", "percent-encoded"),
            ("/v1/tâches", "percent-encoded"),
        ];
        for (path, why) in bad {
            let err = check_request_path(path).unwrap_err();
            assert_eq!(err.code, ErrorCode::Invalid, "{path:?}");
            assert!(err.message.contains(why), "{path:?}: {}", err.message);
        }
        let long = format!("/v1/{}", "a".repeat(MAX_PATH));
        assert!(check_request_path(&long).is_err());
    }

    #[test]
    fn socket_paths() {
        for (path, kind) in [
            ("/v1/stream", SocketKind::Stream),
            ("/v1/stream?since=42", SocketKind::Stream),
            ("/v1/sessions/01J9ZQ3/terminal", SocketKind::Terminal),
            (
                "/v1/sessions/ses_01J9ZQ3/terminal?cols=120&rows=40&from=0",
                SocketKind::Terminal,
            ),
        ] {
            assert_eq!(check_socket_path(path), Ok(kind), "{path}");
        }
        for path in [
            "/v1/tasks",
            "/v1/stream/x",
            "/v1/streams",
            "/v1/sessions//terminal",
            "/v1/sessions/terminal",
            "/v1/sessions/a/b/terminal",
            "/v1/sessions/../terminal",
            "/v1/sessions/%2e%2e/terminal",
            "/v1/sessions/a.b/terminal",
            "/v1/sessions/01J/terminal/x",
            "/v1/stream#x",
            "/v2/stream",
        ] {
            assert_eq!(
                check_socket_path(path).unwrap_err().code,
                ErrorCode::Invalid,
                "{path}"
            );
        }
    }

    #[test]
    fn credential_routes_are_recognised_however_written() {
        for path in [
            "/v1/integrations/01J9ZQ/credential",
            "/v1/integrations/01J9ZQ/credential?x=1",
            "/v1/integrations/01J9ZQ/Credential",
            "/v1/integrations/01J9ZQ/%63redential",
            "/v1/%69ntegrations/01J9ZQ/credential",
            "/v1/integrations/01J9ZQ/credential/",
        ] {
            assert!(is_credential_route(path), "{path}");
        }
        for path in [
            "/v1/integrations",
            "/v1/integrations/01J9ZQ",
            "/v1/integrations/01J9ZQ/test",
            "/v1/integrations/01J9ZQ/sync",
            "/v1/tasks/credential",
            "/v1/integrations/credential",
        ] {
            assert!(!is_credential_route(path), "{path}");
        }
    }

    #[test]
    fn send_limits_are_api_v1s() {
        assert_eq!(SocketKind::Stream.send_limit(), 4096);
        assert_eq!(SocketKind::Terminal.send_limit(), 1 << 20);
    }
}
