//! The helper (`pitcrewd`) on a remote machine: putting it there, starting and stopping it
//! (ADR-0009, principle P6).
//!
//! Nothing on the machine needs internet, root, a compiler or a package manager: everything
//! runs through the user's own ssh as one POSIX-sh script (`helper.sh`) using common tools.
//!
//! ```text
//! let probe = ssh.probe(host).await?;
//! let target = Target::new(ssh, host, &probe)?;          // refuses unknown platforms
//! let bytes = /* the artefact named target.platform().artefact() */;
//! let helper = Helper::new(target.platform(), VERSION, SHA256, bytes)?;
//! deploy(&target, &helper, &DeployOptions::default()).await?;
//! let started = DirectLauncher::default().start(&target).await?;
//! ```
//!
//! **On the machine**, under `~/.pitcrew` (every directory 0700, owned by the user, checked on
//! each call and refused, never repaired, when it is not):
//!
//! ```text
//! bin/<version>/pitcrewd     verified helpers, 0700
//! bin/current -> <version>   the one launchers run (relative, so it works wherever $HOME is
//!                            mounted); bin/previous names the one before
//! bin/.lock/                 deploy lock (mkdir), with an `owner` file: host, pid, call tag
//! run/endpoint.json          the running helper: pid, host, version, started, launcher, socket
//! run/pitcrewd.sock          its socket; run/pitcrewd.log its output; run/.lock the launch lock
//! ```
//!
//! **The script** travels on ssh's stdin, not on the command line: Windows limits a whole
//! command line to 32,767 characters, and the script is larger than the quarter of that the
//! shell-neutral wrapper leaves. The command line only carries a fixed bootstrap that reads
//! the script's exact length with `dd` (so the helper bytes behind it stay on stdin for the
//! upload) and runs it only if it arrived whole. Each call prints a report between markers
//! carrying a random tag, as the probe does.
//!
//! **Locks** are `mkdir` directories. A lock is stale when it is older than a limit, or was
//! taken on the same host by a process that is gone; a stale lock is moved aside atomically and
//! removed, and a holder checks that it still owns its lock before each change.

mod deploy;
mod launch;
mod script;

pub use deploy::{
    DeployOptions, Deployed, HashTool, Helper, MAX_HELPER_SIZE, Platform, Progress, deploy,
    validate_version,
};
pub use launch::{
    DirectLauncher, Endpoint, HelperFuture, HelperState, LaunchOptions, Launcher, MIN_TMUX,
    Started, Status, Stopped, TMUX_SESSION, TMUX_SOCKET, TmuxLauncher, parse_tmux_version,
};

use crate::probe::Probe;
use crate::{Ssh, SshError};

/// Why a deploy, start, status or stop failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum HelperError {
    /// The ssh call failed, or broke its limits.
    #[error(transparent)]
    Ssh(#[from] SshError),
    /// PitCrew builds no helper for this machine.
    #[error(
        "PitCrew has no helper for {os} on {arch}; there are helpers for Linux on x86_64 and \
         aarch64, and for macOS"
    )]
    UnsupportedPlatform {
        /// The operating system, as the probe normalised it.
        os: String,
        /// The architecture, as the probe normalised it.
        arch: String,
    },
    /// The helper is built for another platform than the machine's.
    #[error("the helper is built for {built}, but the machine is {machine}")]
    WrongPlatform {
        /// What the helper is built for.
        built: Platform,
        /// What the machine is.
        machine: Platform,
    },
    /// A value cannot be used: a version, a hash, a path or an option.
    #[error("invalid argument: {0}")]
    InvalidArgument(String),
    /// The helper's bytes do not have the sha256 they came with. Nothing was sent.
    #[error("the helper's bytes do not have the expected sha256; nothing was sent")]
    LocalHashMismatch,
    /// The probe found no home directory.
    #[error("the machine reported no home directory")]
    NoHome,
    /// A directory PitCrew uses on the machine is not private (another owner, group or other
    /// access, an ACL, or a symbolic link). It is not repaired: the user has to look.
    #[error("unsafe directory: {0}")]
    UnsafeDirectory(String),
    /// Another deploy, start or stop held the lock for longer than the wait.
    #[error("busy: {0}")]
    Busy(String),
    /// Another run broke this call's lock, taking it for stale. Nothing more was changed.
    #[error("lost the lock: {0}")]
    LockLost(String),
    /// None of `sha256sum`, `shasum -a 256` and `openssl dgst -sha256` works on the machine.
    #[error(
        "no sha256 tool on the machine (tried sha256sum, shasum -a 256 and openssl dgst -sha256)"
    )]
    NoHashTool,
    /// Fewer bytes arrived than were sent: the upload was cut off. Nothing was installed.
    #[error(
        "the upload was cut off: {received} of {expected} bytes arrived; nothing was installed"
    )]
    Incomplete {
        /// The helper's size.
        expected: u64,
        /// What arrived.
        received: u64,
    },
    /// The uploaded file's sha256, computed on the machine, is not the expected one. It was
    /// deleted.
    #[error("the uploaded helper's sha256 is {actual}, not {expected}; it was deleted")]
    HashMismatch {
        /// The expected hash.
        expected: String,
        /// What the machine computed.
        actual: String,
    },
    /// `pitcrewd --version` failed on the machine (for example a `noexec` home). An upload
    /// was deleted.
    #[error("the helper does not run on the machine (exit code {code:?}): {output}")]
    NotRunnable {
        /// Its exit code.
        code: Option<i32>,
        /// The first line of its output.
        output: String,
    },
    /// `pitcrewd --version` names another version. An upload was deleted.
    #[error("the helper says {reported:?}, which does not name version {expected}")]
    VersionMismatch {
        /// The expected version.
        expected: String,
        /// The first line of its output.
        reported: String,
    },
    /// `current` could not be switched.
    #[error("could not switch to the new version: {0}")]
    SwitchFailed(String),
    /// tmux is missing or too old for the tmux launcher.
    #[error("tmux: {0}")]
    Tmux(String),
    /// Nothing is deployed to start.
    #[error("the helper is not deployed ({0})")]
    NotDeployed(String),
    /// The helper is recorded as running on another host sharing this home directory (another
    /// login node, say). It cannot be checked or stopped from here; see
    /// [`LaunchOptions::take_over`].
    #[error("the helper is recorded on {0}, another host sharing this home")]
    OtherHost(String),
    /// The helper exited at once, or did not open its socket in time (it was then stopped).
    #[error("the helper did not start: {0}")]
    StartFailed(String),
    /// The helper outlived SIGKILL.
    #[error("the helper did not stop: {0}")]
    StopFailed(String),
    /// Another failure the script reported, as `<code>: <detail>`.
    #[error("{0}")]
    Remote(String),
    /// The machine answered with something that is not a report, or not the expected one.
    #[error("unexpected output: {0}")]
    UnexpectedOutput(String),
}

/// Where PitCrew lives on a machine: `<root>/bin`, `<root>/run`. Normally `~/.pitcrew`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    root: String,
}

impl Layout {
    /// `<home>/.pitcrew`.
    ///
    /// # Errors
    /// [`HelperError::InvalidArgument`] for a home that is not an absolute path, or has a
    /// control character or a `.`/`..` component.
    pub fn in_home(home: &str) -> Result<Self, HelperError> {
        let home = home.trim_end_matches('/');
        Self::at(&format!("{home}/.pitcrew"))
    }

    /// Another root, for instance on a site where `$HOME` is too small (and for tests).
    ///
    /// # Errors
    /// As [`Layout::in_home`].
    pub fn at(root: &str) -> Result<Self, HelperError> {
        let invalid = |why: &str| {
            Err(HelperError::InvalidArgument(format!(
                "the remote directory {root:?} {why}"
            )))
        };
        let root = root.trim_end_matches('/');
        if !root.starts_with('/') {
            return invalid("is not an absolute path");
        }
        if root.chars().any(char::is_control) {
            return invalid("has a control character");
        }
        if root.split('/').any(|part| part == "." || part == "..") {
            return invalid("has a . or .. component");
        }
        Ok(Self {
            root: root.to_owned(),
        })
    }

    /// The root, e.g. `/home/someone/.pitcrew`.
    #[must_use]
    pub fn root(&self) -> &str {
        &self.root
    }

    /// Where helper versions live.
    #[must_use]
    pub fn bin_dir(&self) -> String {
        format!("{}/bin", self.root)
    }

    /// One version's helper.
    #[must_use]
    pub fn binary(&self, version: &str) -> String {
        format!("{}/bin/{version}/pitcrewd", self.root)
    }

    /// The helper launchers run: `bin/current/pitcrewd`.
    #[must_use]
    pub fn current_binary(&self) -> String {
        format!("{}/bin/current/pitcrewd", self.root)
    }

    /// Runtime files: the endpoint, socket, log and pid file.
    #[must_use]
    pub fn run_dir(&self) -> String {
        format!("{}/run", self.root)
    }

    /// `run/endpoint.json`.
    #[must_use]
    pub fn endpoint(&self) -> String {
        format!("{}/run/endpoint.json", self.root)
    }

    /// `run/pitcrewd.sock`, where the default launch arguments put the helper's socket.
    #[must_use]
    pub fn socket(&self) -> String {
        format!("{}/run/pitcrewd.sock", self.root)
    }

    /// `run/pitcrewd.log`, the helper's standard output and error.
    #[must_use]
    pub fn log(&self) -> String {
        format!("{}/run/pitcrewd.log", self.root)
    }
}

/// A machine to deploy to: how to reach it, where PitCrew lives there, and its platform.
#[derive(Clone, Debug)]
pub struct Target {
    ssh: Ssh,
    host: String,
    layout: Layout,
    platform: Platform,
}

impl Target {
    /// The machine `host` as `probe` found it: `~/.pitcrew` in its home, its platform.
    ///
    /// # Errors
    /// [`HelperError::NoHome`], [`HelperError::UnsupportedPlatform`], or an invalid host or
    /// home.
    pub fn new(ssh: Ssh, host: impl Into<String>, probe: &Probe) -> Result<Self, HelperError> {
        let home = probe.home.as_deref().ok_or(HelperError::NoHome)?;
        let platform = Platform::detect(&probe.info)?;
        Self::with_layout(ssh, host, Layout::in_home(home)?, platform)
    }

    /// A machine whose layout and platform are already known.
    ///
    /// # Errors
    /// [`SshError::InvalidHost`] for a host name ssh would not get.
    pub fn with_layout(
        ssh: Ssh,
        host: impl Into<String>,
        layout: Layout,
        platform: Platform,
    ) -> Result<Self, HelperError> {
        let host = host.into();
        crate::quote::validate_host(&host)?;
        Ok(Self {
            ssh,
            host,
            layout,
            platform,
        })
    }

    /// How the machine is reached.
    #[must_use]
    pub fn ssh(&self) -> &Ssh {
        &self.ssh
    }

    /// The host name given to ssh.
    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }

    /// Where PitCrew lives on it.
    #[must_use]
    pub fn layout(&self) -> &Layout {
        &self.layout
    }

    /// Its platform.
    #[must_use]
    pub fn platform(&self) -> Platform {
        self.platform
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layouts() {
        let layout = Layout::in_home("/home/someone/").unwrap();
        assert_eq!(layout.root(), "/home/someone/.pitcrew");
        assert_eq!(layout.bin_dir(), "/home/someone/.pitcrew/bin");
        assert_eq!(
            layout.binary("1.2.3"),
            "/home/someone/.pitcrew/bin/1.2.3/pitcrewd"
        );
        assert_eq!(
            layout.current_binary(),
            "/home/someone/.pitcrew/bin/current/pitcrewd"
        );
        assert_eq!(
            layout.endpoint(),
            "/home/someone/.pitcrew/run/endpoint.json"
        );
        assert_eq!(layout.socket(), "/home/someone/.pitcrew/run/pitcrewd.sock");
        assert_eq!(Layout::in_home("/").unwrap().root(), "/.pitcrew");
        assert_eq!(Layout::at("/srv/pc//").unwrap().root(), "/srv/pc");
        for bad in ["relative", "", "/a/../b", "/a/./b", "/a\nb", "/a\u{7f}"] {
            assert!(Layout::at(bad).is_err(), "{bad:?}");
        }
        assert!(Layout::in_home("~").is_err());
    }
}
