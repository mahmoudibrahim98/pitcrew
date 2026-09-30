//! # pitcrew-legacy
//!
//! Importer from the predecessor portal's state into the PitCrew store.
//!
//! **Owned by stream O.** The work packages are in `docs/build/streams/O.md`. Build against
//! `pitcrew-protocol` and `pitcrew-interfaces` only, never another stream's internals.

/// The protocol version this crate was built against.
pub const PROTOCOL_VERSION: u32 = pitcrew_protocol::PROTOCOL_VERSION;
