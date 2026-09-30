//! # pitcrew-hub-work
//!
//! Hub work model: projects, workstreams, tasks and subtasks, asks, dispatch and queue, roles, orders, personas, teams.
//!
//! **Owned by stream E.** The work packages are in `docs/build/streams/E.md`. Build against
//! `pitcrew-protocol` and `pitcrew-interfaces` only, never another stream's internals.

/// The protocol version this crate was built against.
pub const PROTOCOL_VERSION: u32 = pitcrew_protocol::PROTOCOL_VERSION;
