//! # pitcrew-runtime
//!
//! Terminal runtimes: tmux control mode and the PTY supervisor, behind pitcrew-interfaces::runtime::Runtime.
//!
//! **Owned by stream B.** The work packages are in `docs/build/streams/B.md`. Build against
//! `pitcrew-protocol` and `pitcrew-interfaces` only, never another stream's internals.

#![forbid(unsafe_code)]

pub mod background;
pub mod command;
pub mod control;
pub mod detect;
pub mod keys;
pub mod replay;
pub mod screen;
pub mod tmux;

use std::sync::{Mutex, MutexGuard, PoisonError};

pub use control::{ControlParser, Notification, PaneId, SessionId, WindowId};
pub use detect::{TmuxVersion, detect_tmux};
pub use replay::ReplayBuffer;
pub use tmux::TmuxOptions;
#[cfg(unix)]
pub use tmux::TmuxRuntime;

/// The protocol version this crate was built against.
pub const PROTOCOL_VERSION: u32 = pitcrew_protocol::PROTOCOL_VERSION;

/// Locks `mutex`, carrying on past a panic of another holder: every lock here guards state that
/// stays consistent between statements.
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
