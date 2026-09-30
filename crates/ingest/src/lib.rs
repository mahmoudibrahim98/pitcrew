//! # pitcrew-ingest
//!
//! Transcript parsers for Claude Code, Codex and OpenCode (incremental, by offset), and the machine scan.
//!
//! **Owned by stream A.** The work packages are in `docs/build/streams/A.md`. Build against
//! `pitcrew-protocol` and `pitcrew-interfaces` only, never another stream's internals.
//!
//! - [`claude::ClaudeAdapter`]: Claude Code transcripts (`~/.claude/projects/*/*.jsonl`).
//!
//! Transcripts are attacker-controllable text: every parser bounds its allocations, skips lines it
//! cannot use, and exposes a `parse_line` function so it can be fuzzed on its own.

#![forbid(unsafe_code)]

pub mod claude;
mod lines;
mod text;
mod time;

pub use lines::{MAX_LINE_BYTES, SkipReason, SkippedLine};

/// The protocol version this crate was built against.
pub const PROTOCOL_VERSION: u32 = pitcrew_protocol::PROTOCOL_VERSION;
