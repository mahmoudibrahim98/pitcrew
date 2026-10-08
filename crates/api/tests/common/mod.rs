//! A small app for the API tests: agent routes (a read and a write), a read route, a session
//! route, a device route, and a device WebSocket.

#![allow(dead_code)]

use axum::Json;
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::ws::{Message, WebSocketUpgrade};
use axum::http::Request;
use axum::response::Response;
use axum::routing::{get, post};
use pitcrew_api::{RouterParts, local_host_info};
use pitcrew_auth::{Authenticated, FileTokenStore, TokenStore, WS_PROTOCOL};
use pitcrew_protocol::MemberId;
use pitcrew_protocol::api::{Caller, HostRole, TokenScope};
use std::sync::Arc;
use tower::ServiceExt as _;

pub struct Fixture {
    pub tokens: Arc<FileTokenStore>,
    pub person: Caller,
    pub agent: Caller,
    /// The agent, as a reader: a token that may only read.
    pub reader: Caller,
    pub session: Caller,
    pub device_token: String,
    pub agent_token: String,
    pub reader_token: String,
    pub session_token: String,
}

impl Fixture {
    pub fn new() -> Self {
        let tokens = Arc::new(FileTokenStore::in_memory());
        let person = Caller {
            member: MemberId::new(),
            scope: TokenScope::Device,
            on_behalf_of: None,
        };
        let agent = Caller {
            member: MemberId::new(),
            scope: TokenScope::Agent,
            on_behalf_of: Some(person.member),
        };
        let reader = Caller {
            scope: TokenScope::Reader,
            ..agent
        };
        let session = Caller {
            scope: TokenScope::Session(pitcrew_protocol::SessionId::new()),
            ..agent
        };
        let (_, device_token) = tokens.mint(person).unwrap();
        let (_, agent_token) = tokens.mint(agent).unwrap();
        let (_, reader_token) = tokens.mint(reader).unwrap();
        let (_, session_token) = tokens.mint(session).unwrap();
        Self {
            tokens,
            person,
            agent,
            reader,
            session,
            device_token: device_token.into_string(),
            agent_token: agent_token.into_string(),
            reader_token: reader_token.into_string(),
            session_token: session_token.into_string(),
        }
    }

    pub fn app(&self) -> Router {
        let whoami = get(|Authenticated(caller): Authenticated| async move { Json(caller) });
        let write = post(|Authenticated(caller): Authenticated| async move { Json(caller) });
        // Nested routers with their own fallbacks, like a file server would have.
        let nested = |name: &'static str| {
            Router::new()
                .route("/{id}", get(|| async { "route" }))
                .fallback(move || async move { name })
        };
        let parts = RouterParts::new()
            .agent(
                Router::new()
                    .route("/v1/me", whoami.clone())
                    .route("/v1/agent-write", write.clone())
                    .nest("/v1/agent-files", nested("agent fallback")),
            )
            .read(Router::new().route("/v1/read", whoami.clone()))
            .session(Router::new().route("/v1/session-answer", whoami.clone()))
            .device(
                Router::new()
                    .route("/v1/device-only", whoami.clone())
                    .route("/v1/device-write", write)
                    .route("/v1/ws", get(ws_whoami))
                    .nest("/v1/files", nested("device fallback")),
            );
        let store: Arc<dyn TokenStore> = self.tokens.clone();
        pitcrew_api::router(
            local_host_info("0.0.0-test", vec![HostRole::Hub, HostRole::Runner], vec![]),
            store,
            parts,
        )
    }
}

/// Answers with `pitcrew.v1` and sends the caller as one text message.
async fn ws_whoami(Authenticated(caller): Authenticated, ws: WebSocketUpgrade) -> Response {
    ws.protocols([WS_PROTOCOL])
        .on_upgrade(move |mut socket| async move {
            let text = serde_json::to_string(&caller).unwrap();
            let _ = socket.send(Message::Text(text.into())).await;
            let _ = socket.send(Message::Close(None)).await;
        })
}

/// Sends `request` through `app` and returns the status and the JSON body (`null` if none).
pub async fn call(app: Router, request: Request<Body>) -> (u16, serde_json::Value) {
    let response = app.oneshot(request).await.unwrap();
    let status = response.status().as_u16();
    let body = to_bytes(response.into_body(), 1 << 20).await.unwrap();
    (status, serde_json::from_slice(&body).unwrap_or_default())
}

/// Any event, for filling a log.
pub fn event() -> pitcrew_protocol::events::Event {
    pitcrew_protocol::events::Event::now(
        pitcrew_protocol::WorkspaceId::new(),
        MemberId::new(),
        pitcrew_protocol::events::EventBody::MachineLiveness {
            machine: pitcrew_protocol::MachineId::new(),
            liveness: pitcrew_protocol::model::Liveness::Live,
        },
    )
}

pub fn get_request(path: &str, bearer: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder().uri(path);
    if let Some(token) = bearer {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    builder.body(Body::empty()).unwrap()
}
