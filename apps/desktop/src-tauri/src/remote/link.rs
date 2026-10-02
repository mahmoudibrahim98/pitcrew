//! A remote workspace's way to its helper, once added: the tunnel's `Connector`
//! (`pitcrew-remote`), the gateway's [`Connector`] over it with the token from the keychain, and
//! the task that keeps the workspace's state in step with the tunnel's.
//!
//! **States** follow the tunnel ([`state_of`]): `ready` while connected (`needs_pairing` instead
//! when its token is no longer in the keychain); `connecting` while it connects, or while the way
//! does not answer and it is checking (with the reason in `detail`); `unreachable` with the
//! reason once it gave up for now (it goes on trying as the tunnel says, or at
//! `gateway_workspace_retry`). A sign-in that failed because this computer's ssh is too old for
//! prompts says so ([`super::gate`]). The transport the tunnel found worth remembering is saved
//! with the workspace.
//!
//! **Names.** Each time the tunnel connects, the hub is asked its workspace's name again
//! (`GET /v1/workspace`), cleaned and cut as at pairing, so a name set on the hub (by first-run
//! setup, say) shows in the app.

use super::gate::SshCheck;
use crate::gateway::{BoxFuture, Connected, Connector, GatewayError};
use crate::keychain::TokenStore;
use crate::registry::{Registry, WorkspaceState};
use crate::token::DeviceToken;
use pitcrew_remote::{LinkState, TunnelError, Unreachable};
use std::fmt;
use std::sync::{Arc, Mutex};

/// The tunnel to one machine's helper (`pitcrew_remote::Connector`).
pub type Tunnel = pitcrew_remote::Connector;

/// The gateway's way to a remote workspace: a connection through the tunnel, and its token from
/// the keychain, read for each connection.
pub struct RemoteConnector {
    workspace: String,
    tunnel: Tunnel,
    tokens: Arc<dyn TokenStore>,
}

impl fmt::Debug for RemoteConnector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RemoteConnector")
            .field("workspace", &self.workspace)
            .field("host", &self.tunnel.host())
            .finish_non_exhaustive()
    }
}

impl RemoteConnector {
    /// Workspace `workspace`'s daemon through `tunnel`, with its token in `tokens`.
    #[must_use]
    pub fn new(workspace: String, tunnel: Tunnel, tokens: Arc<dyn TokenStore>) -> Self {
        Self {
            workspace,
            tunnel,
            tokens,
        }
    }
}

impl Connector for RemoteConnector {
    fn connect(&self) -> BoxFuture<'_, Result<Connected, GatewayError>> {
        Box::pin(async move {
            let stream = self
                .tunnel
                .connect()
                .await
                .map_err(|e| tunnel_error(self.tunnel.host(), &e))?;
            let token = match self.tokens.get(&self.workspace) {
                Ok(Some(token)) => token,
                Ok(None) => {
                    return Err(GatewayError::needs_pairing(
                        "this computer has no token for this workspace any more; remove it \
                         and add the machine again",
                    ));
                }
                Err(e) => {
                    return Err(GatewayError::unreachable(format!(
                        "cannot read this workspace's token: {e}"
                    )));
                }
            };
            Ok(Connected {
                io: Box::new(stream),
                token,
            })
        })
    }
}

/// A connector with the token in hand, before it is kept: pairing asks the helper which
/// workspace it hosts with it.
pub(crate) struct Pairing {
    pub(crate) tunnel: Tunnel,
    pub(crate) token: DeviceToken,
}

impl Connector for Pairing {
    fn connect(&self) -> BoxFuture<'_, Result<Connected, GatewayError>> {
        Box::pin(async move {
            let stream = self
                .tunnel
                .connect()
                .await
                .map_err(|e| tunnel_error(self.tunnel.host(), &e))?;
            Ok(Connected {
                io: Box::new(stream),
                token: self.token.clone(),
            })
        })
    }
}

/// A tunnel failure as a gateway error: `unreachable`, with the tunnel's reason (which carries
/// no path and no secret).
pub(crate) fn tunnel_error(host: &str, error: &TunnelError) -> GatewayError {
    let message = super::tidy(&format!("{host}: {error}"));
    match error {
        TunnelError::Unsupported(_) => GatewayError::internal(message),
        _ => GatewayError::unreachable(message),
    }
}

/// A workspace's state, and its detail, for a tunnel's state.
#[must_use]
pub fn state_of(link: &LinkState) -> (WorkspaceState, Option<String>) {
    match link {
        LinkState::Connected { .. } => (WorkspaceState::Ready, None),
        LinkState::Unverifiable { reason } => {
            (WorkspaceState::Connecting, Some(super::tidy(reason)))
        }
        LinkState::Unreachable { reason, .. } => {
            (WorkspaceState::Unreachable, Some(super::tidy(reason)))
        }
        LinkState::Closed => (
            WorkspaceState::Unreachable,
            Some("the connection was closed".to_owned()),
        ),
        _ => (WorkspaceState::Connecting, None),
    }
}

/// The state of a connected remote workspace, by whether its token is in the keychain.
fn token_state(workspace: &str, tokens: &dyn TokenStore) -> (WorkspaceState, Option<String>) {
    match tokens.get(workspace) {
        Ok(Some(_)) => (WorkspaceState::Ready, None),
        Ok(None) => (
            WorkspaceState::NeedsPairing,
            Some(
                "this computer has no token for this workspace any more; remove it and add the \
                 machine again"
                    .to_owned(),
            ),
        ),
        Err(e) => (
            WorkspaceState::Unreachable,
            Some(format!("cannot read this workspace's token: {e}")),
        ),
    }
}

/// How a retry's attempt ended: its tunnel's first attempt connected or failed, or the link
/// closed first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// Connected.
    Connected,
    /// It failed (the tunnel is unreachable, or checking again by itself).
    Failed,
    /// The link closed (removed, replaced, the app quitting) before either.
    Closed,
}

/// Told once how a fresh tunnel's first attempt ended ([`Outcome`]).
pub(crate) type Ended = Box<dyn FnOnce(Outcome) + Send>;

/// What a link's follower keeps in step.
#[derive(Clone)]
pub(crate) struct Follow {
    pub(crate) registry: Arc<Registry>,
    pub(crate) tokens: Arc<dyn TokenStore>,
    /// Whether this computer's ssh may answer prompts, for a failed sign-in's detail.
    pub(crate) ssh: SshCheck,
}

/// A remote workspace's tunnel, and the task following it.
pub(crate) struct Link {
    tunnel: Tunnel,
    follower: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl fmt::Debug for Link {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Link")
            .field("tunnel", &self.tunnel)
            .finish_non_exhaustive()
    }
}

impl Link {
    /// Follows `tunnel` for workspace `workspace` as `follow` says, on `runtime`. `ended` is told
    /// how the tunnel's first attempt ends (a retry's attempt).
    pub(crate) fn start(
        workspace: String,
        tunnel: Tunnel,
        follow: Follow,
        ended: Option<Ended>,
        runtime: &tokio::runtime::Handle,
    ) -> Self {
        // Wrapped before the task starts, so a follower stopped before it ever ran still tells.
        let ended = EndOnce(ended);
        let follower = runtime.spawn(follow_tunnel(workspace, tunnel.clone(), follow, ended));
        Self {
            tunnel,
            follower: Mutex::new(Some(follower)),
        }
    }

    /// The computer woke, or the network changed: the tunnel checks its way at once.
    pub(crate) fn wake(&self) {
        self.tunnel.wake();
    }

    /// Whether connections go through now.
    pub(crate) fn is_connected(&self) -> bool {
        self.tunnel.state().is_connected()
    }

    /// Stops following the tunnel at once (another link takes over the workspace's state).
    pub(crate) fn detach(&self) {
        let follower = self
            .follower
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(follower) = follower {
            follower.abort();
        }
    }

    /// Stops the tunnel (every connection through it ends) and the task following it.
    pub(crate) async fn close(&self) {
        self.tunnel.close().await;
        self.detach();
    }
}

/// Tells its [`Ended`] once: when told to, or [`Outcome::Closed`] when dropped first (the
/// follower was stopped).
struct EndOnce(Option<Ended>);

impl EndOnce {
    fn fire(&mut self, outcome: Outcome) {
        if let Some(ended) = self.0.take() {
            ended(outcome);
        }
    }
}

impl Drop for EndOnce {
    fn drop(&mut self) {
        self.fire(Outcome::Closed);
    }
}

/// Keeps workspace `workspace`'s state in step with `tunnel`'s, saves the transport worth
/// remembering, and asks the hub its name on each connect, until the tunnel closes. Connected but
/// without its token in the keychain, it is `needs_pairing`, not `ready`. `ended` hears how the
/// first attempt ended, once this state is set (so a next attempt's states come after it).
async fn follow_tunnel(workspace: String, tunnel: Tunnel, follow: Follow, mut ended: EndOnce) {
    let mut watch = tunnel.watch();
    let mut was_connected = false;
    loop {
        let now = watch.borrow_and_update().clone();
        if now == LinkState::Closed {
            return;
        }
        let (mut state, mut detail) = state_of(&now);
        if state == WorkspaceState::Ready {
            (state, detail) = token_state(&workspace, &*follow.tokens);
        }
        if matches!(
            now,
            LinkState::Unreachable {
                why: Unreachable::SignIn,
                ..
            }
        ) && let Some(refusal) = follow.ssh.refusal().await
        {
            detail = Some(match detail {
                Some(reason) => format!("{reason}; {refusal}"),
                None => refusal,
            });
        }
        tracing::debug!(%workspace, host = tunnel.host(), ?state, "remote link");
        follow.registry.set_state(&workspace, state, detail);
        if now.is_connected() {
            if let Some(transport) = tunnel.transport()
                && let Err(e) = follow.registry.set_transport(&workspace, transport)
            {
                tracing::warn!(%workspace, error = %e, "cannot save the remembered transport");
            }
            if !was_connected {
                tokio::spawn(refresh_name(
                    workspace.clone(),
                    tunnel.clone(),
                    Arc::clone(&follow.registry),
                    Arc::clone(&follow.tokens),
                ));
            }
        }
        was_connected = now.is_connected();
        if now != LinkState::Connecting {
            ended.fire(if now.is_connected() {
                Outcome::Connected
            } else {
                Outcome::Failed
            });
        }
        if watch.changed().await.is_err() {
            return;
        }
    }
}

/// Asks the hub behind `tunnel` its workspace's name, and takes it for workspace `workspace`,
/// cleaned and cut as at pairing. A hub that now hosts another workspace is not taken at its
/// word.
async fn refresh_name(
    workspace: String,
    tunnel: Tunnel,
    registry: Arc<Registry>,
    tokens: Arc<dyn TokenStore>,
) {
    let connector = RemoteConnector::new(workspace.clone(), tunnel, tokens);
    match crate::daemon::hosted_workspace(&connector).await {
        Ok((id, name)) if id == workspace => {
            if let Err(e) = registry.rename_remote(&workspace, &super::workspace_name(&name)) {
                tracing::warn!(%workspace, error = %e, "the workspace's new name is not saved");
            }
        }
        Ok(_) => {
            tracing::warn!(%workspace, "the hub reports another workspace now; its name is not taken");
        }
        Err(e) => {
            tracing::debug!(%workspace, error = %super::tidy(&e.message), "cannot ask the hub its workspace's name");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pitcrew_remote::{Transport, Unreachable};

    #[test]
    fn connected_without_a_token_needs_pairing() {
        let tokens = crate::keychain::MemoryStore::default();
        let (state, detail) = token_state("01JR", &tokens);
        assert_eq!(state, WorkspaceState::NeedsPairing);
        assert!(detail.unwrap().contains("add the machine again"));
        tokens
            .set("01JR", &DeviceToken::new("pcd_x").unwrap())
            .unwrap();
        assert_eq!(token_state("01JR", &tokens), (WorkspaceState::Ready, None));
    }

    #[test]
    fn link_states_map_to_workspace_states() {
        assert_eq!(
            state_of(&LinkState::Connected {
                transport: Transport::Forwarded
            }),
            (WorkspaceState::Ready, None)
        );
        assert_eq!(
            state_of(&LinkState::Connecting),
            (WorkspaceState::Connecting, None)
        );
        assert_eq!(
            state_of(&LinkState::Unverifiable {
                reason: "no answer through the forward".into()
            }),
            (
                WorkspaceState::Connecting,
                Some("no answer through the forward".into())
            )
        );
        let (state, detail) = state_of(&LinkState::Unreachable {
            why: Unreachable::Network,
            reason: "Timeout, server hpc-login not responding.\u{1b}[0m".into(),
        });
        assert_eq!(state, WorkspaceState::Unreachable);
        assert_eq!(
            detail.as_deref(),
            Some("Timeout, server hpc-login not responding. [0m")
        );
        assert_eq!(state_of(&LinkState::Closed).0, WorkspaceState::Unreachable);
    }
}
