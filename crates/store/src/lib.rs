//! # pitcrew-store
//!
//! SQLite store: migrations, the append-only event log, projections, and NFS-safe mode.
//!
//! **Owned by stream C.** The work packages are in `docs/build/streams/C.md`. Build against
//! `pitcrew-protocol` and `pitcrew-interfaces` only, never another stream's internals.

/// The protocol version this crate was built against.
pub const PROTOCOL_VERSION: u32 = pitcrew_protocol::PROTOCOL_VERSION;
