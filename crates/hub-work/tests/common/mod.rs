//! Shared helpers: a seeded store, callers for the demo's members, and the routes mounted the way
//! the API layer mounts them.

#![allow(dead_code)]

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::Request;
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use pitcrew_fixtures::DemoWorkspace;
use pitcrew_hub_work::{WorkService, agent_routes, device_routes, projections};
use pitcrew_protocol::api::{Caller, TokenScope};
use pitcrew_protocol::ids::MemberId;
use pitcrew_store::{Store, StoreOptions};
use serde_json::Value;
use std::path::Path;
use std::sync::Arc;
use tower::ServiceExt as _;

pub const SAM: &str = "01JB000000000000000MEM0001";
pub const WRITER: &str = "01JB000000000000000MEM0002";
pub const RUNNER: &str = "01JB000000000000000MEM0003";
pub const REVIEWER: &str = "01JB000000000000000MEM0004";
pub const BUILDER: &str = "01JB000000000000000MEM0005";
pub const PAPER: &str = "01JB000000000000000PRJ0001";
pub const TOOLING: &str = "01JB000000000000000PRJ0002";
pub const SUBMISSION: &str = "01JB000000000000000WST0001";
pub const SEED_RUNS: &str = "01JB000000000000000WST0002";
pub const PARSERS: &str = "01JB000000000000000WST0003";

pub fn demo() -> DemoWorkspace {
    pitcrew_fixtures::demo_workspace().expect("fixture parses")
}

pub fn member(id: &str) -> MemberId {
    id.parse().expect("member id")
}

/// A person's device token.
pub fn person(id: &str) -> Caller {
    Caller {
        member: member(id),
        scope: TokenScope::Device,
        on_behalf_of: None,
    }
}

/// An agent token, acting for @sam.
pub fn agent(id: &str) -> Caller {
    Caller {
        member: member(id),
        scope: TokenScope::Agent,
        on_behalf_of: Some(member(SAM)),
    }
}

pub fn open(path: &Path) -> Arc<Store> {
    Arc::new(Store::open_with(path, StoreOptions::default(), projections()).expect("open store"))
}

/// A service over a fresh store in `dir`, seeded with the demo.
pub fn seeded(dir: &Path) -> Arc<WorkService> {
    let demo = demo();
    let work = Arc::new(WorkService::new(
        open(&dir.join("hub.db")),
        demo.workspace.id,
    ));
    work.seed(&demo).expect("seed");
    work
}

async fn require_device(request: Request, next: Next) -> Response {
    match request.extensions().get::<Caller>() {
        Some(c) if c.is_person() => next.run(request).await,
        _ => (
            StatusCode::FORBIDDEN,
            axum::Json(serde_json::json!({"code": "forbidden", "message": "device only"})),
        )
            .into_response(),
    }
}

async fn not_found() -> Response {
    (
        StatusCode::NOT_FOUND,
        axum::Json(serde_json::json!({"code": "not_found", "message": "no route"})),
    )
        .into_response()
}

/// The routes as `pitcrew-api` mounts them: agent routes as they are, device routes behind a
/// device-only guard, a JSON 404 for anything else, and the service as an extension.
pub fn app(work: &Arc<WorkService>) -> Router {
    agent_routes()
        .merge(device_routes().layer(middleware::from_fn(require_device)))
        .layer(axum::Extension(Arc::clone(work)))
        .fallback(not_found)
        .method_not_allowed_fallback(not_found)
}

/// Sends one request as `caller` and returns the status and the JSON body (`null` if empty).
pub async fn call(
    app: &Router,
    caller: Option<Caller>,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json")
        .body(match body {
            Some(json) => Body::from(json.to_string()),
            None => Body::empty(),
        })
        .expect("request");
    if let Some(caller) = caller {
        request.extensions_mut().insert(caller);
    }
    let response = app.clone().oneshot(request).await.expect("infallible");
    let status = response.status().as_u16();
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let json = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("JSON body")
    };
    (status, json)
}

pub async fn get(app: &Router, caller: Caller, path: &str) -> (u16, Value) {
    call(app, Some(caller), "GET", path, None).await
}

/// Asserts the status, and that errors carry an `ApiError` body with the matching code.
#[track_caller]
pub fn expect(result: &(u16, Value), status: u16) {
    assert_eq!(result.0, status, "body: {}", result.1);
    let code = match status {
        400 => Some("invalid"),
        401 => Some("unauthorized"),
        403 => Some("forbidden"),
        404 => Some("not_found"),
        409 => Some("conflict"),
        _ => None,
    };
    if let Some(code) = code {
        assert_eq!(result.1["code"], code, "body: {}", result.1);
        assert!(result.1["message"].is_string(), "body: {}", result.1);
    }
}
