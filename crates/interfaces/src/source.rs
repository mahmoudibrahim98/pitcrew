//! Transcript source adapters: reading one agent CLI's sessions from disk.
//!
//! Rules every adapter follows (plan principles P1 and P2):
//! - **Incremental.** Read from a [`Cursor`], return a new cursor, and never re-read what has been
//!   read.
//! - **Tail-first friendly.** Very large files can be summarised from their end.
//! - **Read-only.** Never write to a CLI's files. Import is read in place.
//! - **Take `cwd` from the records, never from a folder name.** Encoded folder names cannot be
//!   reversed.

use pitcrew_protocol::model::{Engine, TimestampMs};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// A transcript on disk.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TranscriptRef {
    /// Which CLI wrote it.
    pub engine: Engine,
    /// Path: a JSONL file, or a database for OpenCode.
    pub path: PathBuf,
    /// For stores that hold many sessions in one file (OpenCode), the session id inside it.
    pub inner_id: Option<String>,
    /// Size in bytes when discovered.
    pub size: u64,
    /// Modification time when discovered.
    pub modified: TimestampMs,
}

/// Where reading stopped. `offset` is a byte offset for JSONL. `state` holds adapter-specific
/// resume data, such as the last row id for OpenCode.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cursor {
    /// Byte offset.
    pub offset: u64,
    /// Adapter-specific state.
    pub state: Option<serde_json::Value>,
}

/// Facts about a session, from its transcript.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionMeta {
    /// The CLI's own session id.
    pub native_id: String,
    /// Working directory, from the records.
    pub cwd: Option<String>,
    /// Git branch, where the CLI records it (Claude records `gitBranch` on every line).
    pub branch: Option<String>,
    /// Title: a custom title first, else a generated one.
    pub title: Option<String>,
    /// Model, if recorded.
    pub model: Option<String>,
    /// First record's time.
    pub started: Option<TimestampMs>,
    /// Whether this is a sub-agent's transcript (to be hidden or nested).
    pub is_subagent: bool,
    /// For a sub-agent, its parent session's own id (the CLI's, as in `native_id`), where the
    /// transcript names it: Claude's `sessionId` on a sub-agent's records (or the session folder
    /// above `subagents/`), Codex's `source.subagent.thread_spawn.parent_thread_id`, OpenCode's
    /// `parent_id`. `None` for a session, or for a sub-agent whose transcript names no parent (a
    /// Codex review sub-agent).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
}

/// Transcript items are wire types (the API serves them), so they live in the protocol crate.
pub use pitcrew_protocol::transcript::{PlanItem, PlanStatus, TranscriptItem, TranscriptPage};

/// The result of one incremental read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ParseChunk {
    /// Where to continue next time.
    pub cursor: Cursor,
    /// Session facts, if this read learned any.
    pub meta: Option<SessionMeta>,
    /// New items, in order.
    pub items: Vec<TranscriptItem>,
}

/// Errors from source adapters.
#[derive(Debug, thiserror::Error)]
pub enum SourceError {
    /// I/O failure.
    #[error("transcript I/O: {0}")]
    Io(#[from] std::io::Error),
    /// A record could not be understood. Adapters skip bad lines and report them without failing
    /// the whole read, unless the file is unusable.
    #[error("unreadable transcript {path}: {reason}")]
    Unreadable {
        /// The file.
        path: PathBuf,
        /// Why.
        reason: String,
    },
}

/// Reads one CLI's transcripts.
pub trait SourceAdapter: Send + Sync {
    /// Which CLI.
    fn engine(&self) -> Engine;

    /// Finds transcripts under `home`, for example `~/.claude` or an account home. Returns an
    /// empty list if the CLI has never run there.
    fn discover(&self, home: &Path) -> Result<Vec<TranscriptRef>, SourceError>;

    /// Reads from `cursor`, returning new items and the next cursor.
    fn read_from(
        &self,
        transcript: &TranscriptRef,
        cursor: &Cursor,
    ) -> Result<ParseChunk, SourceError>;

    /// Tail-first paging for the Agent console: up to `limit` items ending just before the record
    /// at byte offset `before` (or at the end of the transcript when `None`), oldest first.
    ///
    /// Implementations must read backwards from `before` and stop once they have `limit` items,
    /// so opening a huge transcript costs the same as a small one.
    fn read_page(
        &self,
        transcript: &TranscriptRef,
        before: Option<u64>,
        limit: usize,
    ) -> Result<TranscriptPage, SourceError>;
}
