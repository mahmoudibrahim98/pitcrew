//! # pitcrew-office
//!
//! Back office: rules first, model calls for judgement, caps and run log.
//!
//! **Owned by stream F.** The work packages are in `docs/build/streams/F.md`. Build against
//! `pitcrew-protocol` and `pitcrew-interfaces` only, never another stream's internals.

/// The protocol version this crate was built against.
pub const PROTOCOL_VERSION: u32 = pitcrew_protocol::PROTOCOL_VERSION;
