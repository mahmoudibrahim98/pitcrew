//! # pitcrew-runner
//!
//! Runner service: watchers, session linking, derived events to the hub, file API.
//!
//! **Owned by stream D.** The work packages are in `docs/build/streams/D.md`. Build against
//! `pitcrew-protocol` and `pitcrew-interfaces` only, never another stream's internals.

/// The protocol version this crate was built against.
pub const PROTOCOL_VERSION: u32 = pitcrew_protocol::PROTOCOL_VERSION;
