//! The person's local `pitcrewd`: finding it ([`locate`]), connecting to it ([`endpoint`]),
//! starting and supervising it ([`supervisor`]), and keeping the registry's local workspace in
//! step with it ([`follow`]).

pub mod endpoint;
pub mod locate;
pub mod supervisor;

use crate::gateway::http;
use crate::gateway::{BoxFuture, Connected, Connector, GatewayError};
use crate::registry::{Registry, WorkspaceState};
use crate::token::read_token_file;
use endpoint::Endpoint;
use std::fmt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use supervisor::DaemonState;
use tokio::sync::{Notify, watch};

/// The local daemon as the gateway reaches it: its private socket or pipe, and its token file.
pub struct LocalConnector {
    endpoint: Endpoint,
    token_path: Mutex<Option<PathBuf>>,
    on_failure: Mutex<Option<Arc<Notify>>>,
}

impl fmt::Debug for LocalConnector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LocalConnector")
            .field("endpoint", &self.endpoint)
            .finish_non_exhaustive()
    }
}

impl LocalConnector {
    /// The daemon at `endpoint`, whose token file is not known yet.
    #[must_use]
    pub fn new(endpoint: Endpoint) -> Self {
        Self {
            endpoint,
            token_path: Mutex::new(None),
            on_failure: Mutex::new(None),
        }
    }

    /// The daemon at `endpoint`, with its token in `token_path`.
    #[must_use]
    pub fn with_token_path(endpoint: Endpoint, token_path: PathBuf) -> Self {
        let connector = Self::new(endpoint);
        connector.set_token_path(Some(token_path));
        connector
    }

    /// Where the token is, as `pitcrewd token show-path` said.
    pub fn set_token_path(&self, path: Option<PathBuf>) {
        *self
            .token_path
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = path;
    }

    /// Notified whenever a connection fails, so the supervisor looks again at once.
    pub fn notify_failures(&self, notify: Arc<Notify>) {
        *self
            .on_failure
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(notify);
    }

    fn token_path(&self) -> Option<PathBuf> {
        self.token_path
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn failed(&self) {
        if let Some(notify) = self
            .on_failure
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
        {
            notify.notify_one();
        }
    }
}

impl Connector for LocalConnector {
    fn connect(&self) -> BoxFuture<'_, Result<Connected, GatewayError>> {
        Box::pin(async move {
            let token_path = self
                .token_path()
                .ok_or_else(|| GatewayError::unreachable("the local daemon is not ready yet"))?;
            let io = self.endpoint.connect().await.map_err(|e| {
                self.failed();
                GatewayError::unreachable(e.to_string())
            })?;
            // Only once the connection has passed its checks, and afresh every time.
            let token = read_token_file(&token_path).map_err(|e| {
                tracing::warn!(path = %token_path.display(), error = %e, "cannot read the device token");
                GatewayError::unreachable("cannot read the local daemon's device token")
            })?;
            Ok(Connected { io, token })
        })
    }
}

/// The body of `GET /v1/workspace`.
#[derive(serde::Deserialize)]
struct WorkspaceAt {
    workspace: pitcrew_protocol::model::Workspace,
}

/// Asks the daemon behind `connector` which workspace it hosts: `(id, name)`.
///
/// # Errors
/// The daemon cannot be reached, or does not answer with a workspace.
pub async fn hosted_workspace(connector: &dyn Connector) -> Result<(String, String), GatewayError> {
    let connected = connector.connect().await?;
    let reply = http::send(
        connected.io,
        Some(&connected.token),
        ::http::Method::GET,
        "/v1/workspace",
        None,
        1024 * 1024,
        Duration::from_secs(20),
    )
    .await
    .map_err(|e| GatewayError::unreachable(e.to_string()))?;
    if reply.status != 200 {
        return Err(GatewayError::unreachable(format!(
            "GET /v1/workspace answered HTTP {}",
            reply.status
        )));
    }
    let at: WorkspaceAt = serde_json::from_slice(&reply.body).map_err(|e| {
        GatewayError::internal(format!("GET /v1/workspace is not a workspace: {e}"))
    })?;
    // The bare ULID, as in the UI's `/w/$ws` routes (the id's `Display` adds `wsp_`).
    Ok((at.workspace.id.0.to_string(), at.workspace.name))
}

/// Keeps the registry's local workspace in step with the supervisor: `connecting` while it looks
/// for or starts the daemon; once the daemon is ready, its workspace is registered (first start)
/// or refreshed, and `ready`; `unreachable` with the reason when it gave up. Runs until the
/// supervisor stops.
pub async fn follow(
    mut state: watch::Receiver<DaemonState>,
    connector: Arc<LocalConnector>,
    registry: Arc<Registry>,
) {
    loop {
        let current = state.borrow_and_update().clone();
        match current {
            DaemonState::Connecting => {
                registry.set_local_state(WorkspaceState::Connecting, None);
            }
            DaemonState::Ready { token, .. } => {
                connector.set_token_path(Some(token));
                match hosted_workspace(&*connector).await {
                    Ok((id, name)) => {
                        if let Err(e) = registry.set_local(&id, &name) {
                            tracing::warn!(error = %e, "the local workspace is registered but not saved");
                        }
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "the local daemon is up but did not say which workspace it hosts");
                        registry.set_local_state(WorkspaceState::Unreachable, Some(e.message));
                    }
                }
            }
            DaemonState::Unreachable { detail } => {
                registry.set_local_state(WorkspaceState::Unreachable, Some(detail));
            }
        }
        if state.changed().await.is_err() {
            return;
        }
    }
}
