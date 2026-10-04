//! # pitcrew-sync-jira
//!
//! Read Jira issues and epics, for Jira Cloud and Jira Data Center, safely and incrementally, and
//! turn what changed upstream into typed [`UpstreamChange`]s. This crate is read-only and never
//! writes to Jira — applying changes to the hub, the write-approval queue and outward writes are
//! later briefs. Mirrors `pitcrew_sync_github` closely; see "Shape" below for what is reused
//! directly rather than forked.
//!
//! **Owned by stream G.** Build against `pitcrew-protocol` and `pitcrew-sync-github` only, never
//! another stream's internals.
//!
//! ## Shape
//!
//! - [`deployment::Deployment`]: the seam between Jira Cloud ([`deployment::JiraCloud`]: REST v3,
//!   Basic auth, `GET .../search/jql` with `nextPageToken` pagination) and Jira Data Center
//!   ([`deployment::JiraDataCenter`]: REST v2, a Bearer PAT, `GET .../search` with `startAt`
//!   pagination).
//! - [`client::JiraClient`]: the client core over a `Transport`, generic over `Deployment`.
//! - [`state::SyncState`]: what the caller persists between calls (the account time zone, and
//!   each project's cursor and snapshots). It never goes into the event log.
//! - [`sync::sync`]: `sync(state, transport, deployment, config) -> SyncOutcome`, pure beyond the
//!   `Transport` it is given.
//! - [`change::UpstreamChange`]: what changed upstream, each carrying an `ExternalRef` and the
//!   upstream time.
//! - [`ownership::plan`]: turns one `UpstreamChange` into abstract hub `Intent`s;
//!   [`ownership::plan_workstream`] does the same for an epic and a workstream that links it.
//! - [`probe::probe`]: one read of the account and each project, for "test this connection".
//!
//! ## Reuse, not a fork
//!
//! Rather than duplicate `pitcrew_sync_github`, this crate takes a dependency on it for the parts
//! that carry no GitHub-specific meaning:
//!
//! - [`pitcrew_sync_github::transport`] — the `Transport` seam, `Request`/`Response`/`Method`
//!   (`Get` only: this crate stays read-only like the GitHub brief, and both Jira deployments'
//!   search endpoints accept `GET` — see [`deployment`]'s module doc). The credential redaction
//!   this module already provides (`Request`'s `Debug` never shows `Authorization`) applies here
//!   unchanged.
//! - [`pitcrew_sync_github::fixture`] — the recorded-fixture text format and `ReplayTransport`.
//!   This crate's own `tests/fixtures/*.fixture` files use the exact same format (the block
//!   separator is literally `pitcrew_sync_github::fixture`'s own constant); a human or a tool
//!   already familiar with the GitHub fixtures needs nothing new to read these.
//! - [`pitcrew_sync_github::bounds`] — `cap_chars`, `cap_labels`, `backoff_secs`, and the
//!   title/body/label-length and backoff constants; see [`bounds`] for exactly what is reused
//!   versus added locally (ADF-specific depth/node caps).
//! - [`pitcrew_sync_github::ownership::{Intent, FieldOwner, FieldOwnership}`][gh_own] — the
//!   *shape* the brief asks to reuse: `Intent` is already generic over the hub's own model
//!   (nothing GitHub-specific in it), so a GitHub-sourced and a Jira-sourced intent are the same
//!   type the hub eventually applies. See [`ownership`] for the field-ownership table and how its
//!   `milestone` field is reused to mean "the linked epic".
//!
//! [gh_own]: pitcrew_sync_github::ownership
//!
//! What is *not* reused: `GithubTimestamp`/`JiraTimestamp` (different wire shapes — see
//! [`time`]), the HTTP client core's pagination loop (`ETag`/`Link`-header walking has no Jira
//! analogue; Jira's pagination is a token or offset inside the response body, not a header to
//! validate as same-origin, so there is no `origin`-module equivalent here either), and the
//! `UpstreamChange`/wire types (different fields entirely). A proposal for a *third* crate shared
//! by both — rather than one depending on the other — was considered and rejected: the pieces
//! above are a small, stable minority of `sync-github`, and splitting them out would need the
//! integrator to carve a new crate out of an already-merged, reviewed one for no benefit this
//! brief needs.

#![forbid(unsafe_code)]

pub mod adf;
pub mod auth;
pub mod bounds;
pub mod change;
pub mod client;
pub mod deployment;
pub mod jql;
pub mod ownership;
pub mod probe;
pub mod state;
pub mod sync;
pub mod time;
mod wire;

pub use auth::JiraAuth;
pub use change::UpstreamChange;
pub use client::{ClientError, JiraClient};
pub use deployment::{Deployment, JiraCloud, JiraDataCenter, PageState};
pub use jql::{InvalidProjectRef, ProjectRef};
pub use ownership::{
    EPIC_FIELD_OWNERSHIP, FieldOwner, FieldOwnership, ISSUE_FIELD_OWNERSHIP, Intent,
    LinkedWorkstream, plan, plan_workstream,
};
pub use state::{EpicSnapshot, IssueSnapshot, ProjectState, StatusCategory, SyncState};
// `sync::sync` (the function) is not re-exported at the crate root to avoid shadowing the `sync`
// module itself; call it as `pitcrew_sync_jira::sync::sync(..)`.
pub use sync::{RateLimited, Resource, SyncConfig, SyncIssue, SyncOutcome};
pub use time::JiraTimestamp;

/// The protocol version this crate was built against.
pub const PROTOCOL_VERSION: u32 = pitcrew_protocol::PROTOCOL_VERSION;
