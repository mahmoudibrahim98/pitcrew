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
//!   `TmuxRuntime::socket`); a terminal's `native_target` (`pitcrew:@12`) selects its window.
//! - **The server is shared with the programs in it.** tmux gives every pane `$TMUX`, the
//!   server's socket; the runtime unsets it for the programs it starts, but a program that
//!   finds the socket (it is in a predictable place) can still run any tmux command on that
//!   server, as the user. Against that, the runtime limits the damage rather than draws a
//!   boundary: replies without its guard flag (ordinary hook output) are dropped, tags and
//!   duplicated rows never move or end a known terminal, and pane output is bounded before the
//!   screen model sees it. A deliberate attacker running as the same user can still disturb
//!   PitCrew's terminals in other ways.
//!
//! The session exists while terminals do: the server exits when the last one ends. The runtime is
//! Unix-only; [`detect`] says so elsewhere.

mod detect;

#[cfg(unix)]
mod conn;
#[cfg(unix)]
pub(crate) mod exe;
#[cfg(unix)]
mod runtime;
#[cfg(unix)]
pub(crate) mod socket;
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
    /// The tmux executable. A name without a slash is looked up on `PATH` (absolute entries
    /// only) once, when the runtime is created or tmux is detected.
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

    /// Where PitCrew's server lives by default, the first that is usable of:
    /// - `$TMUX_TMPDIR/pitcrew-<uid>/tmux`, if `TMUX_TMPDIR` is a private directory of this
    ///   user's (where the user keeps tmux sockets);
    /// - `$XDG_RUNTIME_DIR/pitcrew/tmux`, if that is private (it usually is). Note that the
    ///   system removes it when the user's last login session ends, socket included, so a
    ///   runner that must outlive logins should pass its own socket;
    /// - `/tmp/pitcrew-<uid>/tmux`.
    ///
    /// Each is short enough for every platform's socket path limit. The choice depends on the
    /// environment: a daemon restarted with a different one would not find its terminals, so
    /// a host should keep the socket it chose (or pass one) across restarts.
    pub fn default_socket() -> PathBuf {
        #[cfg(unix)]
        {
            let uid = rustix::process::getuid().as_raw();
            let private = |var: &str| {
                std::env::var_os(var)
                    .map(PathBuf::from)
                    .filter(|dir| dir.is_absolute() && socket::is_private_dir(dir))
            };
            let candidates = [
                private("TMUX_TMPDIR").map(|dir| dir.join(format!("pitcrew-{uid}")).join("tmux")),
                private("XDG_RUNTIME_DIR").map(|dir| dir.join("pitcrew").join("tmux")),
            ];
            candidates
                .into_iter()
                .flatten()
                .find(|socket| socket.as_os_str().len() <= 100)
                .unwrap_or_else(|| PathBuf::from(format!("/tmp/pitcrew-{uid}/tmux")))
        }
        #[cfg(not(unix))]
        {
            std::env::temp_dir().join("pitcrew").join("tmux")
        }
    }

    /// The tmux executable as an absolute path: `tmux` itself if it contains a slash, otherwise
    /// the first match in this process's `PATH` (absolute entries only).
    #[cfg(unix)]
    pub(crate) fn resolved_tmux(&self) -> Option<PathBuf> {
        let name = self.tmux.to_str()?;
        exe::find(
            name,
            std::env::var_os("PATH").as_deref(),
            std::path::Path::new("/"),
        )
    }
}

impl Default for TmuxOptions {
    fn default() -> Self {
        Self::new(Self::default_socket())
    }
}
