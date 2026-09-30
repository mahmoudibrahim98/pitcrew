//! # pitcrew-cli
//!
//! `pitcrew`, the command agents run to see and report on their work, and `pitcrew hook`, which
//! runs on every agent turn.
//!
//! - [`config`]: where the daemon is and which token to send (`PITCREW_*`).
//! - [`transport`]: blocking connections, checked before any token is sent.
//! - [`http`]: just enough HTTP/1.1.
//! - [`client`]: API calls.
//! - [`error`]: errors and exit codes.
//!
//! There is no async runtime: a verb is a few blocking requests, and the hook one.
//!
//! **Owned by stream I.** The work packages are in `docs/build/streams/I.md`.

pub mod client;
pub mod config;
pub mod error;
pub mod http;
pub mod transport;
