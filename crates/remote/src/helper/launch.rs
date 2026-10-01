//! Starting, checking and stopping the deployed helper: the [`Launcher`] trait, with the
//! `direct` and `tmux` launchers.
//!
//! A launcher runs `bin/<version>/pitcrewd <args>`, the version `bin/current` points to (by
//! default `serve --listen unix:<root>/run/pitcrewd.sock`), from inside the root, with its
//! output appended to `run/pitcrewd.log` and the user's own umask (the script's 077 is for its
//! own files). It waits for the socket, and writes `run/endpoint.json` atomically (a temporary
//! file renamed over it):
//!
//! ```json
//! {"pid":4242,"host":"login01","version":"1.4.0","started":1790850391000,"launcher":"direct","socket":"/home/someone/.pitcrew/run/pitcrewd.sock"}
//! ```
//!
//! `host` is `uname -n`: on clusters whose login nodes share `$HOME`, a pid means something
//! only on the host that recorded it. A record from another host is never acted on from here
//! ([`HelperError::OtherHost`]) unless [`LaunchOptions::take_over`] says so.
//!
//! Every operation is idempotent: starting a running helper returns its endpoint; stopping a
//! stopped one does nothing. Start and stop take the launch lock (`run/.lock`); status takes
//! none and changes nothing. A process counts as the helper only while it is alive (not a
//! zombie) and named `pitcrewd`, and `stop` signals it only while its start time is the one it
//! had, so a recycled pid is never signalled.
//!
//! The SLURM launcher (a later brief) implements the same trait: its `status` will ask the
//! scheduler, and its endpoint will name the compute node.

use super::script::{self, Call, Report};
use super::{HelperError, Target};
use crate::helper::deploy::{minutes, seconds};
use pitcrew_protocol::model::TimestampMs;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

/// The tmux launcher's server socket (`tmux -L`) and session name for a layout:
/// `pitcrew-helper-` and 8 hex digits of the root's sha256. Apart from the user's own tmux, and
/// from another root's helper on the same host.
#[must_use]
pub fn tmux_name(layout: &super::Layout) -> String {
    use sha2::{Digest as _, Sha256};
    let hash = Sha256::digest(layout.root().as_bytes());
    format!("pitcrew-helper-{}", crate::askpass::to_hex(&hash[..4]))
}

/// The oldest tmux the tmux launcher accepts, as stream B's runtime requires.
pub const MIN_TMUX: (u32, u32) = (3, 2);

/// Longest socket path: `sun_path` holds 104 bytes on macOS and 108 on Linux.
const MAX_SOCKET_PATH: usize = 100;

/// What a [`Launcher`] call returns.
pub type HelperFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, HelperError>> + Send + 'a>>;

/// Starts and stops the helper on a machine. Object-safe, so the launcher chosen at setup can
/// be kept as a `Box<dyn Launcher>`.
pub trait Launcher: fmt::Debug + Send + Sync {
    /// Its name, as recorded in `endpoint.json`: `direct`, `tmux`, …
    fn name(&self) -> &'static str;

    /// Starts the helper unless it already runs, and returns its endpoint.
    fn start<'a>(&'a self, target: &'a Target) -> HelperFuture<'a, Started>;

    /// Whether the helper runs, and what is installed.
    fn status<'a>(&'a self, target: &'a Target) -> HelperFuture<'a, Status>;

    /// Stops the helper if it runs, and removes its records.
    fn stop<'a>(&'a self, target: &'a Target) -> HelperFuture<'a, Stopped>;
}

/// Options shared by the launchers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchOptions {
    /// The helper's arguments. `None`: `serve --listen unix:<socket>`, with the layout's
    /// socket. Whatever they are, `start` waits for that socket.
    pub args: Option<Vec<String>>,
    /// How long `start` waits for the socket before it stops the helper and fails. Default
    /// 15 s.
    pub ready_timeout: Duration,
    /// How long `stop` waits after SIGTERM before SIGKILL. Default 10 s.
    pub stop_timeout: Duration,
    /// How long to wait for another start or stop. Default 30 s.
    pub lock_wait: Duration,
    /// When the launch lock counts as stale (whole minutes); longer than a start or stop can
    /// take. Default 5 minutes.
    pub stale_lock: Duration,
    /// Treat a helper recorded on another host as gone: start one here, or forget it on stop.
    /// It may still run there (with its socket in a shared home, now replaced), so set this
    /// only when the user confirms, for instance after that host was retired.
    pub take_over: bool,
}

impl Default for LaunchOptions {
    fn default() -> Self {
        Self {
            args: None,
            ready_timeout: Duration::from_secs(15),
            stop_timeout: Duration::from_secs(10),
            lock_wait: Duration::from_secs(30),
            stale_lock: Duration::from_secs(5 * 60),
            take_over: false,
        }
    }
}

impl LaunchOptions {
    fn check(&self) -> Result<(), HelperError> {
        let longest = seconds(self.lock_wait)
            + seconds(self.ready_timeout).max(seconds(self.stop_timeout) + 5);
        if minutes(self.stale_lock) * 60 <= longest {
            return Err(HelperError::InvalidArgument(
                "stale_lock must be longer than lock_wait plus the ready or stop timeout"
                    .to_owned(),
            ));
        }
        Ok(())
    }

    /// The call's own bound: waiting for the lock, the work, and some slack for ssh.
    fn call_timeout(&self, work: Duration) -> Duration {
        self.lock_wait
            .saturating_add(work)
            .saturating_add(Duration::from_secs(30))
    }
}

/// `run/endpoint.json`: how to reach the running helper.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Endpoint {
    /// Its process id, on `host`.
    pub pid: u32,
    /// The host it runs on (`uname -n`).
    pub host: String,
    /// The version started (what `bin/current` pointed to).
    pub version: String,
    /// When it was started: UTC milliseconds, to the second (`date +%s` on the machine).
    pub started: TimestampMs,
    /// The launcher that started it.
    pub launcher: String,
    /// Its socket.
    pub socket: String,
}

/// What [`Launcher::start`] did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Started {
    /// The running helper.
    pub endpoint: Endpoint,
    /// False when it was already running.
    pub started_now: bool,
}

/// Whether the recorded helper runs.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum HelperState {
    /// It runs on this host: its pid is alive and named `pitcrewd`.
    Running,
    /// Nothing is recorded, or the recorded process is gone.
    NotRunning,
    /// It was recorded on another host sharing this home, and cannot be checked from here.
    OtherHost(String),
}

/// What [`Launcher::status`] found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Status {
    /// Whether it runs.
    pub state: HelperState,
    /// What `endpoint.json` records, if anything.
    pub endpoint: Option<Endpoint>,
    /// The version `bin/current` points to, if any.
    pub installed: Option<String>,
    /// Whether the layout's socket exists.
    pub socket_ready: bool,
    /// For the tmux launcher: whether its session exists.
    pub tmux_session: Option<bool>,
}

impl Status {
    /// Whether the helper runs on this host.
    #[must_use]
    pub fn running(&self) -> bool {
        self.state == HelperState::Running
    }

    /// The version of the running helper.
    #[must_use]
    pub fn running_version(&self) -> Option<&str> {
        self.endpoint
            .as_ref()
            .filter(|_| self.running())
            .map(|e| e.version.as_str())
    }
}

/// What [`Launcher::stop`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stopped {
    /// The process stopped; `None` when none was running.
    pub pid: Option<u32>,
    /// Whether it needed SIGKILL.
    pub forced: bool,
}

/// `setsid nohup pitcrewd … &`: the helper in its own session, detached from the ssh call
/// (`nohup` alone where there is no `setsid`, as on macOS).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DirectLauncher {
    options: LaunchOptions,
}

impl DirectLauncher {
    /// A direct launcher with `options`.
    #[must_use]
    pub fn new(options: LaunchOptions) -> Self {
        Self { options }
    }
}

impl Launcher for DirectLauncher {
    fn name(&self) -> &'static str {
        "direct"
    }

    fn start<'a>(&'a self, target: &'a Target) -> HelperFuture<'a, Started> {
        Box::pin(start(self.name(), &self.options, target))
    }

    fn status<'a>(&'a self, target: &'a Target) -> HelperFuture<'a, Status> {
        Box::pin(status(self.name(), &self.options, target))
    }

    fn stop<'a>(&'a self, target: &'a Target) -> HelperFuture<'a, Stopped> {
        Box::pin(stop(self.name(), &self.options, target))
    }
}

/// The helper as the only window of a dedicated tmux session on its own tmux server, both
/// named [`tmux_name`] (started without the user's config), so neither the user's tmux nor
/// another root's helper touches it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TmuxLauncher {
    options: LaunchOptions,
}

impl TmuxLauncher {
    /// A tmux launcher for a machine whose `tmux -V` printed `version` (the probe's
    /// `tmux_version`).
    ///
    /// # Errors
    /// [`HelperError::Tmux`] when tmux is missing, older than [`MIN_TMUX`], or its version
    /// cannot be read.
    pub fn new(version: Option<&str>, options: LaunchOptions) -> Result<Self, HelperError> {
        let Some(version) = version else {
            return Err(HelperError::Tmux("tmux is not installed".to_owned()));
        };
        let (major, minor) = MIN_TMUX;
        match parse_tmux_version(version) {
            Some(found) if found >= MIN_TMUX => Ok(Self { options }),
            Some(_) => Err(HelperError::Tmux(format!(
                "tmux {} is older than {major}.{minor}, which PitCrew needs",
                script::clean(version)
            ))),
            None => Err(HelperError::Tmux(format!(
                "cannot tell whether tmux {:?} is {major}.{minor} or newer",
                script::clean(version)
            ))),
        }
    }
}

impl Launcher for TmuxLauncher {
    fn name(&self) -> &'static str {
        "tmux"
    }

    fn start<'a>(&'a self, target: &'a Target) -> HelperFuture<'a, Started> {
        Box::pin(start(self.name(), &self.options, target))
    }

    fn status<'a>(&'a self, target: &'a Target) -> HelperFuture<'a, Status> {
        Box::pin(status(self.name(), &self.options, target))
    }

    fn stop<'a>(&'a self, target: &'a Target) -> HelperFuture<'a, Stopped> {
        Box::pin(stop(self.name(), &self.options, target))
    }
}

/// `tmux -V`'s version as (major, minor): `3.2a` is (3, 2), `next-3.5` is (3, 5). `None` for
/// what it cannot read (`master`, `openbsd-7.4`).
#[must_use]
pub fn parse_tmux_version(version: &str) -> Option<(u32, u32)> {
    let version = version.trim();
    let version = version.strip_prefix("tmux ").unwrap_or(version);
    let version = version.strip_prefix("next-").unwrap_or(version);
    let (major, rest) = version.split_once('.')?;
    let minor: String = rest.chars().take_while(char::is_ascii_digit).collect();
    Some((major.parse().ok()?, minor.parse().ok()?))
}

fn flag(on: bool) -> String {
    if on { "1" } else { "0" }.to_owned()
}

async fn start(
    launcher: &'static str,
    options: &LaunchOptions,
    target: &Target,
) -> Result<Started, HelperError> {
    options.check()?;
    let socket = target.layout().socket();
    if socket.len() > MAX_SOCKET_PATH {
        return Err(HelperError::InvalidArgument(format!(
            "the socket path {socket:?} is longer than {MAX_SOCKET_PATH} bytes; use a shorter \
             layout root"
        )));
    }
    let socket_json = serde_json::to_string(&socket)
        .map_err(|e| HelperError::InvalidArgument(format!("the socket path: {e}")))?;
    let mut args = vec![
        launcher.to_owned(),
        seconds(options.ready_timeout).to_string(),
        seconds(options.lock_wait).to_string(),
        minutes(options.stale_lock).to_string(),
        flag(options.take_over),
        socket.clone(),
        socket_json,
        tmux_name(target.layout()),
    ];
    match &options.args {
        Some(custom) => args.extend(custom.iter().cloned()),
        None => args.extend([
            "serve".to_owned(),
            "--listen".to_owned(),
            format!("unix:{socket}"),
        ]),
    }
    let report = script::run(
        target,
        Call {
            command: "start",
            args,
            payload: None,
            progress: None,
            timeout: options.call_timeout(options.ready_timeout),
        },
    )
    .await?;
    let endpoint = endpoint(&report)?.ok_or_else(|| {
        HelperError::UnexpectedOutput("the start reported no endpoint".to_owned())
    })?;
    Ok(Started {
        endpoint,
        started_now: report.get("started") == Some("1"),
    })
}

async fn status(
    launcher: &'static str,
    options: &LaunchOptions,
    target: &Target,
) -> Result<Status, HelperError> {
    let report = script::run(
        target,
        Call {
            command: "status",
            args: vec![
                launcher.to_owned(),
                target.layout().socket(),
                tmux_name(target.layout()),
            ],
            payload: None,
            progress: None,
            timeout: options.call_timeout(Duration::ZERO),
        },
    )
    .await?;
    let endpoint = endpoint(&report)?;
    let state = match report.get("state") {
        Some("running") => HelperState::Running,
        Some("stopped") => HelperState::NotRunning,
        Some("elsewhere") => HelperState::OtherHost(
            endpoint
                .as_ref()
                .map(|e| e.host.clone())
                .unwrap_or_default(),
        ),
        other => {
            return Err(HelperError::UnexpectedOutput(format!(
                "the status reported state {other:?}"
            )));
        }
    };
    Ok(Status {
        state,
        endpoint,
        installed: report.get("installed").map(str::to_owned),
        socket_ready: report.get("socket") == Some("1"),
        tmux_session: report.get("session").map(|s| s == "1"),
    })
}

async fn stop(
    launcher: &'static str,
    options: &LaunchOptions,
    target: &Target,
) -> Result<Stopped, HelperError> {
    options.check()?;
    let report = script::run(
        target,
        Call {
            command: "stop",
            args: vec![
                launcher.to_owned(),
                seconds(options.lock_wait).to_string(),
                minutes(options.stale_lock).to_string(),
                flag(options.take_over),
                seconds(options.stop_timeout).to_string(),
                target.layout().socket(),
                tmux_name(target.layout()),
            ],
            payload: None,
            progress: None,
            timeout: options.call_timeout(options.stop_timeout.saturating_mul(2)),
        },
    )
    .await?;
    let pid = match report.get("pid") {
        Some(pid) => Some(pid.parse().map_err(|_| {
            HelperError::UnexpectedOutput(format!("the stop reported pid {pid:?}"))
        })?),
        None => None,
    };
    Ok(Stopped {
        pid,
        forced: report.get("forced") == Some("1"),
    })
}

/// The endpoint in a report, if there is one. A record the script could not read counts as
/// none; one it read but that does not parse is an error.
fn endpoint(report: &Report) -> Result<Option<Endpoint>, HelperError> {
    let Some(line) = report.get("endpoint") else {
        return Ok(None);
    };
    match report.get("state") {
        Some("stopped") => Ok(serde_json::from_str(line).ok()),
        _ => serde_json::from_str(line).map(Some).map_err(|e| {
            HelperError::UnexpectedOutput(format!("endpoint.json does not parse: {e}"))
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tmux_names_follow_the_root() {
        let a = super::super::Layout::in_home("/home/someone").unwrap();
        let b = super::super::Layout::at("/project/group/someone/.pitcrew").unwrap();
        let name = tmux_name(&a);
        assert!(name.starts_with("pitcrew-helper-"), "{name}");
        assert_eq!(name.len(), "pitcrew-helper-".len() + 8);
        assert_eq!(name, tmux_name(&a));
        assert_ne!(name, tmux_name(&b));
    }

    #[test]
    fn tmux_versions() {
        let cases = [
            ("3.2", Some((3, 2))),
            ("3.2a", Some((3, 2))),
            ("3.3a", Some((3, 3))),
            ("3.10", Some((3, 10))),
            ("next-3.5", Some((3, 5))),
            ("tmux 3.4", Some((3, 4))),
            ("4.0", Some((4, 0))),
            ("3.1c", Some((3, 1))),
            ("2.7", Some((2, 7))),
            ("master", None),
            ("openbsd-7.4", None),
            ("", None),
        ];
        for (text, want) in cases {
            assert_eq!(parse_tmux_version(text), want, "{text:?}");
        }
        let options = LaunchOptions::default();
        for ok in ["3.2", "3.2a", "3.3a", "next-3.5", "3.10"] {
            TmuxLauncher::new(Some(ok), options.clone()).unwrap();
        }
        for bad in [
            None,
            Some("3.1c"),
            Some("2.7"),
            Some("master"),
            Some("x\u{1b}"),
        ] {
            let err = TmuxLauncher::new(bad, options.clone()).unwrap_err();
            assert!(matches!(&err, HelperError::Tmux(_)), "{bad:?}");
            assert!(!err.to_string().contains('\u{1b}'));
        }
    }

    #[test]
    fn endpoints_round_trip() {
        let line = r#"{"pid":4242,"host":"login01","version":"1.4.0","started":1790850391000,"launcher":"direct","socket":"/home/a \"b\"/.pitcrew/run/pitcrewd.sock"}"#;
        let endpoint: Endpoint = serde_json::from_str(line).unwrap();
        assert_eq!(endpoint.pid, 4242);
        assert_eq!(endpoint.socket, "/home/a \"b\"/.pitcrew/run/pitcrewd.sock");
        assert_eq!(serde_json::to_string(&endpoint).unwrap(), line);
    }

    #[test]
    fn options_are_checked() {
        LaunchOptions::default().check().unwrap();
        let tight = LaunchOptions {
            lock_wait: Duration::from_secs(60),
            stale_lock: Duration::from_secs(60),
            ..LaunchOptions::default()
        };
        assert!(tight.check().is_err());
    }

    /// The trait stays object-safe, and its futures can be spawned.
    #[test]
    fn launchers_are_objects() {
        fn spawnable<T: Send>(_: &T) {}
        let launchers: Vec<Box<dyn Launcher>> = vec![
            Box::new(DirectLauncher::default()),
            Box::new(TmuxLauncher::new(Some("3.3a"), LaunchOptions::default()).unwrap()),
        ];
        let target = Target::with_layout(
            crate::Ssh::new("ssh"),
            "box",
            super::super::Layout::in_home("/home/someone").unwrap(),
            super::super::Platform::LinuxX86_64,
        )
        .unwrap();
        for launcher in &launchers {
            spawnable(&launcher.status(&target));
        }
        assert_eq!(
            launchers.iter().map(|l| l.name()).collect::<Vec<_>>(),
            ["direct", "tmux"]
        );
    }
}
