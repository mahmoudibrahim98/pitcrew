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
//!   ([`Launcher`]): directly, in tmux, or as a SLURM batch job on a compute node
//!   ([`helper::slurm`], with site recipes);
//! - [`tunnel`] keeps a way to the helper's daemon open ([`Connector`]): a forwarded socket or
//!   the stdio bridge ([`bridge`], the remote end), to a login node or a job's compute node,
//!   noticing a lost connection within ten seconds and recovering by itself.

pub mod askpass;
pub mod bridge;
mod config;
pub mod helper;
// The crate's only unsafe code, Win32 calls on Windows: a Job Object for ssh, and the askpass
// pipe's security descriptor.
#[cfg(windows)]
mod job;
#[cfg(windows)]
mod pipe_security;
mod private;
pub mod probe;
pub mod quote;
mod report;
mod ssh;
pub mod wsl;
pub use wsl::{Wsl, WslDistro, WslDistros};
pub mod tunnel;

pub use askpass::{
    CancelTrigger, PromptCancel, PromptFuture, PromptHandler, PromptKind, PromptRequest, Reply,
    Secret,
};
pub use config::{HostList, home_dir, list_hosts, list_hosts_in};
pub use helper::slurm::{
    JobOptions, JobScript, JobSpec, JobState, LastHop, Site, SiteRecipe, SlurmStatus, SocketPlace,
};
pub use helper::{
    DeployOptions, Deployed, DirectLauncher, Endpoint, Helper, HelperError, HelperState,
    LaunchOptions, Launcher, Layout, Platform, SlurmLauncher, Started, Status, Stopped, Target,
    TmuxLauncher, deploy,
};
pub use probe::{PROBE_LIMITS, Probe, SlurmTools};
pub use ssh::{
    DEFAULT_CONNECT_TIMEOUT, Input, Limits, MINIMAL_ENV, Output, RESOLVE_LIMITS, ResolvedHost, Ssh,
    SshError,
};
pub use tunnel::{
    Connector, ConnectorOptions, Daemon, LinkState, Transport, TunnelError, TunnelStream,
    Unreachable, WallClock,
};

/// The protocol version this crate was built against.
pub const PROTOCOL_VERSION: u32 = pitcrew_protocol::PROTOCOL_VERSION;
