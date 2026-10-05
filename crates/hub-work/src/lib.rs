//! # pitcrew-hub-work
//!
//! The hub's model of the work, built from the event log (ADR-0004, ADR-0007):
//! - [`projections`]: the tables (migrations `0200`–`0210` in `crates/store/migrations`) kept in
//!   step with the log. Pass them to `Store::open_with`;
//! - [`WorkService`]: reads of those tables, and **commands** that validate a change against them
//!   and append its events, stamped from the [`Caller`](pitcrew_protocol::api::Caller);
//! - [`routes`]: the work routes of API v1, split into [`agent_routes`] and [`device_routes`] for
//!   `RouterParts::agent` and `RouterParts::device`;
//! - three seams for other streams: [`EventRefs`] (the activity reference index, for the API
//!   layer's `GET /v1/events` filters), [`RecapIndex`] (blocks and day recaps, for the recap
//!   routes; see [`recap`]) and [`Dispatcher`] (starting dispatched sessions, and board drafts'
//!   sessions, for the runner link);
//! - board drafts: an agent drafts a workstream's board from its history, and nothing is created
//!   until a person accepts it ([`WorkService::start_draft`]; the routes are
//!   [`board_agent_routes`] and [`board_device_routes`], mounted apart from the others);
//! - [`SyncCommands`]: what a tracker sync (GitHub, Jira) changes in the hub, as the sync's own
//!   member with `Mover::Sync`, and [`links`], the workstream links a sync routes issues by;
//! - [`writes`]: outward writes to GitHub and Jira, each proposed with an approval ask and started
//!   only once a person answered "Send" (the commands are on [`SyncCommands`]);
//! - the back office acting on the hub ([`BackOffice`], [`OfficeCommands`]): its run log is a
//!   projection ([`projections_with_office`]), and the daemon calls [`WorkService::run_office`]
//!   after each append to apply what it emitted.
//!
//! **One writer.** Exactly one `Arc<WorkService>` per store appends work events; everything that
//! changes the work model goes through it (see [`WorkService`], "One writer"). The projections
//! stay deterministic if that rule is broken, but commands then lose races: a task created with a
//! key another writer took, or moved after another writer moved it, answers `409 conflict`.
//!
//! ```no_run
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! use pitcrew_hub_work::{BackOffice, WorkService, projections_with_office};
//! use pitcrew_store::{RevRange, Store, StoreOptions};
//! use std::sync::Arc;
//!
//! let demo = pitcrew_fixtures::demo_workspace()?;
//! // The back office acts as its own agent member (`@office` in the demo).
//! let office = BackOffice::new("01JB000000000000000MEM0006".parse()?);
//! let store = Arc::new(Store::open_with(
//!     "pitcrew.db",
//!     StoreOptions::default(),
//!     projections_with_office(&office),
//! )?);
//! // Subscribe, then read where the log is, before anything appends: every later revision is the
//! // back office's to look at.
//! let mut appended = store.subscribe();
//! let mut last = store.latest_rev()?;
//! // The one writer for this store; share this `Arc`.
//! let work = Arc::new(WorkService::new(Arc::clone(&store), demo.workspace.clone()));
//! work.seed(&demo)?;
//! // The API layer mounts the routes and adds the service as an extension:
//! let agent = pitcrew_hub_work::agent_routes::<()>().layer(axum::Extension(Arc::clone(&work)));
//! let device = pitcrew_hub_work::device_routes::<()>().layer(axum::Extension(Arc::clone(&work)));
//! // ... filters activity through the reference index, and serves recaps from the recap index
//! // (built from the log on first use; `sync_recaps` builds it now):
//! let refs: Arc<dyn pitcrew_hub_work::EventRefs> = work.clone();
//! let recaps: Arc<dyn pitcrew_hub_work::RecapIndex> = work.clone();
//! work.sync_recaps()?;
//! // After each append (the seed, any writer's, the office's own), the back office applies what
//! // it emitted, from the first revision it has not looked at: that also covers revisions other
//! // processes appended, which are not announced.
//! while let Ok(revs) = appended.try_recv() {
//!     if revs.to_rev > last {
//!         work.run_office(&office, RevRange { from_rev: last + 1, to_rev: revs.to_rev })?;
//!         last = revs.to_rev;
//!     }
//! }
//! # let _ = (agent, device, refs, recaps);
//! # Ok(()) }
//! ```
//!
//! **Owned by stream E.** The work packages are in `docs/build/streams/E.md`.

#![forbid(unsafe_code)]

mod activity;
mod board;
mod board_routes;
mod codec;
mod commands;
mod cursors;
mod directory;
mod dispatch;
mod edits;
mod error;
mod import;
pub mod links;
mod office;
pub mod projection;
pub mod query;
pub mod recap;
mod recap_db;
pub mod routes;
mod safety;
mod seed;
mod service;
mod setup;
mod sync;
pub mod writes;

pub use activity::EventRefs;
pub use board_routes::{board_agent_routes, board_device_routes};
pub use commands::{AnswerAsk, BriefEdit, NewAsk, NewComment, SessionLink, WorkstreamPatch};
pub use dispatch::{
    DispatchError, DispatchRequest, Dispatcher, ENDED_WITHOUT_REPORT, MAX_BRIEF, NEVER_STARTED,
    NewDispatch, RecordedStart, SessionRequest,
};
pub use edits::{LABEL_CHARS, MAX_LABELS, TITLE_CHARS};
pub use error::{INTERNAL_MESSAGE, Result, WorkError};
pub use office::{
    Applied, BackOffice, OFFICE_HANDLE, OFFICE_NAME, OfficeCommands, OfficeRun,
    projections_with_office,
};
pub use pitcrew_protocol::api::{
    NewProject, NewTask, NewWorkstream, Setup, SetupDone, SetupPerson,
};
pub use pitcrew_protocol::model::TaskPatch;
pub use projection::projections;
pub use query::{AskFilter, REF_SCAN_BUDGET, RefFilter, SessionFilter, TaskFilter, TaskRef};
pub use recap::{BlockFilter, DAY_CACHE_ENTRIES, DaysScope, RecapIndex, Recaps};
pub use routes::{agent_routes, device_routes, routes};
pub use seed::demo_events;
pub use service::{Clock, WorkService, WorkspaceAt};
pub use setup::SetupListener;
pub use sync::{
    Outcome as SyncOutcome, SYNC_FALLBACK_HANDLE, SYNC_HANDLE, SYNC_NAME, SyncCommands, fit_labels,
    fit_title,
};
pub use writes::WriteFilter;

/// The protocol version this crate was built against.
pub const PROTOCOL_VERSION: u32 = pitcrew_protocol::PROTOCOL_VERSION;

pub use pitcrew_protocol::api::{PersonaEdit, TeamEdit};
