//! # pitcrew-store
//!
//! SQLite store: migrations, the append-only event log, projections, and NFS-safe mode.
//!
//! **Owned by stream C.** The work packages are in `docs/build/streams/C.md`. Build against
//! `pitcrew-protocol` and `pitcrew-interfaces` only, never another stream's internals.
//!
//! ```no_run
//! # fn main() -> pitcrew_store::Result<()> {
//! use pitcrew_store::{Store, StoreOptions};
//! let store = Store::open("pitcrew.db", StoreOptions::default())?;
//! let page = store.since(0, 100)?;
//! # Ok(()) }
//! ```

#![forbid(unsafe_code)]

mod error;
mod fs_kind;
mod lease;
mod maintenance;
pub mod migrations;
pub mod projection;
mod scan;
mod store;

/// The store's `rusqlite`. Projections and [`Store::read`] use it, so every crate writes SQL
/// against the workspace's one version and the store's own transaction type. Domain crates do not
/// need their own `rusqlite` dependency.
pub use rusqlite as sql;

pub use error::{DbError, Error, Result};
pub use fs_kind::{FsKind, FsMode, detect};
pub use lease::{Clock, SystemClock};
pub use maintenance::{IntegrityReport, integrity_check};
pub use projection::{BoxError, Projection};
pub use store::{
    EventFilter, MAX_SUBSCRIBER_CAPACITY, RevRange, Store, StoreOptions, StoredEvent, event_type,
};

/// The protocol version this crate was built against.
pub const PROTOCOL_VERSION: u32 = pitcrew_protocol::PROTOCOL_VERSION;
