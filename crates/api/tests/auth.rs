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

#[tokio::test]
async fn host_info_needs_no_token() {
    let f = Fixture::new();
    let (status, body) = call(f.app(), get_request("/v1/host/info", None)).await;
    assert_eq!(status, 200);
    assert_eq!(body["name"], "pitcrewd");
}
