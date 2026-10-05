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
//! bin/current -> <version>   the version launchers start (relative, so it works wherever
//!                            $HOME is mounted); bin/previous names the one before
//! bin/.lock/                 deploy lock (mkdir), with an `owner` file: host, pid, call tag, time
//! run/endpoint.json          the running helper: pid, host, version, started, launcher, socket
//!                            (and job, for the SLURM launcher)
//! run/pitcrewd.sock          its socket; run/pitcrewd.log its output; run/.lock the launch lock
//! run/slurm.json             the batch job the SLURM launcher submitted; run/slurm-<id>.out
//!                            its output
//! ```
//!
//! **The way there** is checked first, as sshd checks the way to `authorized_keys`: every
//! directory from `/` down to the root's parent (following symbolic links, and checking where
//! they lead) must belong to root or the user, and be writable by no one else unless sticky, as
//! `/tmp`. Otherwise someone else could rename the tree after the checks and have their own
//! `pitcrewd` run. The script then works from inside the root (`cd -P`) with relative paths, and
//! a launched helper checks the directory it starts in.
//!
//! **The script** travels on ssh's stdin, not on the command line: Windows limits a whole
//! command line to 32,767 characters, and the script is larger than the quarter of that the
//! shell-neutral wrapper leaves. The command line only carries a fixed bootstrap, run by
//! `/bin/sh`, that puts the tool path ([`DEFAULT_TOOL_PATH`]) in front of `PATH` and reads the
//! script's exact length with `dd` (so the helper bytes behind it stay on stdin for the upload).
//! It runs the script only with its first and last lines and its length intact. Each call prints
//! a report between markers carrying a random tag, as the probe does.
//!
//! **Locks** are `mkdir` directories with an owner line: host, pid, call tag, and time. One
//! taken on this host is stale when its process is gone or it is older than the limit by this
//! host's clock; one taken on another host (a login node sharing the home) only when its
//! directory is older than the limit plus 10 minutes, the clock skew tolerated between hosts and
//! the file server. A stale lock is moved aside atomically; a holder checks that it still owns
//! its lock before every change, and stops ([`HelperError::LockLost`]) if not.

mod deploy;
mod launch;
pub(crate) mod script;
pub mod slurm;

pub use deploy::{
    DeployOptions, DeployStep, Deployed, HashTool, Helper, MAX_HELPER_SIZE, Platform, Progress,
    deploy, validate_version,
};
pub use launch::{
    DirectLauncher, Endpoint, HelperFuture, HelperState, LaunchOptions, Launcher, MIN_TMUX,
    Started, Status, Stopped, TmuxLauncher, parse_tmux_version, tmux_name,
};
pub use slurm::SlurmLauncher;

use crate::probe::Probe;
use crate::{Ssh, SshError};

/// Where the script looks for tools first, before the user's `PATH`: the system's own, not a
/// look-alike from a conda environment or a module.
pub const DEFAULT_TOOL_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";

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
    /// The helper is recorded as running on another host sharing this home directory: another
    /// login node, say, or a compute node for a SLURM job. It cannot be checked or stopped from
    /// here: stop it there (a SLURM job with the SLURM launcher), or, once that host is known to
    /// be gone, forget the record with a direct launcher's stop under
    /// [`LaunchOptions::take_over`]. A host is told apart by its name: a record of this host
    /// made before its id changed (as at a reboot of a stateless node) is this host's.
    #[error(
        "the {launcher} launcher's helper is recorded on {host}, another host sharing this home"
    )]
    OtherHost {
        /// The host it is recorded on.
        host: String,
        /// The launcher that started it: `direct`, `tmux` or `slurm`.
        launcher: String,
    },
    /// The helper exited at once, or did not open its socket in time (it was then stopped).
    #[error("the helper did not start: {0}")]
    StartFailed(String),
    /// The helper outlived SIGKILL, or its batch job did not leave the queue in time.
    #[error("the helper did not stop: {0}")]
    StopFailed(String),
    /// Another launcher's helper uses this root (and its socket), here or on another host: stop
    /// it first.
    #[error("in use: {0}")]
    InUse(String),
    /// A SLURM command is missing, or failed: squeue could not say what the job is doing
    /// (the scheduler may be unreachable), so nothing was concluded or changed.
    #[error("SLURM: {0}")]
    Slurm(String),
    /// sbatch refused the job; its message says why (an unknown account, say).
    #[error("sbatch refused the job: {0}")]
    SubmitFailed(String),
    /// The helper's batch job is queued but has not started within the wait. It stays
    /// queued; starting again waits for the same job.
    #[error("the helper's job {job} has not started yet: {state}")]
    Queued {
        /// The job's id.
        job: u64,
        /// What the scheduler says, e.g. `job 4242 pending (Priority)`.
        state: String,
    },
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
    tool_path: String,
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
        ssh.validate_destination(&host)?;
        Ok(Self {
            ssh,
            host,
            layout,
            platform,
            tool_path: DEFAULT_TOOL_PATH.to_owned(),
        })
    }

    /// Looks for tools in `path` (`:`-separated absolute directories) before the user's `PATH`,
    /// instead of [`DEFAULT_TOOL_PATH`]: for a site whose tools live elsewhere.
    ///
    /// On macOS the scripts read modes and access control lists with `/bin/ls`, by its path,
    /// whatever `ls` this path finds first: GNU `ls` cannot list ACLs (it has no `-e`), so every
    /// deploy would be refused (every home folder has an ACL), and uutils' or busybox's `ls`
    /// shows no `+`, so an ACL on the way to the root would go unjudged.
    ///
    /// # Errors
    /// [`HelperError::InvalidArgument`] for an empty path, a relative directory, or a control
    /// character.
    pub fn with_tool_path(mut self, path: &str) -> Result<Self, HelperError> {
        let ok = !path.is_empty()
            && !path.chars().any(char::is_control)
            && path.split(':').all(|dir| dir.starts_with('/'));
        if !ok {
            return Err(HelperError::InvalidArgument(format!(
                "the tool path {:?} must be absolute directories separated by ':'",
                script::clean(path)
            )));
        }
        self.tool_path = path.to_owned();
        Ok(self)
    }

    /// Where the script looks for tools first.
    #[must_use]
    pub fn tool_path(&self) -> &str {
        &self.tool_path
    }

    /// How the machine is reached.
    #[must_use]
    pub fn ssh(&self) -> &Ssh {
        &self.ssh
    }

    /// The same machine, reached with `ssh` (the tunnel's calls through its own connection).
    pub(crate) fn with_ssh(&self, ssh: Ssh) -> Self {
        Self {
            ssh,
            ..self.clone()
        }
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

    #[test]
    fn tool_paths() {
        let target = Target::with_layout(
            Ssh::new("ssh"),
            "box",
            Layout::in_home("/home/someone").unwrap(),
            Platform::LinuxX86_64,
        )
        .unwrap();
        assert_eq!(target.tool_path(), DEFAULT_TOOL_PATH);
        let target = target.with_tool_path("/opt/tools/bin:/usr/bin").unwrap();
        assert_eq!(target.tool_path(), "/opt/tools/bin:/usr/bin");
        for bad in [
            "",
            "bin",
            "/usr/bin:",
            "/usr/bin::/bin",
            "/usr/bin:.",
            "/a\nb",
        ] {
            assert!(target.clone().with_tool_path(bad).is_err(), "{bad:?}");
        }
    }
}
