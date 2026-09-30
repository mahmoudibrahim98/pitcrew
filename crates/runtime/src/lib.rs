//! # pitcrew-runtime
//!
//! Terminal runtimes: tmux control mode and the PTY supervisor, behind pitcrew-interfaces::runtime::Runtime.
//!
//! **Owned by stream B.** The work packages are in `docs/build/streams/B.md`. Build against
//! `pitcrew-protocol` and `pitcrew-interfaces` only, never another stream's internals.

#![forbid(unsafe_code)]

pub mod command;
pub mod control;
pub mod detect;
pub mod keys;
pub mod replay;

pub use control::{ControlParser, Notification, PaneId, SessionId, WindowId};
pub use detect::{TmuxVersion, detect_tmux};
pub use replay::ReplayBuffer;

/// The protocol version this crate was built against.
pub const PROTOCOL_VERSION: u32 = pitcrew_protocol::PROTOCOL_VERSION;
