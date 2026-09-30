//! # pitcrew-auth
//!
//! Device and agent tokens, scopes, and author stamping (ADR-0006).
//!
//! - [`token`]: raw tokens (`pcd_…` for devices, `pca_…` for agents), their ids and hashes.
//! - [`store`]: the [`TokenStore`] trait and [`FileTokenStore`], a registry that keeps only
//!   SHA-256 hashes.
//! - [`http`]: what domain crates use in `routes()`: the [`Authenticated`] and [`Person`]
//!   extractors, the [`device_only`] guard, and [`ErrorResponse`].
//!
//! **Owned by stream H.** The work packages are in `docs/build/streams/H.md`. Build against
//! `pitcrew-protocol` and `pitcrew-interfaces` only, never another stream's internals.

pub mod http;
pub mod store;
pub mod token;

pub use http::{
    Authenticated, ErrorResponse, Person, WS_BEARER_PREFIX, WS_PROTOCOL, device_only,
    require_device,
};
pub use store::{FileTokenStore, TokenError, TokenInfo, TokenStore, create_private_dir};
pub use token::{SecretToken, TokenId};

/// The protocol version this crate was built against.
pub const PROTOCOL_VERSION: u32 = pitcrew_protocol::PROTOCOL_VERSION;
