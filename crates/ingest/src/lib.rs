//! # pitcrew-ingest
//!
//! Transcript parsers for Claude Code, Codex and OpenCode (incremental, by offset), and the machine scan.
//!
//! **Owned by stream A.** The work packages are in `docs/build/streams/A.md`. Build against
//! `pitcrew-protocol` and `pitcrew-interfaces` only, never another stream's internals.
//!
//! - [`claude::ClaudeAdapter`]: Claude Code transcripts (`~/.claude/projects/*/*.jsonl`).
//! - [`codex::CodexAdapter`]: Codex CLI rollouts (`~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl`).
//! - [`opencode::OpenCodeAdapter`]: OpenCode sessions in its SQLite store
//!   (`~/.local/share/opencode/opencode.db`).
//!
//! Transcripts are attacker-controllable text: every parser bounds its allocations, skips lines it
//! cannot use, and exposes a `parse_line` function so it can be fuzzed on its own.

#![forbid(unsafe_code)]

mod bound;
pub mod claude;
pub mod codex;
mod jsonl;
mod lines;
pub mod opencode;
mod patch;
mod text;
mod time;

pub use jsonl::{MAX_REPORTED_SKIPS, ReadReport};
pub use lines::{MAX_LINE_BYTES, SkipReason, SkippedLine};

/// The protocol version this crate was built against.
pub const PROTOCOL_VERSION: u32 = pitcrew_protocol::PROTOCOL_VERSION;

/// The user's home folder: `HOME`, else `USERPROFILE`.
fn user_home() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .filter(|d| !d.is_empty())
        .map(std::path::PathBuf::from)
}
