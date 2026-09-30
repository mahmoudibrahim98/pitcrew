//! Terminal runtimes (ADR-0005).
//!
//! A runtime owns terminals in which agent CLIs run:
//! - **tmux**, the default where it exists. Sessions survive runner restarts, and people can
//!   `tmux attach` themselves. It is driven through tmux control mode (one long-lived connection),
//!   never one process per keystroke.
//! - **PTY**, a PitCrew-owned supervisor (ConPTY on Windows, POSIX `pty` elsewhere). The screen
//!   state comes from a terminal emulator.
//!
//! Output is addressed by **byte offset**. A reader asks for bytes from an offset, and a
//! reconnecting client replays exactly what it missed, the same idea as transcripts.

use pitcrew_protocol::ids::TerminalId;
use pitcrew_protocol::runner::Key;
use serde::{Deserialize, Serialize};

/// Which kind of runtime this is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeKind {
    /// tmux via control mode.
    Tmux,
    /// PitCrew's own PTY supervisor.
    Pty,
}

/// What to start in a new terminal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartSpec {
    /// Program to run, e.g. `claude`. It is resolved through the login shell's PATH.
    pub program: String,
    /// Arguments.
    pub args: Vec<String>,
    /// Working directory.
    pub cwd: String,
    /// Extra environment variables. **No secrets:** account homes are passed by path only.
    pub env: Vec<(String, String)>,
    /// A human-readable name, used for the tmux window name.
    pub name: String,
    /// Initial size.
    pub cols: u16,
    /// Initial size.
    pub rows: u16,
}

/// A terminal the runtime knows about.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalInfo {
    /// Id.
    pub id: TerminalId,
    /// Name.
    pub name: String,
    /// Process id of the program, if known.
    pub pid: Option<u32>,
    /// Whether the program is still running.
    pub alive: bool,
    /// For tmux, the target such as `project:@12`, so people can attach themselves.
    pub native_target: Option<String>,
}

/// The visible screen. It is used to detect prompts and menus without scraping raw bytes.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Screen {
    /// Visible rows, top to bottom, trailing spaces trimmed.
    pub rows: Vec<String>,
    /// Width in columns.
    pub cols: u16,
    /// Cursor row, 0-based.
    pub cursor_row: u16,
    /// Cursor column, 0-based.
    pub cursor_col: u16,
}

/// A slice of a terminal's output stream.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OutputChunk {
    /// Offset of `data[0]` in the stream.
    pub offset: u64,
    /// The bytes.
    pub data: Vec<u8>,
    /// Offset just after the last byte available now.
    pub end: u64,
    /// True if the requested offset had already been dropped from the replay buffer, so `offset`
    /// is later than requested.
    pub truncated: bool,
}

/// Errors from runtimes.
#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    /// No such terminal.
    #[error("no such terminal: {0}")]
    NotFound(TerminalId),
    /// The program could not be started.
    #[error("could not start {program:?}: {reason}")]
    Spawn {
        /// The program.
        program: String,
        /// Why.
        reason: String,
    },
    /// This runtime is not available on this machine (e.g. no tmux).
    #[error("runtime unavailable: {0}")]
    Unavailable(String),
    /// Any other I/O failure.
    #[error("runtime I/O: {0}")]
    Io(#[from] std::io::Error),
}

/// A terminal runtime.
pub trait Runtime: Send + Sync {
    /// Which kind this is.
    fn kind(&self) -> RuntimeKind;

    /// Starts a program in a new terminal.
    fn start(&self, spec: &StartSpec) -> Result<TerminalInfo, RuntimeError>;

    /// Writes raw bytes (typed text) to a terminal.
    fn write(&self, id: TerminalId, bytes: &[u8]) -> Result<(), RuntimeError>;

    /// Sends named keys, in order.
    fn send_keys(&self, id: TerminalId, keys: &[Key]) -> Result<(), RuntimeError>;

    /// Resizes a terminal.
    fn resize(&self, id: TerminalId, cols: u16, rows: u16) -> Result<(), RuntimeError>;

    /// The visible screen now.
    fn screen(&self, id: TerminalId) -> Result<Screen, RuntimeError>;

    /// Output from `from` onwards, at most `max` bytes.
    fn read_output(
        &self,
        id: TerminalId,
        from: u64,
        max: usize,
    ) -> Result<OutputChunk, RuntimeError>;

    /// Terminal details, including whether its program is still alive.
    fn info(&self, id: TerminalId) -> Result<TerminalInfo, RuntimeError>;

    /// All terminals this runtime owns, including ones started before a runner restart.
    fn list(&self) -> Result<Vec<TerminalInfo>, RuntimeError>;

    /// Kills a terminal and its program.
    fn kill(&self, id: TerminalId) -> Result<(), RuntimeError>;
}
