//! Authentication: a bearer token in, an `Extension<Caller>` out.
//!
//! - HTTP: `Authorization: Bearer <token>`.
//! - WebSocket upgrades may instead offer `Sec-WebSocket-Protocol: pitcrew.v1,
//!   pitcrew.bearer.<token>`, since browsers cannot set headers there.
//! - Query strings are never read.
//!
//! After a token is accepted it is removed from the request's headers, so nothing downstream can
//! log it.
//!
//! A reader token (`TokenScope::Reader`, the Orchestrator's CLI) may only read: any request of
//! one but a `GET` or `HEAD`, and any WebSocket upgrade, is `403 forbidden` here, before a route
//! sees it or its body is read.

use axum::extract::{Request, State};
use axum::http::header::{AUTHORIZATION, SEC_WEBSOCKET_PROTOCOL, UPGRADE};
use axum::http::{HeaderMap, HeaderValue, Method};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use pitcrew_auth::{ErrorResponse, TokenStore, WS_BEARER_PREFIX};
use std::sync::Arc;

/// Why a request carries no usable token. The messages never include the token.
#[derive(Debug, PartialEq, Eq)]
enum Missing {
    None,
    NotBearer,
    Ambiguous,
}

pub(crate) async fn authenticate(
    State(tokens): State<Arc<dyn TokenStore>>,
    mut request: Request,
    next: Next,
) -> Response {
    let token = match bearer_token(request.headers()) {
        Ok(token) => token,
        Err(missing) => {
            let message = match missing {
                Missing::None => "This route needs `Authorization: Bearer <token>`.",
                Missing::NotBearer => "Only `Authorization: Bearer <token>` is accepted.",
                Missing::Ambiguous => "The request carries more than one token.",
            };
            tracing::debug!(method = %request.method(), path = request.uri().path(), ?missing, "rejected: no token");
            return ErrorResponse::unauthorized(message).into_response();
        }
    };
    let Some(caller) = tokens.verify(&token) else {
        tracing::debug!(method = %request.method(), path = request.uri().path(), "rejected: unknown token");
        return ErrorResponse::unauthorized("Unknown or revoked token.").into_response();
    };
    if caller.reads_only() && !is_read(request.method(), request.headers()) {
        tracing::debug!(method = %request.method(), path = request.uri().path(), "refused: a reader token only reads");
        return ErrorResponse::forbidden(format!(
            "{} {} is refused: this token may only read.",
            request.method(),
            request.uri().path()
        ))
        .into_response();
    }
    scrub(request.headers_mut());
    request.extensions_mut().insert(caller);
    next.run(request).await
}

/// Whether a request only reads: a `GET` or `HEAD` that is not a WebSocket upgrade (a socket
/// carries input, such as a terminal's).
fn is_read(method: &Method, headers: &HeaderMap) -> bool {
    (method == Method::GET || method == Method::HEAD) && !is_websocket_upgrade(headers)
}

fn bearer_token(headers: &HeaderMap) -> Result<String, Missing> {
    let mut found = Vec::new();

    let mut authorization = headers.get_all(AUTHORIZATION).iter();
    if let Some(value) = authorization.next() {
        if authorization.next().is_some() {
            return Err(Missing::Ambiguous);
        }
        let value = value.to_str().map_err(|_| Missing::NotBearer)?;
        let (scheme, token) = value.trim().split_once(' ').ok_or(Missing::NotBearer)?;
        if !scheme.eq_ignore_ascii_case("bearer") || token.trim().is_empty() {
            return Err(Missing::NotBearer);
        }
        found.push(token.trim().to_owned());
    }

    if is_websocket_upgrade(headers) {
        found.extend(
            offered_protocols(headers)
                .filter_map(|p| p.strip_prefix(WS_BEARER_PREFIX).map(str::to_owned)),
        );
    }

    match found.len() {
        0 => Err(Missing::None),
        1 => Ok(found.remove(0)),
        _ => Err(Missing::Ambiguous),
    }
}

fn is_websocket_upgrade(headers: &HeaderMap) -> bool {
    headers
        .get_all(UPGRADE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .any(|v| v.eq_ignore_ascii_case("websocket"))
}

fn offered_protocols(headers: &HeaderMap) -> impl Iterator<Item = &str> {
    headers
        .get_all(SEC_WEBSOCKET_PROTOCOL)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(str::trim)
        .filter(|p| !p.is_empty())
}

/// Drops the token from the headers, keeping the other subprotocols for the WebSocket handshake.
fn scrub(headers: &mut HeaderMap) {
    headers.remove(AUTHORIZATION);
    let kept: Vec<&str> = offered_protocols(headers)
        .filter(|p| !p.starts_with(WS_BEARER_PREFIX))
        .collect();
    let kept = (!kept.is_empty())
        .then(|| HeaderValue::from_str(&kept.join(", ")).ok())
        .flatten();
    headers.remove(SEC_WEBSOCKET_PROTOCOL);
    if let Some(kept) = kept {
        headers.insert(SEC_WEBSOCKET_PROTOCOL, kept);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.append(
                axum::http::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                HeaderValue::from_str(value).unwrap(),
            );
        }
        map
    }

    #[test]
    fn reads_bearer_headers() {
        assert_eq!(
            bearer_token(&headers(&[("authorization", "Bearer abc")])),
            Ok("abc".into())
        );
        assert_eq!(
            bearer_token(&headers(&[("authorization", "bearer  abc ")])),
            Ok("abc".into())
        );
        assert_eq!(
            bearer_token(&headers(&[("authorization", "Basic abc")])),
            Err(Missing::NotBearer)
        );
        assert_eq!(
            bearer_token(&headers(&[("authorization", "Bearer")])),
            Err(Missing::NotBearer)
        );
        assert_eq!(bearer_token(&headers(&[])), Err(Missing::None));
    }

    #[test]
    fn reads_the_websocket_subprotocol_only_on_upgrades() {
        let offer = ("sec-websocket-protocol", "pitcrew.v1, pitcrew.bearer.abc");
        assert_eq!(
            bearer_token(&headers(&[("upgrade", "websocket"), offer])),
            Ok("abc".into())
        );
        assert_eq!(bearer_token(&headers(&[offer])), Err(Missing::None));
        assert_eq!(
            bearer_token(&headers(&[
                ("upgrade", "websocket"),
                offer,
                ("authorization", "Bearer xyz")
            ])),
            Err(Missing::Ambiguous)
        );
    }

    #[test]
    fn reads_any_order_spacing_or_a_bearer_only_offer() {
        for offer in [
            "pitcrew.bearer.abc",
            "pitcrew.bearer.abc, pitcrew.v1",
            "  pitcrew.v1 ,   pitcrew.bearer.abc  ",
        ] {
            let map = headers(&[("upgrade", "WebSocket"), ("sec-websocket-protocol", offer)]);
            assert_eq!(bearer_token(&map), Ok("abc".into()), "{offer:?}");
        }
        // Split across two header lines.
        let map = headers(&[
            ("upgrade", "websocket"),
            ("sec-websocket-protocol", "pitcrew.v1"),
            ("sec-websocket-protocol", "pitcrew.bearer.abc"),
        ]);
        assert_eq!(bearer_token(&map), Ok("abc".into()));
        // Two bearer entries are ambiguous.
        let map = headers(&[
            ("upgrade", "websocket"),
            (
                "sec-websocket-protocol",
                "pitcrew.bearer.a, pitcrew.bearer.b",
            ),
        ]);
        assert_eq!(bearer_token(&map), Err(Missing::Ambiguous));
    }

    #[test]
    fn only_plain_gets_are_reads() {
        assert!(is_read(&Method::GET, &headers(&[])));
        assert!(is_read(&Method::HEAD, &headers(&[])));
        for method in [
            Method::POST,
            Method::PUT,
            Method::PATCH,
            Method::DELETE,
            Method::OPTIONS,
        ] {
            assert!(!is_read(&method, &headers(&[])), "{method}");
        }
        assert!(!is_read(
            &Method::GET,
            &headers(&[("upgrade", "websocket")])
        ));
    }

    #[test]
    fn two_authorization_headers_are_ambiguous() {
        let map = headers(&[("authorization", "Bearer a"), ("authorization", "Bearer a")]);
        assert_eq!(bearer_token(&map), Err(Missing::Ambiguous));
    }

    #[test]
    fn scrub_removes_the_token_and_keeps_other_protocols() {
        let mut map = headers(&[
            ("authorization", "Bearer abc"),
            ("sec-websocket-protocol", "pitcrew.v1, pitcrew.bearer.abc"),
        ]);
        scrub(&mut map);
        assert!(map.get(AUTHORIZATION).is_none());
        assert_eq!(map.get(SEC_WEBSOCKET_PROTOCOL).unwrap(), "pitcrew.v1");

        let mut map = headers(&[("sec-websocket-protocol", "pitcrew.bearer.abc")]);
        scrub(&mut map);
        assert!(map.get(SEC_WEBSOCKET_PROTOCOL).is_none());
    }
}
