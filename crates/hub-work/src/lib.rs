//! # pitcrew-hub-work
//!
//! The hub's model of the work, built from the event log (ADR-0004, ADR-0007):
//! - [`projections`]: the tables (migrations `0200`–`0206` in `crates/store/migrations`) kept in
//!   step with the log. Pass them to `Store::open_with`;
//! - [`WorkService`]: reads of those tables, and **commands** that validate a change against them
//!   and append its events, stamped from the [`Caller`](pitcrew_protocol::api::Caller);
//! - [`routes`]: the work routes of API v1, split into [`agent_routes`] and [`device_routes`] for
//!   `RouterParts::agent` and `RouterParts::device`.
//!
//! ```no_run
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! use pitcrew_hub_work::{WorkService, projections};
//! use pitcrew_store::{Store, StoreOptions};
//! use std::sync::Arc;
//!
//! let store = Arc::new(Store::open_with("pitcrew.db", StoreOptions::default(), projections())?);
//! let demo = pitcrew_fixtures::demo_workspace()?;
//! let work = Arc::new(WorkService::new(store, demo.workspace.id));
//! work.seed(&demo)?;
//! // The API layer mounts the routes and adds the service as an extension:
//! let agent = pitcrew_hub_work::agent_routes::<()>().layer(axum::Extension(Arc::clone(&work)));
//! let device = pitcrew_hub_work::device_routes::<()>().layer(axum::Extension(work));
//! # let _ = (agent, device);
//! # Ok(()) }
//! ```
//!
//! **Owned by stream E.** The work packages are in `docs/build/streams/E.md`.

#![forbid(unsafe_code)]

mod codec;
mod commands;
mod error;
pub mod projection;
pub mod query;
pub mod routes;
mod seed;
mod service;

pub use commands::{AnswerAsk, BriefEdit, NewAsk, NewComment, NewTask, WorkstreamPatch};
pub use error::{Result, WorkError};
pub use projection::projections;
pub use query::{AskFilter, SessionFilter, TaskFilter, TaskRef};
pub use routes::{agent_routes, device_routes, routes};
pub use seed::demo_events;
pub use service::{Clock, WorkService};

/// The protocol version this crate was built against.
pub const PROTOCOL_VERSION: u32 = pitcrew_protocol::PROTOCOL_VERSION;
