//! The gateway (`docs/build/contracts/desktop-gateway.md`): the webview's only way to a
//! workspace's daemon. It checks each call, adds the workspace's token, and forwards the call
//! over the daemon's socket or pipe. The webview sets no headers and never sees a token.
//!
//! This module is the gateway itself, independent of Tauri; `commands.rs` exposes it as the five
//! Tauri commands.
//!
//! It logs workspace ids, paths without their query, statuses and timings; never tokens, bodies
//! or frames.

pub mod error;
pub(crate) mod http;
pub mod path;
pub mod sink;
pub(crate) mod socket;

pub use error::{ErrorCode, GatewayError};
pub use path::SocketKind;
pub use sink::{Delivery, Sink, SinkClosed};

use crate::daemon::endpoint::BoxIo;
use crate::registry::{GatewayWorkspace, Registry};
use crate::token::DeviceToken;
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use socket::{Control, codes};
use std::collections::HashMap;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

/// A boxed, sendable future.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// An open, checked connection to a workspace's daemon, and the token to send on it.
pub struct Connected {
    /// The stream.
    pub io: BoxIo,
    /// The workspace's device token, read for this connection.
    pub token: DeviceToken,
}

impl fmt::Debug for Connected {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Connected").finish_non_exhaustive()
    }
}

/// How the gateway reaches one workspace's daemon: the local socket or pipe now, an SSH tunnel to
/// a remote one later.
pub trait Connector: Send + Sync + 'static {
    /// Opens a checked connection and reads the token for it.
    ///
    /// # Errors
    /// `unreachable` when the daemon cannot be reached or trusted; `needs_pairing` when there is
    /// no token yet.
    fn connect(&self) -> BoxFuture<'_, Result<Connected, GatewayError>>;
}

/// The gateway's limits. The defaults are the contract's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// A request body: 1 MiB.
    pub request_body: usize,
    /// A response body: 32 MiB.
    pub response_body: usize,
    /// Incoming frames the webview has not taken yet, per socket: 8 MiB. Also the largest frame
    /// the daemon may send.
    pub backlog: usize,
    /// How long a request may take, connecting included.
    pub request_timeout: Duration,
    /// How long opening a socket may take: connecting and the upgrade.
    pub open_timeout: Duration,
    /// How long a closing socket waits for the daemon's end of the close handshake.
    pub close_grace: Duration,
    /// How long sending one frame to the daemon may take before the socket is given up.
    pub send_timeout: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            request_body: 1024 * 1024,
            response_body: 32 * 1024 * 1024,
            backlog: 8 * 1024 * 1024,
            request_timeout: Duration::from_secs(120),
            open_timeout: Duration::from_secs(20),
            close_grace: Duration::from_secs(2),
            send_timeout: Duration::from_secs(10),
        }
    }
}

/// `gateway_request`'s argument.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct GatewayRequest {
    /// The workspace's id.
    pub workspace: String,
    /// `GET`, `POST`, `PATCH`, `PUT` or `DELETE`.
    pub method: String,
    /// `/v1/…`, with an optional `?query`.
    pub path: String,
    /// JSON text.
    #[serde(default)]
    pub body: Option<String>,
}

/// What `gateway_request` resolves with: the daemon's answer, whatever its status.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayResponse {
    /// The HTTP status.
    pub status: u16,
    /// The `Content-Type`, the only daemon header that reaches the webview.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    /// The body as text.
    pub body: String,
}

/// What a socket carries from the webview.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Payload {
    /// A text frame.
    Text(String),
    /// A binary frame.
    Binary(Vec<u8>),
}

impl Payload {
    fn len(&self) -> usize {
        match self {
            Self::Text(t) => t.len(),
            Self::Binary(b) => b.len(),
        }
    }
}

/// The open sockets, and each page's generation (it changes when the page reloads).
#[derive(Debug, Default)]
struct Sockets {
    next: u32,
    serial: u64,
    open: HashMap<u32, Handle>,
    generations: HashMap<String, u64>,
}

#[derive(Debug)]
struct Handle {
    owner: String,
    kind: SocketKind,
    /// Tells this socket apart from a later one with the same number.
    serial: u64,
    messages: mpsc::Sender<Message>,
    control: mpsc::UnboundedSender<Control>,
}

/// The gateway.
pub struct Gateway {
    registry: Arc<Registry>,
    sockets: Arc<Mutex<Sockets>>,
    limits: Limits,
}

impl fmt::Debug for Gateway {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Gateway")
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}

impl Gateway {
    /// A gateway to the workspaces in `registry`, with the contract's limits.
    #[must_use]
    pub fn new(registry: Arc<Registry>) -> Self {
        Self::with_limits(registry, Limits::default())
    }

    /// A gateway with other limits (tests).
    #[must_use]
    pub fn with_limits(registry: Arc<Registry>, limits: Limits) -> Self {
        Self {
            registry,
            sockets: Arc::default(),
            limits,
        }
    }

    /// The registry.
    #[must_use]
    pub fn registry(&self) -> &Arc<Registry> {
        &self.registry
    }

    /// `gateway_workspaces`.
    #[must_use]
    pub fn workspaces(&self) -> Vec<GatewayWorkspace> {
        self.registry.list()
    }

    /// `gateway_request`: checks the call, forwards it with the workspace's token, and returns
    /// the daemon's answer whatever its status.
    ///
    /// # Errors
    /// A `GatewayError` when the call is malformed or the daemon never answered.
    pub async fn request(&self, req: GatewayRequest) -> Result<GatewayResponse, GatewayError> {
        let method = parse_method(&req.method)?;
        let path = path::check_request_path(&req.path)?;
        if path::is_credential_route(path) {
            return Err(GatewayError::invalid(
                "an integration's credential goes through gateway_integration_credential, not \
                 gateway_request",
            ));
        }
        let body = match req.body {
            Some(body) if body.len() > self.limits.request_body => {
                return Err(GatewayError::too_large(format!(
                    "the body is over {} bytes",
                    self.limits.request_body
                )));
            }
            other => other.map(Bytes::from),
        };
        self.forward(&req.workspace, method, path, body).await
    }

    /// `gateway_integration_credential`: hands `secret` to the workspace's daemon as
    /// `PUT /v1/integrations/{integration}/credential` (api-v1.md, "Integrations"), with the
    /// workspace's token, and returns the daemon's answer whatever its status. The secret is in
    /// the request body only: never logged, never in an error, and no daemon answer holds it.
    ///
    /// # Errors
    /// `invalid` for a malformed integration id; a `GatewayError` when the daemon never answered.
    pub async fn store_credential(
        &self,
        workspace: &str,
        integration: &str,
        secret: &pitcrew_remote::Secret,
    ) -> Result<GatewayResponse, GatewayError> {
        if !path::is_integration_id(integration) {
            return Err(GatewayError::invalid(
                "integration must be an integration id",
            ));
        }
        let body = serde_json::to_vec(&serde_json::json!({ "secret": secret.expose() }))
            .map_err(|_| GatewayError::internal("the credential could not be encoded"))?;
        if body.len() > self.limits.request_body {
            return Err(GatewayError::too_large(format!(
                "the credential is over {} bytes",
                self.limits.request_body
            )));
        }
        let path = format!("/v1/integrations/{integration}/credential");
        self.forward(
            workspace,
            ::http::Method::PUT,
            &path,
            Some(Bytes::from(body)),
        )
        .await
    }

    /// Sends one checked request to `workspace`'s daemon with its token, and returns the answer
    /// whatever its status. Logs the workspace (shortened), the method, the route, the status and
    /// the time; never a body.
    async fn forward(
        &self,
        workspace_id: &str,
        method: ::http::Method,
        path: &str,
        body: Option<Bytes>,
    ) -> Result<GatewayResponse, GatewayError> {
        let started = Instant::now();
        // Webview-supplied: logged only through `shorten`.
        let workspace = error::shorten(workspace_id);
        let connector = self.registry.connector(workspace_id)?;
        let timeout = self.limits.request_timeout;
        let connected = tokio::time::timeout(timeout, connector.connect())
            .await
            .unwrap_or_else(|_| Err(not_in_time("connect to the daemon", timeout)))
            .inspect_err(|e| {
                tracing::info!(%workspace, route = path::route_of(path), error = %e, "request not sent");
            })?;
        let reply = http::send(
            connected.io,
            Some(&connected.token),
            method.clone(),
            path,
            body,
            self.limits.response_body,
            timeout.saturating_sub(started.elapsed()),
        )
        .await;
        drop(connected.token);
        let reply = match reply {
            Ok(reply) => reply,
            Err(e) => {
                tracing::info!(%workspace, %method, route = path::route_of(path), error = %e, "request failed");
                return Err(match e {
                    http::HttpError::TooLarge(_) => GatewayError::too_large(e.to_string()),
                    http::HttpError::Bad(_) => GatewayError::internal(e.to_string()),
                    http::HttpError::Broken(_) | http::HttpError::Timeout(_) => {
                        GatewayError::unreachable(e.to_string())
                    }
                });
            }
        };
        tracing::debug!(
            %workspace,
            %method,
            route = path::route_of(path),
            status = reply.status,
            ms = started.elapsed().as_millis(),
            "request"
        );
        let body = String::from_utf8(reply.body.to_vec())
            .map_err(|_| GatewayError::internal("the daemon's answer is not UTF-8 text"))?;
        Ok(GatewayResponse {
            status: reply.status,
            content_type: reply.content_type,
            body,
        })
    }

    /// `gateway_socket_open` for the page `owner` (its webview's label): opens the WebSocket at
    /// `path`, and returns the socket's number once the upgrade has succeeded. Its messages go to
    /// `sink`.
    ///
    /// # Errors
    /// `invalid` for a path that is not a socket route; `unknown_workspace`; `unreachable` when
    /// the daemon is down or answers 503; others as the upgrade failed.
    pub async fn socket_open(
        &self,
        owner: &str,
        workspace: &str,
        path: &str,
        sink: Arc<dyn Sink>,
    ) -> Result<u32, GatewayError> {
        let started = Instant::now();
        let kind = path::check_socket_path(path)?;
        let generation = self.lock().generations.get(owner).copied().unwrap_or(0);
        let connector = self.registry.connector(workspace)?;
        // Webview-supplied: logged only through `shorten`.
        let workspace = error::shorten(workspace);
        let limits = self.limits;
        let opening = async {
            let connected = connector.connect().await?;
            let opened = socket::open(connected.io, &connected.token, path, limits.backlog).await;
            drop(connected.token);
            opened
        };
        let ws = tokio::time::timeout(limits.open_timeout, opening)
            .await
            .unwrap_or_else(|_| Err(not_in_time("open the socket", limits.open_timeout)))
            .inspect_err(|e| {
                tracing::info!(%workspace, route = path::route_of(path), error = %e, "socket not opened");
            })?;

        let (messages, message_rx) = mpsc::channel(64);
        let (control, control_rx) = mpsc::unbounded_channel();
        let registered = {
            let mut sockets = self.lock();
            if sockets.generations.get(owner).copied().unwrap_or(0) == generation {
                let id = next_id(&mut sockets);
                sockets.serial += 1;
                let serial = sockets.serial;
                sockets.open.insert(
                    id,
                    Handle {
                        owner: owner.to_owned(),
                        kind,
                        serial,
                        messages,
                        control: control.clone(),
                    },
                );
                Some((id, serial))
            } else {
                None
            }
        };
        let Some((id, serial)) = registered else {
            // The page reloaded while this socket opened: nobody is listening any more.
            let _ = control.send(Control::Abandon);
            tokio::spawn(socket::pump(ws, message_rx, control_rx, sink, limits));
            return Err(GatewayError::invalid(
                "the page reloaded while the socket opened",
            ));
        };
        drop(control);
        tracing::debug!(%workspace, socket = id, kind = kind.name(), route = path::route_of(path), ms = started.elapsed().as_millis(), "socket open");

        let sockets = Arc::clone(&self.sockets);
        tokio::spawn(async move {
            let ended = socket::pump(ws, message_rx, control_rx, sink, limits).await;
            if let Ok(mut sockets) = sockets.lock()
                && sockets.open.get(&id).is_some_and(|h| h.serial == serial)
            {
                sockets.open.remove(&id);
            }
            tracing::debug!(%workspace, socket = id, code = ended.code, reported = ended.reported, ms = started.elapsed().as_millis(), "socket closed");
        });
        Ok(id)
    }

    /// `gateway_socket_send`. Over the socket's limit (1 MiB on terminals, 4 KiB on the stream)
    /// the socket closes with 1009, as the daemon would, and the call fails with `too_large`.
    ///
    /// # Errors
    /// `invalid` on a closed or unknown socket, or another page's; `too_large` over the limit.
    pub async fn socket_send(
        &self,
        owner: &str,
        socket: u32,
        payload: Payload,
    ) -> Result<(), GatewayError> {
        let messages = {
            let mut sockets = self.lock();
            let Some(handle) = sockets.open.get(&socket).filter(|h| h.owner == owner) else {
                return Err(closed(socket));
            };
            let limit = handle.kind.send_limit();
            if payload.len() > limit {
                if let Some(handle) = sockets.open.remove(&socket) {
                    let _ = handle
                        .control
                        .send(Control::Close(codes::TOO_BIG, "message too big".into()));
                }
                return Err(GatewayError::too_large(format!(
                    "a message on this socket is at most {limit} bytes; the socket is closed"
                )));
            }
            handle.messages.clone()
        };
        let message = match payload {
            Payload::Text(text) => Message::Text(text.into()),
            Payload::Binary(data) => Message::Binary(data.into()),
        };
        messages.send(message).await.map_err(|_| closed(socket))
    }

    /// `gateway_socket_close`: closes the socket with `code` (1000 by default). Idempotent.
    ///
    /// # Errors
    /// `invalid` for a code a client may not send (only 1000 and 3000 to 4999), or a reason over
    /// 123 bytes.
    pub fn socket_close(
        &self,
        owner: &str,
        socket: u32,
        code: Option<u16>,
        reason: Option<String>,
    ) -> Result<(), GatewayError> {
        let code = code.unwrap_or(codes::NORMAL);
        if code != codes::NORMAL && !(3000..=4999).contains(&code) {
            return Err(GatewayError::invalid(
                "the close code must be 1000 or 3000 to 4999",
            ));
        }
        let reason = reason.unwrap_or_default();
        if reason.len() > 123 {
            return Err(GatewayError::invalid(
                "the close reason must be at most 123 bytes",
            ));
        }
        let mut sockets = self.lock();
        let is_ours = sockets.open.get(&socket).is_some_and(|h| h.owner == owner);
        if is_ours && let Some(handle) = sockets.open.remove(&socket) {
            let _ = handle.control.send(Control::Close(code, reason));
        }
        Ok(())
    }

    /// The page in `owner` started loading (a reload, or its first load): its sockets close.
    pub fn page_started(&self, owner: &str) {
        let mut sockets = self.lock();
        *sockets.generations.entry(owner.to_owned()).or_default() += 1;
        drop(sockets);
        self.forget(owner);
    }

    /// The window or webview `owner` closed: every socket it opened closes.
    pub fn forget(&self, owner: &str) {
        let handles: Vec<(u32, Handle)> = {
            let mut sockets = self.lock();
            let ids: Vec<u32> = sockets
                .open
                .iter()
                .filter(|(_, h)| h.owner == owner)
                .map(|(id, _)| *id)
                .collect();
            ids.into_iter()
                .filter_map(|id| sockets.open.remove(&id).map(|h| (id, h)))
                .collect()
        };
        if !handles.is_empty() {
            tracing::debug!(
                owner,
                sockets = handles.len(),
                "closing the sockets of a page that went away"
            );
        }
        for (_, handle) in handles {
            let _ = handle.control.send(Control::Abandon);
        }
    }

    /// How many sockets are open (for tests and diagnostics).
    #[must_use]
    pub fn open_sockets(&self) -> usize {
        self.lock().open.len()
    }

    fn lock(&self) -> MutexGuard<'_, Sockets> {
        self.sockets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl Drop for Gateway {
    fn drop(&mut self) {
        // The pumps see their command channel close and shut their sockets with 1001.
        self.lock().open.clear();
    }
}

fn parse_method(method: &str) -> Result<::http::Method, GatewayError> {
    match method {
        "GET" => Ok(::http::Method::GET),
        "POST" => Ok(::http::Method::POST),
        "PATCH" => Ok(::http::Method::PATCH),
        "PUT" => Ok(::http::Method::PUT),
        "DELETE" => Ok(::http::Method::DELETE),
        _ => Err(GatewayError::invalid(
            "the method must be GET, POST, PATCH, PUT or DELETE",
        )),
    }
}

/// `unreachable`: the daemon did not do `what` within `limit`.
fn not_in_time(what: &str, limit: Duration) -> GatewayError {
    GatewayError::unreachable(format!("could not {what} within {} s", limit.as_secs_f32()))
}

fn closed(socket: u32) -> GatewayError {
    GatewayError::invalid(format!("socket {socket} is closed"))
}

/// The next free socket number: from 1, never 0, skipping numbers in use.
fn next_id(sockets: &mut Sockets) -> u32 {
    loop {
        sockets.next = sockets.next.wrapping_add(1);
        if sockets.next != 0 && !sockets.open.contains_key(&sockets.next) {
            return sockets.next;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn methods() {
        for m in ["GET", "POST", "PATCH", "PUT", "DELETE"] {
            assert_eq!(parse_method(m).unwrap().as_str(), m);
        }
        for m in ["get", "HEAD", "OPTIONS", "CONNECT", "TRACE", ""] {
            assert_eq!(parse_method(m).unwrap_err().code, ErrorCode::Invalid, "{m}");
        }
    }

    #[test]
    fn socket_numbers_start_at_one_and_skip_those_in_use() {
        let mut s = Sockets::default();
        assert_eq!(next_id(&mut s), 1);
        s.next = u32::MAX - 1;
        assert_eq!(next_id(&mut s), u32::MAX);
        let (messages, _rx) = mpsc::channel(1);
        let (control, _control_rx) = mpsc::unbounded_channel();
        s.open.insert(
            1,
            Handle {
                owner: "main".into(),
                kind: SocketKind::Stream,
                serial: 1,
                messages,
                control,
            },
        );
        assert_eq!(next_id(&mut s), 2, "0 and 1 are skipped");
    }

    #[test]
    fn responses_carry_only_status_content_type_and_body() {
        let r = GatewayResponse {
            status: 404,
            content_type: Some("application/json".into()),
            body: "{}".into(),
        };
        assert_eq!(
            serde_json::to_value(&r).unwrap(),
            serde_json::json!({ "status": 404, "contentType": "application/json", "body": "{}" })
        );
        let r = GatewayResponse {
            status: 204,
            content_type: None,
            body: String::new(),
        };
        assert_eq!(
            serde_json::to_value(&r).unwrap(),
            serde_json::json!({ "status": 204, "body": "" })
        );
    }
}
