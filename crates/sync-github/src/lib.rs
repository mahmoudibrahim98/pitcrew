//! # pitcrew-sync-github
//!
//! GitHub: read issues, pull requests and milestones for a set of repositories efficiently and
//! safely, and turn what changed upstream into typed [`UpstreamChange`]s; and build the one
//! request an approved outward write is sent as ([`write`]). The sync only reads: a write is sent
//! only by the hub, after a person approved it (api-v1.md, "Outward writes").
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
//!   respecting the field-ownership table in [`ownership::ISSUE_FIELD_OWNERSHIP`];
//!   [`ownership::plan_workstream`] does the same for a milestone and a workstream that links it
//!   ([`ownership::MILESTONE_FIELD_OWNERSHIP`]).
//! - [`probe::probe`]: one read of each repository, for "test this connection".
//! - [`write::send`]: one approved write (create an issue, comment, or edit one: title, body,
//!   labels, milestone, close or reopen), sent once.

#![forbid(unsafe_code)]

pub mod bounds;
pub mod change;
pub mod client;
pub mod fixture;
pub mod link_header;
pub mod links;
mod origin;
pub mod ownership;
pub mod probe;
pub mod state;
pub mod sync;
pub mod time;
pub mod transport;
mod wire;
pub mod write;

pub use change::UpstreamChange;
pub use client::{ClientError, GithubClient, Outcome};
pub use ownership::{
    FieldOwner, FieldOwnership, ISSUE_FIELD_OWNERSHIP, Intent, LinkedWorkstream,
    MILESTONE_FIELD_OWNERSHIP, Outward, outward, plan, plan_workstream,
};
pub use state::{CloseReason, RepoState, SyncState};
// Not public API: exposed only so stream Q's fuzz harness can call `trusted_next_url` directly,
// rather than only reaching it indirectly through `sync::sync` — see its own doc.
#[doc(hidden)]
pub use origin::trusted_next_url;
// `sync::sync` (the function) is not re-exported at the crate root to avoid shadowing the
// `sync` module itself; call it as `pitcrew_sync_github::sync::sync(..)`.
pub use sync::{
    InvalidRepoRef, RateLimited, RepoRef, Resource, SyncConfig, SyncIssue, SyncOutcome,
};
pub use time::GithubTimestamp;
pub use transport::{AuthToken, Method, Request, Response, Transport, TransportError};

/// The protocol version this crate was built against.
pub const PROTOCOL_VERSION: u32 = pitcrew_protocol::PROTOCOL_VERSION;
