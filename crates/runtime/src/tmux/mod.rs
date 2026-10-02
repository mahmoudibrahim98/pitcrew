//! The tmux runtime: PitCrew's terminals are windows of a private tmux server.
//!
//! - **One server for PitCrew**, on its own socket in a directory only the user can open (0700,
//!   [`TmuxOptions::default_socket`]). A person's own tmux server and sessions are never touched.
//!   The server is started with `-f /dev/null`, so the user's `~/.tmux.conf` does not apply.
//! - **One session**, [`SESSION`], holds every terminal as a window. One long-lived control client
//!   (`tmux -C`) is attached to it: commands go over its stdin, replies are matched to them in
//!   order, and `%output` feeds each terminal's replay buffer and screen model.
//! - **Each window is tagged** with the pane option [`TERMINAL_OPTION`], so a new runtime finds
//!   its terminals after a restart, and [`OFFSET_OPTION`] records where its output numbering
//!   resumes.
//! - **People can attach:** `tmux -S <socket> attach -t pitcrew` (the socket is
//!   [`TmuxRuntime::socket`]); a terminal's `native_target` (`pitcrew:@12`) selects its window.
//!
//! The session exists while terminals do: the server exits when the last one ends. The runtime is
//! Unix-only; [`detect`] says so elsewhere.

mod detect;

#[cfg(unix)]
mod conn;
#[cfg(unix)]
mod runtime;
#[cfg(unix)]
mod socket;
#[cfg(unix)]
mod state;

use std::path::PathBuf;
use std::time::Duration;

pub use detect::{Detecting, TmuxSupport, detect, detect_async};
#[cfg(unix)]
pub use runtime::TmuxRuntime;

/// The tmux session that holds PitCrew's terminals.
pub const SESSION: &str = "pitcrew";

/// The pane option naming the terminal a pane belongs to (`term_…`).
pub const TERMINAL_OPTION: &str = "@pitcrew-terminal";

/// The pane option holding the offset at which a terminal's output numbering resumes after a
/// restart. It is never below an offset a reader has seen.
pub const OFFSET_OPTION: &str = "@pitcrew-offset";

/// Where and how the runtime runs tmux.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct TmuxOptions {
    /// The tmux executable, looked up on `PATH` unless it contains a slash.
    pub tmux: PathBuf,
    /// The private server's socket. Its directory must belong to the user and be closed to
    /// everyone else (mode 0700); it is created if missing.
    pub socket: PathBuf,
    /// Variables for the tmux processes the runtime starts. A server they start, and so every
    /// terminal in it, inherits them on top of this process's environment.
    pub env: Vec<(String, String)>,
    /// The longest any call waits for tmux, except `start`.
    pub call_timeout: Duration,
    /// The longest `start` waits, including starting the server.
    pub start_timeout: Duration,
    /// Output history kept per terminal, in bytes.
    pub history: usize,
}

impl TmuxOptions {
    /// Options for a server on `socket`, with defaults for the rest.
    pub fn new(socket: impl Into<PathBuf>) -> Self {
        Self {
            tmux: PathBuf::from("tmux"),
            socket: socket.into(),
            env: Vec::new(),
            call_timeout: Duration::from_secs(5),
            start_timeout: Duration::from_secs(15),
            history: crate::replay::DEFAULT_CAPACITY,
        }
    }

    /// `/tmp/pitcrew-<uid>/tmux` on Unix: short enough for any platform's socket path limit,
    /// and the same for every PitCrew process of this user, so a restarted daemon finds it.
    pub fn default_socket() -> PathBuf {
        #[cfg(unix)]
        {
            PathBuf::from(format!(
                "/tmp/pitcrew-{}/tmux",
                rustix::process::getuid().as_raw()
            ))
        }
        #[cfg(not(unix))]
        {
            std::env::temp_dir().join("pitcrew").join("tmux")
        }
    }
}

impl Default for TmuxOptions {
    fn default() -> Self {
        Self::new(Self::default_socket())
    }
}
