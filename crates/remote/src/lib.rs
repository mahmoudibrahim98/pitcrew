//! # pitcrew-remote
//!
//! Remote machines: SSH connection manager, helper deployment, launchers (direct, tmux, systemd, SLURM), tunnels, reconnect.
//!
//! **Owned by stream J.** The work packages are in `docs/build/streams/J.md`. Build against
//! `pitcrew-protocol` and `pitcrew-interfaces` only, never another stream's internals.
//!
//! This layer talks to any host the user can already reach with their own **system OpenSSH**:
//! - [`list_hosts`] reads `~/.ssh/config`; [`Ssh::resolve`] asks `ssh -G` what a host means;
//! - [`Ssh::run`] runs a command under any login shell (see [`quote`]), with agent and X11
//!   forwarding off and host keys confirmed through the user;
//! - [`askpass`] brings password, passphrase, one-time-code and host-key prompts to the
//!   desktop without storing them;
//! - [`Ssh::probe`] reports what the machine is;
//! - [`helper`] deploys the static helper there ([`deploy`]) and starts, checks and stops it
//!   ([`Launcher`]).

pub mod askpass;
mod config;
pub mod helper;
// The crate's only unsafe code: a Job Object for ssh on Windows.
#[cfg(windows)]
mod job;
mod private;
pub mod probe;
pub mod quote;
mod report;
mod ssh;

pub use askpass::{
    CancelTrigger, PromptCancel, PromptFuture, PromptHandler, PromptKind, PromptRequest, Reply,
    Secret,
};
pub use config::{HostList, home_dir, list_hosts, list_hosts_in};
pub use helper::{
    DeployOptions, Deployed, DirectLauncher, Endpoint, Helper, HelperError, HelperState,
    LaunchOptions, Launcher, Layout, Platform, Started, Status, Stopped, Target, TmuxLauncher,
    deploy,
};
pub use probe::{PROBE_LIMITS, Probe};
pub use ssh::{
    DEFAULT_CONNECT_TIMEOUT, Input, Limits, Output, RESOLVE_LIMITS, ResolvedHost, Ssh, SshError,
};

/// The protocol version this crate was built against.
pub const PROTOCOL_VERSION: u32 = pitcrew_protocol::PROTOCOL_VERSION;
