//! Protocol between a hub and the runner on each machine.
//!
//! Transport: newline-delimited JSON (one message per line) over the runner's local socket. From
//! the desktop this is reached through an SSH-forwarded unix socket, or an SSH stdio bridge where
//! forwarding is disabled (ADR-0009). Use [`encode_line`] and [`decode_line`].
//!
//! Flow:
//! 1. The runner sends [`RunnerToHub::Hello`]. The hub replies [`HubToRunner::Welcome`] with the
//!    last event cursor it holds.
//! 2. The runner streams [`RunnerToHub::Events`] from that cursor, and the hub acknowledges each
//!    batch with [`HubToRunner::AckEvents`]. Unacknowledged batches are resent after a reconnect.
//! 3. The hub sends [`HubToRunner::Command`]s. **The [`CommandId`] is an idempotency key:** a
//!    runner that sees a command id again returns the stored outcome and does not run it twice.

use crate::events::Event;
use crate::ids::{CommandId, PersonaId, SessionId, TerminalId};
use crate::model::{Engine, MachineInfo, PermissionMode, TimestampMs};
use serde::{Deserialize, Serialize};

/// What a runner can do on its machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Capability {
    /// Runs sessions in tmux, where they survive runner restarts.
    Tmux,
    /// Runs sessions in its own PTY supervisor.
    Pty,
    /// Can submit and track SLURM jobs.
    Slurm,
    /// Can watch transcript folders for changes.
    Watch,
    /// Can scan the machine for existing sessions.
    Scan,
}

/// Messages from a runner to its hub.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RunnerToHub {
    /// First message after connecting.
    Hello {
        /// The runner's version.
        runner_version: String,
        /// Protocol version it speaks.
        protocol: u32,
        /// Facts about the machine.
        machine: MachineInfo,
        /// What it can do.
        capabilities: Vec<Capability>,
    },
    /// A batch of events. `cursor` is the runner's position after this batch.
    Events {
        /// The events, in order.
        events: Vec<Event>,
        /// Position after this batch.
        cursor: u64,
    },
    /// The result of a command.
    CommandResult {
        /// The command.
        command: CommandId,
        /// Outcome.
        outcome: CommandOutcome,
    },
    /// Terminal output, starting at `offset` in the terminal's output stream.
    TerminalOutput {
        /// The terminal.
        terminal: TerminalId,
        /// Offset of the first byte.
        offset: u64,
        /// The bytes, base64-encoded.
        data_b64: String,
    },
    /// Liveness signal.
    Heartbeat {
        /// Runner time.
        at: TimestampMs,
    },
}

/// Messages from a hub to a runner.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HubToRunner {
    /// Reply to `Hello`.
    Welcome {
        /// The hub's version.
        hub_version: String,
        /// The protocol version both will use.
        protocol: u32,
        /// Resend events after this cursor.
        resume_after_cursor: u64,
    },
    /// Do something.
    Command {
        /// Id and idempotency key.
        id: CommandId,
        /// What to do.
        command: RunnerCommand,
    },
    /// Events up to `cursor` are stored.
    AckEvents {
        /// Stored up to here.
        cursor: u64,
    },
}

/// A key to send to a terminal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum Key {
    /// Enter.
    Enter,
    /// Escape.
    Escape,
    /// Tab.
    Tab,
    /// Arrow up.
    Up,
    /// Arrow down.
    Down,
    /// Arrow left.
    Left,
    /// Arrow right.
    Right,
    /// Backspace.
    Backspace,
    /// Ctrl-C.
    CtrlC,
}

/// How to end a session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum EndMode {
    /// Ask the CLI to exit, then wait for the process to go.
    Graceful,
    /// Kill the process.
    Kill,
}

/// Commands a hub sends to a runner.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum RunnerCommand {
    /// Start a new agent session.
    StartSession {
        /// The CLI.
        engine: Engine,
        /// Working directory.
        cwd: String,
        /// Session name, which the CLI also sees where supported.
        name: String,
        /// First prompt.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        brief: Option<String>,
        /// Persona it is made from.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        persona: Option<PersonaId>,
        /// Model override.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        model: Option<String>,
        /// Which CLI account home to use (e.g. a `CLAUDE_CONFIG_DIR`). The runner resolves it;
        /// secrets never travel in commands.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        account: Option<String>,
        /// Permission mode.
        #[serde(default)]
        permission_mode: PermissionMode,
    },
    /// Resume an existing CLI session by its native id.
    ResumeSession {
        /// The CLI.
        engine: Engine,
        /// The CLI's session id.
        native_id: String,
        /// Working directory.
        cwd: String,
        /// Session name.
        name: String,
    },
    /// Type text into a session and submit it.
    SendText {
        /// The session.
        session: SessionId,
        /// Text.
        text: String,
    },
    /// Send keys, for example to answer a menu.
    SendKeys {
        /// The session.
        session: SessionId,
        /// Keys, in order.
        keys: Vec<Key>,
    },
    /// Interrupt the current turn (Escape).
    Interrupt {
        /// The session.
        session: SessionId,
    },
    /// End a session.
    EndSession {
        /// The session.
        session: SessionId,
        /// How.
        mode: EndMode,
    },
    /// Resize a terminal.
    ResizeTerminal {
        /// The terminal.
        terminal: TerminalId,
        /// Columns.
        cols: u16,
        /// Rows.
        rows: u16,
    },
    /// Read terminal output from an offset. The output arrives as `TerminalOutput`.
    ReadTerminal {
        /// The terminal.
        terminal: TerminalId,
        /// Start offset.
        from_offset: u64,
        /// Maximum bytes to return.
        max_bytes: u32,
    },
    /// Scan for existing sessions under these roots (empty means the engines' default homes).
    Scan {
        /// Roots.
        #[serde(default)]
        roots: Vec<String>,
    },
}

/// The outcome of a command.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum CommandOutcome {
    /// Done. Optional structured detail, such as a new session's id.
    Ok {
        /// Detail.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(optional))]
        detail: Option<serde_json::Value>,
    },
    /// Refused by policy, for example a permission mode that isn't allowed.
    Rejected {
        /// Why.
        reason: String,
    },
    /// Tried and failed.
    Failed {
        /// What went wrong.
        error: String,
    },
}

/// Errors from framing.
#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    /// The line is not valid JSON for the expected message.
    #[error("invalid frame: {0}")]
    Invalid(#[from] serde_json::Error),
    /// The line contains a newline, which would break framing.
    #[error("frame contains a newline")]
    EmbeddedNewline,
}

/// Encodes a message as one line of JSON, including the trailing `\n`.
pub fn encode_line<T: Serialize>(message: &T) -> Result<String, FrameError> {
    let mut line = serde_json::to_string(message)?;
    if line.contains('\n') {
        return Err(FrameError::EmbeddedNewline);
    }
    line.push('\n');
    Ok(line)
}

/// Decodes one line of JSON. A trailing `\r\n` or `\n` is ignored.
pub fn decode_line<T: for<'de> Deserialize<'de>>(line: &str) -> Result<T, FrameError> {
    Ok(serde_json::from_str(line.trim_end_matches(['\r', '\n']))?)
}
