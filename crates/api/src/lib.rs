//! # pitcrew-api
//!
//! HTTP and WebSocket API over a unix socket or named pipe; composes every crate's routes.
//!
//! - [`router`] builds the app: `GET /v1/host/info` (no auth), the caller's routes behind
//!   bearer-token authentication, and `404 not_found` for anything else.
//! - [`Bound`] listens on a [`Listen`] transport and serves the app.
//! - [`serve`] does both.
//! - [`client`] holds the checks a client makes before trusting a daemon with a token.
//! - [`stream`] serves `GET /v1/stream` from an [`EventSource`] (the store, or memory).
//! - [`terminal`] serves `GET /v1/sessions/{id}/terminal` from [`Terminals`].
//! - [`activity`] serves `GET /v1/events` from the same [`EventSource`], filtered by project or
//!   workstream through the work model's activity index ([`EventRefs`]) when it has one.
//! - [`hooks`] serves `POST /v1/hooks/{engine}/{event}` into a [`HookSink`].
//!
//! Authentication inserts an `axum::Extension<Caller>` (from `pitcrew-protocol`) into every
//! authenticated request. Domain crates read it with `pitcrew_auth::Authenticated` or
//! `pitcrew_auth::Person`, and never see tokens.
//!
//! **Owned by stream H.** The work packages are in `docs/build/streams/H.md`. Build against
//! `pitcrew-protocol` and `pitcrew-interfaces` only, never another stream's internals.

pub mod activity;
mod auth;
pub mod client;
pub mod hooks;
mod host;
mod listener;
pub mod source;
pub mod stream;
pub mod terminal;
mod util;

pub use activity::{Activity, EventRefs, RefFilter};
pub use hooks::{HookEvent, HookIntake, HookSink, LogHookSink};
pub use host::{local_host_info, local_machine_info};
pub use listener::{Bound, Listen};
#[cfg(windows)]
pub use listener::{NamedPipe, PipeAddr, default_pipe_name};
#[cfg(unix)]
pub use listener::{SOCKET_NAME, UnixSocket};
#[cfg(feature = "store")]
pub use source::StoreSource;
pub use source::{EventSource, MemorySource};
pub use stream::StreamConfig;
pub use terminal::{Attachment, RuntimeTerminals, TerminalConfig, TerminalError, Terminals};

use axum::Json;
use axum::Router;
use axum::extract::Request;
use axum::middleware;
use axum::routing::get;
use pitcrew_auth::{ErrorResponse, TokenStore};
use pitcrew_protocol::api::HostInfo;
use std::future::Future;
use std::io;
use std::sync::Arc;

/// The protocol version this crate was built against.
pub const PROTOCOL_VERSION: u32 = pitcrew_protocol::PROTOCOL_VERSION;

/// The routes to serve behind authentication, split by who may call them.
///
/// - `agent` routes accept both scopes (the routes marked **agent** in `api-v1.md`).
/// - `device` routes accept only device tokens; agents get `403 forbidden`.
///
/// Routes are device-only unless added with [`RouterParts::agent`], so forgetting to mark a
/// route fails closed.
#[derive(Debug, Default)]
pub struct RouterParts {
    agent: Router,
    device: Router,
}

impl RouterParts {
    /// No routes yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds routes that agents may call too.
    #[must_use]
    pub fn agent(mut self, routes: Router) -> Self {
        self.agent = self.agent.merge(routes);
        self
    }

    /// Adds routes that only a person's device may call.
    #[must_use]
    pub fn device(mut self, routes: Router) -> Self {
        self.device = self.device.merge(routes);
        self
    }
}

/// Builds the app: host info, then `parts` behind authentication, then a JSON 404.
///
/// Authentication wraps every part with `layer`, not `route_layer`, so it also covers fallbacks
/// of nested routers (a `nest(..)` with its own `fallback_service`, e.g. a file server).
///
/// **Do not merge more routes into the returned router**: they would be unauthenticated. Add
/// them to `parts` instead.
pub fn router(host_info: HostInfo, tokens: Arc<dyn TokenStore>, parts: RouterParts) -> Router {
    let host_info = Arc::new(host_info);
    let authenticated = parts
        .agent
        .merge(pitcrew_auth::device_only(parts.device))
        .layer(middleware::from_fn_with_state(tokens, auth::authenticate));
    Router::new()
        .route(
            "/v1/host/info",
            get(move || {
                let info = Arc::clone(&host_info);
                async move { Json((*info).clone()) }
            }),
        )
        .merge(authenticated)
        .fallback(not_found)
        .method_not_allowed_fallback(not_found)
}

async fn not_found(request: Request) -> ErrorResponse {
    ErrorResponse::not_found(format!(
        "No route for {} {}.",
        request.method(),
        request.uri().path()
    ))
}

/// Binds `listen` and serves [`router`] until `shutdown` completes.
///
/// # Errors
/// Binding fails (see [`Bound::bind`]), or serving does.
pub async fn serve<F>(
    listen: &Listen,
    host_info: HostInfo,
    tokens: Arc<dyn TokenStore>,
    parts: RouterParts,
    shutdown: F,
) -> io::Result<()>
where
    F: Future<Output = ()> + Send + 'static,
{
    let bound = Bound::bind(listen).await?;
    tracing::info!(at = %bound.describe(), "API listening");
    bound
        .serve(router(host_info, tokens, parts), shutdown)
        .await
}
