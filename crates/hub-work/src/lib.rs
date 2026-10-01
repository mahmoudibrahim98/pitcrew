//! # pitcrew-hub-work
//!
//! The hub's model of the work, built from the event log (ADR-0004, ADR-0007):
//! - [`projections`]: the tables (migrations `0200`–`0210` in `crates/store/migrations`) kept in
//!   step with the log. Pass them to `Store::open_with`;
//! - [`WorkService`]: reads of those tables, and **commands** that validate a change against them
//!   and append its events, stamped from the [`Caller`](pitcrew_protocol::api::Caller);
//! - [`routes`]: the work routes of API v1, split into [`agent_routes`] and [`device_routes`] for
//!   `RouterParts::agent` and `RouterParts::device`;
//! - two seams for other streams: [`EventRefs`] (the activity reference index, for the API
//!   layer's `GET /v1/events` filters) and [`Dispatcher`] (starting dispatched sessions, for the
//!   runner link).
//!
//! **One writer.** Exactly one `Arc<WorkService>` per store appends work events; everything that
//! changes the work model goes through it (see [`WorkService`], "One writer"). The projections
//! stay deterministic if that rule is broken, but commands then lose races: a task created with a
//! key another writer took, or moved after another writer moved it, answers `409 conflict`.
//!
//! ```no_run
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! use pitcrew_hub_work::{WorkService, projections};
//! use pitcrew_store::{Store, StoreOptions};
//! use std::sync::Arc;
//!
//! let store = Arc::new(Store::open_with("pitcrew.db", StoreOptions::default(), projections())?);
//! let demo = pitcrew_fixtures::demo_workspace()?;
//! // The one writer for this store; share this `Arc`.
//! let work = Arc::new(WorkService::new(store, demo.workspace.clone()));
//! work.seed(&demo)?;
//! // The API layer mounts the routes and adds the service as an extension:
//! let agent = pitcrew_hub_work::agent_routes::<()>().layer(axum::Extension(Arc::clone(&work)));
//! let device = pitcrew_hub_work::device_routes::<()>().layer(axum::Extension(Arc::clone(&work)));
//! // ... and filters activity through the reference index:
//! let refs: Arc<dyn pitcrew_hub_work::EventRefs> = work;
//! # let _ = (agent, device, refs);
//! # Ok(()) }
//! ```
//!
//! **Owned by stream E.** The work packages are in `docs/build/streams/E.md`.

#![forbid(unsafe_code)]

mod activity;
mod codec;
mod commands;
mod dispatch;
mod edits;
mod error;
pub mod projection;
pub mod query;
pub mod routes;
mod seed;
mod service;

pub use activity::EventRefs;
pub use commands::{AnswerAsk, BriefEdit, NewAsk, NewComment, WorkstreamPatch};
pub use dispatch::{DispatchError, DispatchRequest, Dispatcher, NewDispatch};
pub use edits::{LABEL_CHARS, MAX_LABELS, TITLE_CHARS};
pub use error::{INTERNAL_MESSAGE, Result, WorkError};
pub use pitcrew_protocol::api::{NewProject, NewTask, NewWorkstream};
pub use pitcrew_protocol::model::TaskPatch;
pub use projection::projections;
pub use query::{AskFilter, REF_SCAN_BUDGET, RefFilter, SessionFilter, TaskFilter, TaskRef};
pub use routes::{agent_routes, device_routes, routes};
pub use seed::demo_events;
pub use service::{Clock, WorkService, WorkspaceAt};

/// The protocol version this crate was built against.
pub const PROTOCOL_VERSION: u32 = pitcrew_protocol::PROTOCOL_VERSION;
