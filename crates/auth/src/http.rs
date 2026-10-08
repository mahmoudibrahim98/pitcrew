//! What domain crates use in their `routes()`: the caller extractors, the device-only guard, and
//! an error type that renders as an `ApiError` body.
//!
//! The API layer (`pitcrew-api`) authenticates every request and inserts the [`Caller`] into its
//! extensions. Nothing here parses tokens.

use axum::Json;
use axum::Router;
use axum::extract::{FromRequestParts, Request};
use axum::http::header::WWW_AUTHENTICATE;
use axum::http::request::Parts;
use axum::http::{HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use pitcrew_protocol::api::{ApiError, Caller, ErrorCode};

/// The subprotocol a WebSocket route answers with.
pub const WS_PROTOCOL: &str = "pitcrew.v1";
/// Prefix of the subprotocol entry that carries a WebSocket client's token.
pub const WS_BEARER_PREFIX: &str = "pitcrew.bearer.";

/// A failed request, rendered as an [`ApiError`] body with the status of its code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ErrorResponse(pub ApiError);

impl ErrorResponse {
    /// An error with `code` and a message for people.
    #[must_use]
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self(ApiError {
            code,
            message: message.into(),
        })
    }

    /// `401 unauthorized`.
    #[must_use]
    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Unauthorized, message)
    }

    /// `403 forbidden`.
    #[must_use]
    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Forbidden, message)
    }

    /// `404 not_found`.
    #[must_use]
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::NotFound, message)
    }
}

impl From<ApiError> for ErrorResponse {
    fn from(error: ApiError) -> Self {
        Self(error)
    }
}

impl IntoResponse for ErrorResponse {
    fn into_response(self) -> Response {
        let status = StatusCode::from_u16(self.0.code.http_status())
            .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        if self.0.code == ErrorCode::Unauthorized {
            let challenge = [(WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"))];
            return (status, challenge, Json(self.0)).into_response();
        }
        (status, Json(self.0)).into_response()
    }
}

fn missing_caller() -> ErrorResponse {
    // Only reachable if a route is mounted outside the API layer's authentication.
    ErrorResponse::unauthorized("This route needs a bearer token.")
}

/// Extracts the authenticated caller, of either scope.
///
/// Equivalent to `axum::Extension<Caller>`, but a missing caller is a `401` `ApiError` body.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Authenticated(pub Caller);

impl<S: Send + Sync> FromRequestParts<S> for Authenticated {
    type Rejection = ErrorResponse;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<Caller>()
            .copied()
            .map(Self)
            .ok_or_else(missing_caller)
    }
}

/// Extracts the caller and requires a device token (a person); an agent gets `403 forbidden`.
/// Use it on a single handler; for whole routers use [`device_only`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Person(pub Caller);

impl<S: Send + Sync> FromRequestParts<S> for Person {
    type Rejection = ErrorResponse;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let caller = parts
            .extensions
            .get::<Caller>()
            .copied()
            .ok_or_else(missing_caller)?;
        if caller.is_person() {
            Ok(Self(caller))
        } else {
            Err(needs_device(parts.method.as_str(), parts.uri.path()))
        }
    }
}

fn needs_device(method: &str, path: &str) -> ErrorResponse {
    ErrorResponse::forbidden(format!("{method} {path} needs a device token."))
}

/// Middleware that lets only device tokens through. Agents get `403 forbidden`.
///
/// ```ignore
/// router.layer(axum::middleware::from_fn(pitcrew_auth::require_device))
/// ```
///
/// Use `layer`, not `route_layer`: `route_layer` skips the fallbacks of nested routers.
pub async fn require_device(request: Request, next: Next) -> Response {
    match request.extensions().get::<Caller>() {
        Some(caller) if caller.is_person() => next.run(request).await,
        Some(_) => needs_device(request.method().as_str(), request.uri().path()).into_response(),
        None => missing_caller().into_response(),
    }
}

/// Marks every route already in `router` as device-only (see [`require_device`]), including the
/// fallbacks of routers nested in it.
pub fn device_only<S>(router: Router<S>) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    router.layer(middleware::from_fn(require_device))
}

/// Middleware that refuses session tokens (`TokenScope::Session`): `403 forbidden`, before the
/// route sees the request. A session token may call only the routes mounted for it (in
/// `pitcrew-api`, `RouterParts::session`); every other route that agents may call has this.
///
/// Use `layer`, not `route_layer`: `route_layer` skips the fallbacks of nested routers.
pub async fn refuse_session(request: Request, next: Next) -> Response {
    match request.extensions().get::<Caller>() {
        Some(caller) if caller.scope.session().is_some() => ErrorResponse::forbidden(format!(
            "{} {} is refused: this token may only answer for the session it was made for.",
            request.method(),
            request.uri().path()
        ))
        .into_response(),
        Some(_) => next.run(request).await,
        None => missing_caller().into_response(),
    }
}

/// Closes every route already in `router` to session tokens (see [`refuse_session`]), including
/// the fallbacks of routers nested in it.
pub fn no_session<S>(router: Router<S>) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    router.layer(middleware::from_fn(refuse_session))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{Body, to_bytes};
    use axum::routing::get;
    use pitcrew_protocol::MemberId;
    use pitcrew_protocol::api::TokenScope;
    use tower::ServiceExt as _;

    fn caller(scope: TokenScope) -> Caller {
        Caller {
            member: MemberId::new(),
            scope,
            on_behalf_of: (scope == TokenScope::Agent).then(MemberId::new),
        }
    }

    async fn call(router: Router, caller: Option<Caller>, path: &str) -> (u16, serde_json::Value) {
        let mut request = Request::builder().uri(path).body(Body::empty()).unwrap();
        if let Some(caller) = caller {
            request.extensions_mut().insert(caller);
        }
        let response = router.oneshot(request).await.unwrap();
        let status = response.status().as_u16();
        let body = to_bytes(response.into_body(), 1 << 20).await.unwrap();
        (status, serde_json::from_slice(&body).unwrap_or_default())
    }

    fn app() -> Router {
        let device = device_only(Router::new().route(
            "/layer",
            get(|Authenticated(c): Authenticated| async move { Json(c) }),
        ));
        Router::new()
            .route(
                "/extractor",
                get(|Person(c): Person| async move { Json(c) }),
            )
            .route(
                "/any",
                get(|Authenticated(c): Authenticated| async move { Json(c) }),
            )
            .merge(device)
    }

    #[tokio::test]
    async fn devices_pass_and_agents_are_forbidden() {
        let person = caller(TokenScope::Device);
        let agent = caller(TokenScope::Agent);
        for path in ["/layer", "/extractor"] {
            let (status, body) = call(app(), Some(person), path).await;
            assert_eq!(status, 200);
            assert_eq!(body, serde_json::to_value(person).unwrap());

            let (status, body) = call(app(), Some(agent), path).await;
            assert_eq!(status, 403);
            assert_eq!(body["code"], "forbidden");
        }
        let (status, body) = call(app(), Some(agent), "/any").await;
        assert_eq!(status, 200);
        assert_eq!(body, serde_json::to_value(agent).unwrap());
    }

    #[tokio::test]
    async fn session_tokens_pass_only_where_they_are_not_refused() {
        let session = Caller {
            member: MemberId::new(),
            scope: TokenScope::Session(pitcrew_protocol::ids::SessionId::new()),
            on_behalf_of: Some(MemberId::new()),
        };
        let open = Router::new().route(
            "/session",
            get(|Authenticated(c): Authenticated| async move { Json(c) }),
        );
        let closed = no_session(Router::new().route(
            "/agent",
            get(|Authenticated(c): Authenticated| async move { Json(c) }),
        ));
        let app = app().merge(open).merge(closed);
        for path in ["/layer", "/extractor", "/agent"] {
            let (status, body) = call(app.clone(), Some(session), path).await;
            assert_eq!(status, 403, "{path}");
            assert_eq!(body["code"], "forbidden");
        }
        let (status, body) = call(app.clone(), Some(session), "/session").await;
        assert_eq!(status, 200);
        assert_eq!(body, serde_json::to_value(session).unwrap());
        // An agent's own token still reaches the routes closed to session tokens.
        let agent = caller(TokenScope::Agent);
        assert_eq!(call(app, Some(agent), "/agent").await.0, 200);
    }

    #[tokio::test]
    async fn a_missing_caller_is_unauthorized() {
        for path in ["/layer", "/extractor", "/any"] {
            let (status, body) = call(app(), None, path).await;
            assert_eq!(status, 401);
            assert_eq!(body["code"], "unauthorized");
        }
    }
}
