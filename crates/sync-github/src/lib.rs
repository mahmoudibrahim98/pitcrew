//! # pitcrew-sync-github
//!
//! GitHub, read side: read issues, pull requests and milestones for a set of repositories
//! efficiently and safely, and turn what changed upstream into typed [`UpstreamChange`]s. This
//! crate is read-only and never writes to GitHub — applying changes to the hub, the write-
//! approval queue and outward writes are later briefs.
//!
//! **Owned by stream G.** Build against `pitcrew-protocol` only, never another stream's internals.
//!
//! ## Shape
//!
//! - [`transport::Transport`]: the seam. This crate has no HTTP client or TLS dependency; the real
//!   HTTPS transport is a dependency decision for later. Tests use [`fixture::ReplayTransport`].
//! - [`client::GithubClient`]: REST v3 over a `Transport`, with pagination, conditional requests
//!   (`ETag`/`Last-Modified`) and rate-limit handling.
//! - [`state::SyncState`]: what the caller persists between calls (cursors, `ETag`s, snapshots of
//!   each tracked item's owned fields). It never goes into the event log.
//! - [`sync::sync`]: `sync(state, transport, config) -> SyncOutcome`, pure beyond the `Transport`
//!   it is given.
//! - [`change::UpstreamChange`]: what changed upstream, each carrying an `ExternalRef` and the
//!   upstream time.
//! - [`ownership::plan`]: turns one `UpstreamChange` into abstract hub [`ownership::Intent`]s,
//!   respecting the field-ownership table in [`ownership::ISSUE_FIELD_OWNERSHIP`].

#![forbid(unsafe_code)]

pub mod bounds;
pub mod change;
pub mod client;
pub mod fixture;
pub mod link_header;
pub mod links;
pub mod ownership;
pub mod state;
pub mod sync;
pub mod time;
pub mod transport;
mod wire;

pub use change::UpstreamChange;
pub use client::{ClientError, GithubClient, Outcome};
pub use ownership::{FieldOwner, FieldOwnership, ISSUE_FIELD_OWNERSHIP, Intent, plan};
pub use state::{CloseReason, RepoState, SyncState};
// `sync::sync` (the function) is not re-exported at the crate root to avoid shadowing the
// `sync` module itself; call it as `pitcrew_sync_github::sync::sync(..)`.
pub use sync::{
    InvalidRepoRef, RateLimited, RepoRef, Resource, SyncConfig, SyncIssue, SyncOutcome,
};
pub use time::GithubTimestamp;
pub use transport::{AuthToken, Method, Request, Response, Transport, TransportError};

/// The protocol version this crate was built against.
pub const PROTOCOL_VERSION: u32 = pitcrew_protocol::PROTOCOL_VERSION;
