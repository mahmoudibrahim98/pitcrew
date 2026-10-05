//! # pitcrew-auth
//!
//! Device, agent and reader tokens, scopes, and author stamping (ADR-0006).
//!
//! - [`token`]: raw tokens (`pcd_…` for devices, `pca_…` for agents, `pcr_…` for readers: agents
//!   that may only read), their ids and hashes.
//! - [`store`]: the [`TokenStore`] trait and [`FileTokenStore`], a registry that keeps only
//!   SHA-256 hashes.
//! - [`private`]: private directories and files (owner and mode checks on Unix).
//! - [`http`]: what domain crates use in `routes()`: the [`Authenticated`], [`Person`] and
//!   [`Reading`] extractors, the [`device_only`] and [`readable`] guards, and [`ErrorResponse`].
//!
//! **Owned by stream H.** The work packages are in `docs/build/streams/H.md`. Build against
//! `pitcrew-protocol` and `pitcrew-interfaces` only, never another stream's internals.

pub mod http;
pub mod private;
pub mod store;
pub mod token;

pub use http::{
    Authenticated, ErrorResponse, Person, Reading, WS_BEARER_PREFIX, WS_PROTOCOL, device_only,
    readable, require_device, require_device_or_reader,
};
#[cfg(unix)]
pub use private::euid;
pub use private::{check_private_dir, create_private_dir};
pub use store::{FileTokenStore, TokenError, TokenInfo, TokenStore};
pub use token::{SecretToken, TokenId};

/// The protocol version this crate was built against.
pub const PROTOCOL_VERSION: u32 = pitcrew_protocol::PROTOCOL_VERSION;
