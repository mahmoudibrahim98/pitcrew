//! # pitcrew-ingest
//!
//! Transcript parsers for Claude Code, Codex and OpenCode (incremental, by offset), and the machine scan.
//!
//! **Owned by stream A.** The work packages are in `docs/build/streams/A.md`. Build against
//! `pitcrew-protocol` and `pitcrew-interfaces` only, never another stream's internals.

/// The protocol version this crate was built against.
pub const PROTOCOL_VERSION: u32 = pitcrew_protocol::PROTOCOL_VERSION;
