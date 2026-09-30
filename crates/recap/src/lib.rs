//! # pitcrew-recap
//!
//! Recap engine: activity blocks, summaries with receipts, and Where-it-stands proposals.
//!
//! **Owned by stream F.** The work packages are in `docs/build/streams/F.md`. Build against
//! `pitcrew-protocol` and `pitcrew-interfaces` only, never another stream's internals.

/// The protocol version this crate was built against.
pub const PROTOCOL_VERSION: u32 = pitcrew_protocol::PROTOCOL_VERSION;
