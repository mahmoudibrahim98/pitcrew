//! # pitcrew-sync-jira
//!
//! Two-way Jira sync: issues and epics; field ownership; write-approval queue.
//!
//! **Owned by stream G.** The work packages are in `docs/build/streams/G.md`. Build against
//! `pitcrew-protocol` and `pitcrew-interfaces` only, never another stream's internals.

/// The protocol version this crate was built against.
pub const PROTOCOL_VERSION: u32 = pitcrew_protocol::PROTOCOL_VERSION;
