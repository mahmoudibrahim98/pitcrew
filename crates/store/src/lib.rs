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
pub mod migrations;
mod scan;
mod store;

pub use error::{DbError, Error, Result};
pub use store::{
    EventFilter, MAX_SUBSCRIBER_CAPACITY, RevRange, Store, StoreOptions, StoredEvent, event_type,
};

/// The protocol version this crate was built against.
pub const PROTOCOL_VERSION: u32 = pitcrew_protocol::PROTOCOL_VERSION;
