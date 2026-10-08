//! CORS for development TCP only, so a browser on the UI's dev server can call the daemon, as it
//! calls the mock hub. The rules are the mock hub's (`apps/mock-hub/src/server.ts`):
//!
//! - every response varies by `Origin`;
//! - only local origins (`http://localhost:<port>`, `http://127.0.0.1:<port>`) and the Tauri
//!   app's are allowed, and only they get `Access-Control-Allow-*` headers;
//! - a preflight (`OPTIONS`) is answered here, before authentication: `204`, or `403` for an
//!   origin that is not allowed. So is a WebSocket upgrade from such an origin.
//!
//! The private socket and pipe never see a browser, so they get none of this. Tokens are still
//! required for everything else; the API's `Host` check against DNS rebinding runs first.

use axum::extract::Request;
use axum::http::header::{
    ACCESS_CONTROL_ALLOW_HEADERS, ACCESS_CONTROL_ALLOW_METHODS, ACCESS_CONTROL_ALLOW_ORIGIN,
    ACCESS_CONTROL_MAX_AGE, ORIGIN, UPGRADE, VARY,
};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use pitcrew_auth::ErrorResponse;

/// The Tauri app's origins: macOS and Linux use its scheme, Windows `tauri.localhost`.
const TAURI_ORIGINS: [&str; 3] = [
    "tauri://localhost",
    "http://tauri.localhost",
    "https://tauri.localhost",
];

/// The middleware. Add it with `axum::middleware::from_fn(cors)`.
pub async fn cors(request: Request, next: Next) -> Response {
    let origin = request.headers().get(ORIGIN).cloned();
    let allowed = origin
        .as_ref()
        .and_then(|o| o.to_str().ok())
        .is_some_and(is_allowed_origin);
    let refused = || {
        let shown = origin
            .as_ref()
            .and_then(|o| o.to_str().ok())
            .unwrap_or("(not text)");
        ErrorResponse::forbidden(format!(
            "Origin {shown} is not allowed; use http://localhost:PORT or http://127.0.0.1:PORT."
        ))
        .into_response()
    };
    let mut response = if request.method() == Method::OPTIONS {
        if origin.is_some() && !allowed {
            refused()
        } else {
            StatusCode::NO_CONTENT.into_response()
        }
    } else if origin.is_some() && !allowed && is_websocket_upgrade(request.headers()) {
        refused()
    } else {
        next.run(request).await
    };
    let headers = response.headers_mut();
    headers.append(VARY, HeaderValue::from_static("Origin"));
    if let (Some(origin), true) = (origin, allowed) {
        headers.insert(ACCESS_CONTROL_ALLOW_ORIGIN, origin);
        headers.insert(
            ACCESS_CONTROL_ALLOW_METHODS,
            HeaderValue::from_static("GET, POST, PUT, PATCH, DELETE, OPTIONS"),
        );
        headers.insert(
            ACCESS_CONTROL_ALLOW_HEADERS,
            HeaderValue::from_static("Authorization, Content-Type"),
        );
        headers.insert(ACCESS_CONTROL_MAX_AGE, HeaderValue::from_static("600"));
    }
    response
}

/// `http://localhost[:port]`, `http://127.0.0.1[:port]`, and the Tauri app's origins.
fn is_allowed_origin(origin: &str) -> bool {
    if TAURI_ORIGINS.contains(&origin) {
        return true;
    }
    let Some(rest) = origin
        .strip_prefix("http://localhost")
        .or_else(|| origin.strip_prefix("http://127.0.0.1"))
    else {
        return false;
    };
    match rest.strip_prefix(':') {
        None => rest.is_empty(),
        Some(port) => (1..=5).contains(&port.len()) && port.bytes().all(|b| b.is_ascii_digit()),
    }
}

fn is_websocket_upgrade(headers: &HeaderMap) -> bool {
    headers
        .get_all(UPGRADE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .any(|v| v.eq_ignore_ascii_case("websocket"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_local_and_tauri_origins_are_allowed() {
        for origin in [
            "http://localhost:5173",
            "http://127.0.0.1:1420",
            "http://localhost",
            "tauri://localhost",
            "http://tauri.localhost",
            "https://tauri.localhost",
        ] {
            assert!(is_allowed_origin(origin), "{origin}");
        }
        for origin in [
            "https://evil.example",
            "http://localhost.evil.example",
            "https://localhost:5173",
            "http://127.0.0.1.evil.example",
            "http://localhost:",
            "http://localhost:123456",
            "http://localhost:80/path",
            "null",
            "",
        ] {
            assert!(!is_allowed_origin(origin), "{origin}");
        }
    }
}
