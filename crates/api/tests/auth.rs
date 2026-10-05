//! Bearer authentication and scopes, through the whole router with `oneshot`.

#![allow(clippy::unwrap_used)]

mod common;

use common::{Fixture, call, get_request};
use pitcrew_auth::TokenStore as _;

#[tokio::test]
async fn no_token_is_unauthorized() {
    let f = Fixture::new();
    for path in ["/v1/me", "/v1/device-only"] {
        let (status, body) = call(f.app(), get_request(path, None)).await;
        assert_eq!(status, 401, "{path}");
        assert_eq!(body["code"], "unauthorized");
        assert!(body["message"].is_string());
    }
}

#[tokio::test]
async fn an_unknown_token_is_unauthorized() {
    let f = Fixture::new();
    let other = pitcrew_auth::FileTokenStore::in_memory();
    let (_, stranger) = other.mint(f.person).unwrap();
    for token in [stranger.expose(), "pcd_nope", "dev-device-token", ""] {
        let (status, body) = call(f.app(), get_request("/v1/me", Some(token))).await;
        assert_eq!(status, 401, "{token:?}");
        assert_eq!(body["code"], "unauthorized");
    }
}

#[tokio::test]
async fn a_token_in_the_query_only_is_unauthorized() {
    let f = Fixture::new();
    for query in ["token", "access_token", "bearer", "authorization"] {
        let path = format!("/v1/me?{query}={}", f.device_token);
        let (status, body) = call(f.app(), get_request(&path, None)).await;
        assert_eq!(status, 401, "{query}");
        assert_eq!(body["code"], "unauthorized");
    }
}

#[tokio::test]
async fn a_non_bearer_scheme_is_unauthorized() {
    let f = Fixture::new();
    let request = axum::http::Request::builder()
        .uri("/v1/me")
        .header("authorization", format!("Basic {}", f.device_token))
        .body(axum::body::Body::empty())
        .unwrap();
    let (status, _) = call(f.app(), request).await;
    assert_eq!(status, 401);
}

#[tokio::test]
async fn an_agent_token_on_a_device_only_route_is_forbidden() {
    let f = Fixture::new();
    let (status, body) = call(
        f.app(),
        get_request("/v1/device-only", Some(&f.agent_token)),
    )
    .await;
    assert_eq!(status, 403);
    assert_eq!(body["code"], "forbidden");
}

#[tokio::test]
async fn a_device_token_passes_and_the_handler_sees_its_caller() {
    let f = Fixture::new();
    for path in ["/v1/me", "/v1/device-only"] {
        let (status, body) = call(f.app(), get_request(path, Some(&f.device_token))).await;
        assert_eq!(status, 200, "{path}");
        assert_eq!(body, serde_json::to_value(f.person).unwrap());
    }
}

#[tokio::test]
async fn an_agent_token_on_an_agent_route_sees_the_agent_and_its_owner() {
    let f = Fixture::new();
    let (status, body) = call(f.app(), get_request("/v1/me", Some(&f.agent_token))).await;
    assert_eq!(status, 200);
    assert_eq!(body, serde_json::to_value(f.agent).unwrap());
    assert_eq!(body["on_behalf_of"], f.person.member.0.to_string());
}

#[tokio::test]
async fn a_session_token_reaches_only_the_session_routes() {
    let f = Fixture::new();
    for path in [
        "/v1/me",
        "/v1/device-only",
        "/v1/agent-files/x",
        "/v1/agent-files/x/y",
    ] {
        let (status, body) = call(f.app(), get_request(path, Some(&f.session_token))).await;
        assert_eq!(status, 403, "{path}");
        assert_eq!(body["code"], "forbidden");
    }
    let (status, body) = call(
        f.app(),
        get_request("/v1/session-answer", Some(&f.session_token)),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body, serde_json::to_value(f.session).unwrap());
    // The session routes are open to agents and people too; each checks the session itself.
    for token in [&f.agent_token, &f.device_token] {
        let (status, _) = call(f.app(), get_request("/v1/session-answer", Some(token))).await;
        assert_eq!(status, 200);
    }
}

#[tokio::test]
async fn a_revoked_token_is_unauthorized() {
    let f = Fixture::new();
    let id = f
        .tokens
        .list()
        .into_iter()
        .find(|t| t.caller == f.person)
        .unwrap()
        .id;
    f.tokens.revoke(id).unwrap();
    let (status, _) = call(f.app(), get_request("/v1/me", Some(&f.device_token))).await;
    assert_eq!(status, 401);
}

#[tokio::test]
async fn unknown_routes_and_methods_are_not_found() {
    let f = Fixture::new();
    for token in [None, Some(f.device_token.as_str())] {
        let (status, body) = call(f.app(), get_request("/v1/nope", token)).await;
        assert_eq!(status, 404);
        assert_eq!(body["code"], "not_found");
    }
    let request = axum::http::Request::builder()
        .method("DELETE")
        .uri("/v1/host/info")
        .body(axum::body::Body::empty())
        .unwrap();
    let (status, body) = call(f.app(), request).await;
    assert_eq!(status, 404);
    assert_eq!(body["code"], "not_found");
}

/// Status and raw body text.
async fn call_text(app: axum::Router, path: &str, bearer: Option<&str>) -> (u16, String) {
    use tower::ServiceExt as _;
    let response = app.oneshot(get_request(path, bearer)).await.unwrap();
    let status = response.status().as_u16();
    let body = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    (status, String::from_utf8(body.to_vec()).unwrap())
}

#[tokio::test]
async fn nested_fallbacks_are_authenticated_and_scoped() {
    let f = Fixture::new();
    let device_nested = "/v1/files/a/b";
    let agent_nested = "/v1/agent-files/a/b";

    assert_eq!(call_text(f.app(), device_nested, None).await.0, 401);
    assert_eq!(
        call_text(f.app(), device_nested, Some(&f.agent_token))
            .await
            .0,
        403
    );
    assert_eq!(
        call_text(f.app(), device_nested, Some(&f.device_token)).await,
        (200, "device fallback".to_owned())
    );

    assert_eq!(call_text(f.app(), agent_nested, None).await.0, 401);
    assert_eq!(
        call_text(f.app(), agent_nested, Some(&f.agent_token)).await,
        (200, "agent fallback".to_owned())
    );

    // Nested routes themselves, too.
    assert_eq!(call_text(f.app(), "/v1/files/x", None).await.0, 401);
    assert_eq!(
        call_text(f.app(), "/v1/files/x", Some(&f.agent_token))
            .await
            .0,
        403
    );
}

#[tokio::test]
async fn an_empty_router_parts_serves_host_info() {
    let tokens: std::sync::Arc<dyn pitcrew_auth::TokenStore> =
        std::sync::Arc::new(pitcrew_auth::FileTokenStore::in_memory());
    let app = pitcrew_api::router(
        pitcrew_api::local_host_info("0.0.0-test", vec![], vec![]),
        tokens,
        pitcrew_api::RouterParts::new().device(axum::Router::new()),
    );
    let (status, _) = call(app.clone(), get_request("/v1/host/info", None)).await;
    assert_eq!(status, 200);
    let (status, body) = call(app, get_request("/v1/nope", None)).await;
    assert_eq!(status, 404);
    assert_eq!(body["code"], "not_found");
}

#[tokio::test]
async fn two_authorization_headers_are_unauthorized() {
    let f = Fixture::new();
    let request = axum::http::Request::builder()
        .uri("/v1/me")
        .header("authorization", format!("Bearer {}", f.device_token))
        .header("authorization", format!("Bearer {}", f.device_token))
        .body(axum::body::Body::empty())
        .unwrap();
    let (status, _) = call(f.app(), request).await;
    assert_eq!(status, 401);
}

#[tokio::test]
async fn unauthorized_responses_carry_a_bearer_challenge() {
    use tower::ServiceExt as _;
    let f = Fixture::new();
    let response = f.app().oneshot(get_request("/v1/me", None)).await.unwrap();
    assert_eq!(response.status().as_u16(), 401);
    assert_eq!(response.headers()["www-authenticate"], "Bearer");
}

#[tokio::test]
async fn host_info_needs_no_token() {
    let f = Fixture::new();
    let (status, body) = call(f.app(), get_request("/v1/host/info", None)).await;
    assert_eq!(status, 200);
    assert_eq!(body["name"], "pitcrewd");
}
