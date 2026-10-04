//! Remote workspaces (`docs/build/contracts/desktop-gateway.md`, "Remote workspaces" and
//! "Prompts"): a hub on another machine (a server, an HPC login node, a SLURM compute node),
//! added from the app and then used like the local one, over the person's own OpenSSH
//! (`pitcrew-remote`, stream J).
//!
//! Adding takes two steps, so nothing changes on the machine until the person has seen what will
//! happen:
//! 1. [`Remotes::plan`] probes the machine and works out what adding does: the helper to copy
//!    there, how it starts, and for SLURM the exact job script. It changes nothing. The plan is
//!    kept under an opaque id for 10 minutes ([`plan`]).
//! 2. [`Remotes::add`] carries out that plan, once: it deploys the helper (checked against its
//!    sha256 here and on the machine), starts it (submitting exactly the shown script for
//!    SLURM), connects through the tunnel, and pairs: it reads the hub's device token over SSH
//!    (from the file `pitcrewd token show-path` names, between random markers), checks it by
//!    asking the hub which workspace it hosts, and registers the workspace with its token in the
//!    OS keychain. If a step fails after the helper was started (or its job submitted) by this
//!    add, that is stopped again; if stopping fails too, the error says what may be left.
//!    [`Remotes::cancel`] stops a running add the same way. Taking the plan and recording the
//!    add's cancel are one step, so a cancel is never lost between them.
//!
//! **The hub's answer is not trusted.** Its workspace id is claimed in the registry in one step
//! that refuses an id already held by the local workspace, or by a remote one on another machine
//! (another host or root): a hostile hub cannot take over another workspace's entry or token.
//! Only the same machine may be paired again. Its name is cleaned and cut, at pairing and each
//! time the tunnel connects again.
//!
//! **The token** goes from ssh's output into a [`DeviceToken`] and the keychain, and from there
//! only into the `Authorization` header or the WebSocket subprotocol of a request to that hub.
//! It is never in a result, an event, an error, a log line or a file of ours.
//!
//! **Prompts** (passwords, passphrases, one-time codes, host keys, other yes/no questions and
//! notices) from any of these calls, and from the tunnel reconnecting, go through
//! `pitcrew-askpass` to the [`PromptHub`] ([`prompt`]), behind the ssh version gate ([`gate`]):
//! only an ssh that said 8.4 or newer gets prompts; any other runs in `BatchMode` and never
//! asks. A missing `pitcrew-askpass` (or a configured `ssh` that fails its checks) is a clear
//! error before any ssh call.
//!
//! **Afterwards** each remote workspace has a [`link::Link`]: the tunnel's `Connector`, which
//! reconnects by itself, and a task keeping the workspace's state in step with it. At start,
//! [`Remotes::resume`] makes them again for the workspaces saved in the registry; a computer that
//! slept is noticed by a timer that fires late ([`Remotes::watch_wakes`]), and every tunnel is
//! told to check at once. A connection that gave up (a sign-in cancelled while reconnecting)
//! starts over at [`Remotes::retry`], with retries coalesced. A link is put in only while its
//! workspace is still a registered remote one and the app is not quitting; a remove's token,
//! entry and link change under the same lock as a pairing's claim, token and link, so a retry or
//! a pairing racing a remove leaves nothing behind, and loses nothing.

pub mod gate;
pub mod helpers;
pub mod link;
pub mod plan;
pub mod prompt;

pub use gate::{GatedPrompts, SshCheck, SshVersions, Verdict};
pub use helpers::{HelperRef, Helpers};
pub use plan::{RemotePlan, RemotePlanRequest};
pub use prompt::{GatewayPrompt, PromptEvent, PromptHub};

use crate::gateway::{Connector, GatewayError};
use crate::keychain::TokenStore;
use crate::registry::{
    Connection, GatewayWorkspace, HopKind, JobRequest, LauncherKind, Registry, RemoteConnection,
    WorkspaceKind, WorkspaceRecord, WorkspaceState,
};
use crate::token::DeviceToken;
use link::{Ended, Follow, Link, Outcome, Pairing, RemoteConnector, Tunnel};
use pitcrew_protocol::machine_setup::MachineCheck;
use pitcrew_remote::helper::slurm::{Site, check_tools, generic, load_sites, sites_dir};
use pitcrew_remote::helper::tmux_name;
use pitcrew_remote::{
    ConnectorOptions, Daemon, DeployOptions, DeployStep, DirectLauncher, HelperError, JobScript,
    JobSpec, JobState, LastHop, LaunchOptions, Launcher, Layout, Limits, LinkState, Platform,
    PromptHandler, SiteRecipe as _, SlurmLauncher, Ssh, SshError, Target, TmuxLauncher, Transport,
    Unreachable,
};
use plan::PlanStore;
use serde::Serialize;
use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use tokio::sync::watch;

/// The step name of the whole add, in its last progress event.
pub const ADD_STEP: &str = "add";

/// How long undoing a failed add may take (stopping the helper it started).
const UNDO_WAIT: Duration = Duration::from_secs(90);

/// How often the wake timer ticks, and how late a tick must be to mean the computer slept.
const WAKE_TICK: Duration = Duration::from_secs(5);
const WAKE_LATE: Duration = Duration::from_secs(10);

/// What reading the token over SSH may print, and take.
const TOKEN_LIMITS: Limits = Limits {
    max_output: Some(8 * 1024),
    timeout: Some(Duration::from_secs(60)),
};

/// Prints the hub's device token, the file `pitcrewd token show-path` names, between the lines
/// `$2` and `$3`. `$1` is the helper.
const READ_TOKEN: &str = "p=$(\"$1\" token show-path) || exit 3\n[ -f \"$p\" ] || exit 4\n\
                          printf '%s\\n' \"$2\"\ncat -- \"$p\" || exit 5\nprintf '\\n%s\\n' \"$3\"";

/// How the app reaches remote machines.
#[derive(Clone)]
pub struct RemoteOptions {
    /// The ssh program: `ssh` on `PATH` unless the settings name one; or why the configured one
    /// is not used.
    pub ssh: Result<PathBuf, String>,
    /// wsl.exe: `%SystemRoot%\System32\wsl.exe` ([`pitcrew_remote::wsl::default_program`]);
    /// tests name a stand-in.
    pub wsl: PathBuf,
    /// `pitcrew-askpass`, or why it is missing.
    pub askpass: Result<PathBuf, String>,
    /// The helper binaries.
    pub helpers: Helpers,
    /// Where ssh's control sockets, askpass sockets and logs go; `None`: `pitcrew-remote`'s
    /// default.
    pub runtime_dir: Option<PathBuf>,
    /// Connection reuse (a ControlMaster); `None`: on, on Unix.
    pub multiplex: Option<bool>,
    /// The person's ssh config, for the host list and the tunnel's node check; `None`:
    /// `~/.ssh/config`.
    pub ssh_config: Option<PathBuf>,
    /// Their home, for the config's `~` and relative `Include`s; `None`: `HOME`.
    pub home: Option<PathBuf>,
    /// Where their SLURM site recipes are; `None`: `~/.pitcrew/sites`.
    pub sites_dir: Option<PathBuf>,
    /// How long a plan lasts.
    pub plan_ttl: Duration,
    /// How long adding waits for a SLURM job to start.
    pub job_wait: Duration,
    /// How often adding asks the scheduler meanwhile.
    pub job_poll: Duration,
    /// How long adding waits for the tunnel to connect.
    pub connect_wait: Duration,
    /// The tunnel's options; each workspace's remembered transport goes into them.
    pub connector: ConnectorOptions,
}

impl fmt::Debug for RemoteOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RemoteOptions")
            .field("ssh", &self.ssh)
            .field("askpass", &self.askpass)
            .field("helpers", &self.helpers)
            .finish_non_exhaustive()
    }
}

impl RemoteOptions {
    /// The defaults, with these programs and helpers.
    #[must_use]
    pub fn new(
        ssh: Result<PathBuf, String>,
        askpass: Result<PathBuf, String>,
        helpers: Helpers,
    ) -> Self {
        Self {
            ssh,
            wsl: pitcrew_remote::wsl::default_program(),
            askpass,
            helpers,
            runtime_dir: None,
            multiplex: None,
            ssh_config: None,
            home: None,
            sites_dir: None,
            plan_ttl: plan::PLAN_TTL,
            job_wait: Duration::from_secs(10 * 60),
            job_poll: Duration::from_secs(2),
            connect_wait: Duration::from_secs(3 * 60),
            connector: ConnectorOptions::default(),
        }
    }
}

/// `gateway_ssh_hosts`' answer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SshHosts {
    /// The concrete `Host` names of the person's ssh config, in file order.
    pub hosts: Vec<String>,
}

/// `gateway_remote_probe`'s answer (the contract's `RemoteProbe`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProbe {
    /// The host, as given.
    pub host: String,
    /// `linux`, `macos`, …
    pub os: String,
    /// `x86_64`, `aarch64`, …
    pub arch: String,
    /// The helper already there, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub helper: Option<HelperFound>,
    /// SLURM, if the machine has it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slurm: Option<SlurmFound>,
    /// tmux, if the machine has it (the tmux launcher needs 3.2 or newer).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tmux: Option<TmuxFound>,
    /// The machine check, made over the same connection: each agent CLI and its version, tmux,
    /// git, gh, free disk in the home folder, SLURM where it is there, and PitCrew's helper
    /// (`pitcrew_remote::check`). Absent when the check could not run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub check: Option<MachineCheck>,
}

/// tmux on the machine.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TmuxFound {
    /// `tmux -V`'s version, e.g. `3.3a`.
    pub version: String,
}

/// A helper already on the machine.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct HelperFound {
    /// The version running, else the version installed.
    pub version: String,
    /// Whether it runs (for SLURM: its job runs and it listens).
    pub running: bool,
}

/// SLURM on the machine.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SlurmFound {
    /// `sbatch --version`'s first line.
    pub version: String,
    /// The partition `sinfo` marks as the default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_partition: Option<String>,
    /// Whether `srun` has `--overlap` (what a recipe's `srun` last hop needs).
    pub srun_overlap: bool,
}

/// A progress message of `gateway_remote_add`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AddProgress {
    /// One of the plan's steps, or [`ADD_STEP`] for the whole add (always the last message).
    pub step: String,
    /// Where it stands.
    pub state: StepState,
    /// More, for people.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Where a step stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StepState {
    /// Under way.
    Running,
    /// Done.
    Done,
    /// It failed; `detail` says why.
    Failed,
}

impl AddProgress {
    fn new(step: &str, state: StepState, detail: Option<String>) -> Self {
        Self {
            step: step.to_owned(),
            state,
            detail,
        }
    }
}

/// Where `gateway_remote_add`'s progress goes.
pub type Progress = Arc<dyn Fn(&AddProgress) + Send + Sync>;

/// What a plan holds until it is carried out.
struct Plan {
    host: String,
    wsl: Option<crate::registry::WslTarget>,
    launcher: LauncherKind,
    target: Target,
    helper: HelperRef,
    /// SLURM: the script shown, which is what is submitted; `None` when a job of the helper's is
    /// already queued or running, and is used instead.
    script: Option<JobScript>,
    site: Option<String>,
    job: Option<JobRequest>,
    last_hop: Option<LastHop>,
    steps: Steps,
}

#[derive(Clone)]
struct Steps {
    deploy: String,
    launch: String,
    connect: String,
    pair: String,
}

impl Steps {
    fn all(&self) -> Vec<String> {
        vec![
            self.deploy.clone(),
            self.launch.clone(),
            self.connect.clone(),
            self.pair.clone(),
        ]
    }
}

/// What a failed add stops again.
enum Undo {
    Nothing,
    /// The helper this add started.
    Helper(Arc<dyn Launcher>),
    /// The SLURM job this add submitted.
    Job(Arc<dyn Launcher>, u64),
}

/// `error`, saying also what a failed undo left behind.
fn with_note(error: GatewayError, note: Option<String>) -> GatewayError {
    match note {
        None => error,
        Some(note) => GatewayError::new(error.code, format!("{}; {note}", error.message)),
    }
}

/// Fires when the person cancels a running add ([`Remotes::cancel`]).
struct Cancel(watch::Receiver<bool>);

impl Cancel {
    fn is_set(&self) -> bool {
        *self.0.borrow()
    }

    /// Completes once the add is cancelled (never, if it is not).
    async fn fired(&self) {
        let mut rx = self.0.clone();
        if rx.wait_for(|set| *set).await.is_err() {
            std::future::pending::<()>().await;
        }
    }
}

/// The error of an add the person cancelled.
fn cancelled(host: &str) -> GatewayError {
    GatewayError::unreachable(format!("adding {host} was cancelled"))
}

/// The error of an add that could not finish because the app is quitting.
fn quitting(host: &str) -> GatewayError {
    GatewayError::unreachable(format!("adding {host} stopped: PitCrew is quitting"))
}

/// `work`, unless the add is cancelled first: then `work` is dropped where it waits.
async fn or_cancelled<T>(
    cancel: &Cancel,
    host: &str,
    work: impl Future<Output = Result<T, GatewayError>>,
) -> Result<T, GatewayError> {
    tokio::select! {
        biased;
        () = cancel.fired() => Err(cancelled(host)),
        done = work => done,
    }
}

/// The adds running, by plan id, each with its cancel.
type Adds = Mutex<HashMap<String, watch::Sender<bool>>>;

/// Forgets a running add however it ends.
struct Running<'a> {
    adds: &'a Adds,
    plan: String,
}

impl Drop for Running<'_> {
    fn drop(&mut self) {
        self.adds
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.plan);
    }
}

type Links = Arc<Mutex<HashMap<String, Arc<Link>>>>;

/// The workspaces whose retry's attempt is running, each with whether another retry came
/// meanwhile.
type Retries = Arc<Mutex<HashMap<String, bool>>>;

/// Remote workspaces: the gateway's remote commands, and the tunnels of the workspaces added.
pub struct Remotes {
    core: Core,
    plans: Mutex<PlanStore<Plan>>,
    adds: Adds,
    waker: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl fmt::Debug for Remotes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Remotes")
            .field("options", &self.core.options)
            .field("links", &self.core.lock_links().len())
            .finish_non_exhaustive()
    }
}

impl Remotes {
    /// Remote workspaces in `registry`, their tokens in `tokens`, their prompts asked through
    /// `prompts`, their tunnels on `runtime`.
    #[must_use]
    pub fn new(
        options: RemoteOptions,
        registry: Arc<Registry>,
        tokens: Arc<dyn TokenStore>,
        prompts: Arc<PromptHub>,
        runtime: tokio::runtime::Handle,
    ) -> Self {
        let ttl = options.plan_ttl;
        Self {
            core: Core {
                options: Arc::new(options),
                registry,
                tokens,
                prompts,
                versions: Arc::default(),
                links: Links::default(),
                retries: Retries::default(),
                membership: Arc::default(),
                closed: Arc::default(),
                runtime,
                #[cfg(test)]
                seams: Seams::default(),
            },
            plans: Mutex::new(PlanStore::new(ttl)),
            adds: Adds::default(),
            waker: Mutex::new(None),
        }
    }

    /// The prompt hub, for `gateway_prompt_reply`.
    #[must_use]
    pub fn prompts(&self) -> &Arc<PromptHub> {
        &self.core.prompts
    }

    /// `gateway_ssh_hosts`: the concrete `Host` names of the person's ssh config, read off the
    /// async threads.
    pub async fn ssh_hosts(&self) -> SshHosts {
        let options = &self.core.options;
        let home = options.home.clone().or_else(pitcrew_remote::home_dir);
        let Some(home) = home else {
            return SshHosts { hosts: Vec::new() };
        };
        let config = options
            .ssh_config
            .clone()
            .unwrap_or_else(|| home.join(".ssh").join("config"));
        let listed =
            tokio::task::spawn_blocking(move || pitcrew_remote::list_hosts_in(&config, &home))
                .await;
        let Ok(list) = listed else {
            return SshHosts { hosts: Vec::new() };
        };
        for note in &list.notes {
            tracing::debug!(note = %tidy(note), "reading the ssh config");
        }
        SshHosts { hosts: list.hosts }
    }

    /// `gateway_remote_probe`: what the machine is, and what of PitCrew is there already.
    ///
    /// # Errors
    /// `invalid` for a host ssh would not take; `unreachable` when ssh fails (with its reason);
    /// `internal` without `pitcrew-askpass`.
    pub async fn probe(&self, host: &str) -> Result<RemoteProbe, GatewayError> {
        self.probe_target(host, None).await
    }

    /// Lists local distributions without reading SSH configuration.
    pub async fn wsl_distros(&self) -> Result<pitcrew_remote::WslDistros, GatewayError> {
        pitcrew_remote::Wsl::new(&self.core.options.wsl)
            .distros()
            .await
            .map_err(|e| GatewayError::unreachable(tidy(&e.to_string())))
    }

    /// Probes a transport selected by the caller.
    pub async fn probe_target(
        &self,
        host: &str,
        target: Option<&crate::registry::WslTarget>,
    ) -> Result<RemoteProbe, GatewayError> {
        let (host, ssh) = self.core.transport(host, target).await?;
        let host = host.as_str();
        let probe = ssh
            .probe(host)
            .await
            .map_err(|e| self.core.ssh_err(host, &e))?;
        // The tools' check rides on the same connection; a check that fails leaves it out.
        let checked = match ssh.check_machine(host).await {
            Ok(checked) => Some(checked),
            Err(e) => {
                tracing::warn!(host, error = %tidy(&e.to_string()), "the machine check did not run");
                None
            }
        };
        let helper = match Target::new(ssh, host, &probe) {
            Ok(target) => helper_status(&target).await,
            Err(_) => None,
        };
        let slurm = probe
            .slurm
            .sbatch
            .as_ref()
            .filter(|_| target.is_none())
            .map(|version| SlurmFound {
                version: tidy(version),
                default_partition: probe.slurm.default_partition.clone(),
                srun_overlap: probe.slurm.srun_overlap,
            });
        let tmux = probe.tmux_version.as_ref().map(|version| TmuxFound {
            version: tidy(version),
        });
        let check = checked.map(|mut checked| {
            checked.rows.push(pitcrew_remote::check::helper_row(
                helper.as_ref().map(|h| (h.version.as_str(), h.running)),
            ));
            // WSL is direct and tmux only: no SLURM there.
            if target.is_some() {
                checked.rows.retain(|row| {
                    row.id != pitcrew_protocol::machine_setup::MachineCheckItem::Slurm
                });
            }
            checked
        });
        tracing::info!(host, os = %probe.info.os, arch = %probe.info.arch, "probed a machine");
        Ok(RemoteProbe {
            host: host.to_owned(),
            os: tidy(&probe.info.os),
            arch: tidy(&probe.info.arch),
            helper,
            slurm,
            tmux,
            check,
        })
    }

    /// `gateway_remote_plan`: what adding the machine would do, without doing it.
    ///
    /// # Errors
    /// `invalid` for a request, a machine or a job PitCrew refuses (with why), or a missing
    /// helper; `unreachable` when ssh fails.
    pub async fn plan(&self, req: RemotePlanRequest) -> Result<RemotePlan, GatewayError> {
        if req.target.is_some() && req.launcher == LauncherKind::Slurm {
            return Err(GatewayError::invalid("WSL supports direct and tmux only"));
        }
        let (host, ssh) = self.core.transport(&req.host, req.target.as_ref()).await?;
        if req.launcher != LauncherKind::Slurm && (req.site.is_some() || req.job.is_some()) {
            return Err(GatewayError::invalid(
                "a site and job options are for the slurm launcher only",
            ));
        }
        if let Some(site) = &req.site {
            plan::check_site_name(site)?;
        }
        let probe = ssh
            .probe(&host)
            .await
            .map_err(|e| self.core.ssh_err(&host, &e))?;
        let target =
            Target::new(ssh, &host, &probe).map_err(|e| self.core.helper_err(&host, &e))?;
        let helper = self
            .core
            .options
            .helpers
            .find(target.platform())
            .map_err(|e| GatewayError::invalid(tidy(&e)))?;
        let root = target.layout().root().to_owned();
        let deploy = format!(
            "Copy pitcrewd {} to {root}/bin on {host}, and check it there",
            helper.version
        );
        let (launch, script, site, last_hop) = match req.launcher {
            LauncherKind::Direct => (
                format!("Start it on {host} in the background"),
                None,
                None,
                None,
            ),
            LauncherKind::Tmux => {
                TmuxLauncher::new(probe.tmux_version.as_deref(), LaunchOptions::default())
                    .map_err(|e| helper_error(&host, &e))?;
                (
                    format!(
                        "Start it on {host} in its own tmux session, {}",
                        tmux_name(target.layout())
                    ),
                    None,
                    None,
                    None,
                )
            }
            LauncherKind::Slurm => {
                let site = self.site(req.site.as_deref())?;
                check_tools(&probe.slurm, site.last_hop()).map_err(|e| helper_error(&host, &e))?;
                let options = plan::job_options(req.job.as_ref())?;
                let script = JobSpec::new(&site, &options)
                    .and_then(|spec| spec.render(&target))
                    .map_err(|e| helper_error(&host, &e))?;
                // A job of the helper's already queued or running is used, not a new one.
                let active = SlurmLauncher::default()
                    .job_status(&target)
                    .await
                    .ok()
                    .filter(|s| {
                        matches!(s.state, JobState::Pending { .. } | JobState::Running { .. })
                    });
                let (launch, script) = match active {
                    Some(status) => (
                        format!(
                            "Use the helper's {} on {host}; no job is submitted",
                            tidy(&status.describe())
                        ),
                        None,
                    ),
                    None => (
                        format!(
                            "Submit the job script below on {host} with sbatch, and wait for it \
                             to start"
                        ),
                        Some(script),
                    ),
                };
                (
                    launch,
                    script,
                    Some(site.name.clone()),
                    Some(site.last_hop()),
                )
            }
        };
        let steps = Steps {
            deploy,
            launch,
            connect: format!(
                "Connect to the helper on {host} through {}",
                if req.target.is_some() {
                    "WSL stdio"
                } else {
                    "SSH"
                }
            ),
            pair: "Pair: keep its device token in this computer's keychain".to_owned(),
        };
        let id = new_id();
        let answer = RemotePlan {
            plan: id.clone(),
            steps: steps.all(),
            job_script: script.as_ref().map(|s| s.text().to_owned()),
        };
        let plan = Plan {
            host: host.clone(),
            wsl: req.target,
            launcher: req.launcher,
            target,
            helper,
            script,
            site,
            job: req.job,
            last_hop,
            steps,
        };
        self.lock_plans().insert(id, plan, Instant::now());
        tracing::info!(
            host,
            launcher = req.launcher.as_str(),
            "planned adding a machine"
        );
        Ok(answer)
    }

    /// `gateway_remote_add`: carries out plan `plan`, once. Progress goes to `progress`, ending
    /// with one [`ADD_STEP`] message, `done` or `failed`.
    ///
    /// # Errors
    /// `invalid` for a plan that is unknown, used or expired, and for a launch the machine
    /// refuses; `unreachable` when the connection is lost, a prompt is cancelled or the add is
    /// ([`Remotes::cancel`]); others as the steps fail.
    pub async fn add(
        &self,
        plan: &str,
        progress: Progress,
    ) -> Result<GatewayWorkspace, GatewayError> {
        // The plan is taken and the add's cancel recorded in one step, under the plans lock,
        // which `cancel` takes first too: a cancel comes before both, and drops the plan, or
        // after both, and finds the add.
        let started = {
            let mut plans = self.lock_plans();
            let taken = plans.take(plan, Instant::now());
            self.core.at("add: plan taken");
            taken.map(|taken| {
                let (cancel_tx, cancel_rx) = watch::channel(false);
                self.lock_adds().insert(plan.to_owned(), cancel_tx);
                (taken, cancel_rx)
            })
        };
        let result = match started {
            Ok((taken, cancel_rx)) => {
                let _running = Running {
                    adds: &self.adds,
                    plan: plan.to_owned(),
                };
                self.core.at("add: running");
                let host = taken.host.clone();
                tracing::info!(host, launcher = taken.launcher.as_str(), "adding a machine");
                let result = self.carry_out(taken, &progress, &Cancel(cancel_rx)).await;
                match &result {
                    Ok(workspace) => {
                        tracing::info!(host, workspace = %workspace.id, "added a remote workspace");
                    }
                    Err(e) => tracing::info!(host, error = %e, "adding a machine failed"),
                }
                result
            }
            Err(refused) => Err(refused.into()),
        };
        match &result {
            Ok(_) => progress(&AddProgress::new(ADD_STEP, StepState::Done, None)),
            Err(e) => progress(&AddProgress::new(
                ADD_STEP,
                StepState::Failed,
                Some(e.message.clone()),
            )),
        }
        result
    }

    /// `gateway_remote_cancel`: stops the add carrying out plan `plan`, if one is running, and
    /// undoes what it started (the helper it started is stopped, the job it submitted
    /// cancelled); that add then fails. A step already running on the machine (starting the
    /// helper, submitting the job) finishes first, so that what it started is known and undone;
    /// waiting (for the upload, the job, the connection, the pairing) stops at once. A plan not
    /// used yet is dropped, so an add that comes after the cancel fails. An add that has
    /// finished, or an unknown plan, is left alone.
    pub fn cancel(&self, plan: &str) {
        // The plans lock first, as `add` takes it: see there.
        let mut plans = self.lock_plans();
        let unused = plans.take(plan, Instant::now()).is_ok();
        let running = self
            .lock_adds()
            .get(plan)
            .map(|add| add.send_replace(true))
            .is_some();
        drop(plans);
        if unused || running {
            tracing::info!(running, "cancelled adding a machine");
        }
    }

    /// `gateway_workspace_remove`: forgets remote workspace `workspace` and deletes its token.
    /// With `stop_helper`, first stops its helper (cancelling its job for SLURM); if that fails,
    /// nothing is forgotten.
    ///
    /// Its token, its entry and its link go in one step, under the membership lock that
    /// pairing's claim, token and link take too ([`Core::keep`]): a pairing of the same machine
    /// comes wholly before (and is removed with it) or wholly after (and stays, with its token
    /// and link). A retry that comes meanwhile finds no workspace to put a link in for
    /// ([`Core::install`]). The link closes after that step.
    ///
    /// # Errors
    /// `unknown_workspace`; `invalid` for the local workspace; the stop's error; `internal` when
    /// the keychain cannot delete the token (then nothing is forgotten).
    pub async fn remove(&self, workspace: &str, stop_helper: bool) -> Result<(), GatewayError> {
        let core = &self.core;
        let record = core
            .registry
            .record(workspace)
            .ok_or_else(|| GatewayError::unknown_workspace(workspace))?;
        let Connection::Remote(remote) = &record.connection else {
            return Err(GatewayError::invalid(
                "the local workspace cannot be removed",
            ));
        };
        if stop_helper {
            if remote.target.is_none() {
                core.ssh_check().ask().await;
            }
            let target = core.target_of(remote)?;
            let stopped = launcher_of(remote.launcher)
                .stop(&target)
                .await
                .map_err(|e| core.helper_err(&remote.host, &e))?;
            tracing::info!(workspace = %record.id, host = %remote.host, pid = ?stopped.pid, "stopped the remote helper");
        }
        let link = {
            let _membership = core.lock_membership();
            core.tokens.delete(&record.id).map_err(|e| {
                GatewayError::internal(format!("cannot delete the workspace's token: {e}"))
            })?;
            if let Err(e) = core.registry.remove(&record.id) {
                tracing::warn!(workspace = %record.id, error = %e, "the workspace is removed but the registry is not saved");
            }
            core.at("remove: forgotten");
            core.lock_retries().remove(&record.id);
            core.lock_links().remove(&record.id)
        };
        if let Some(link) = link {
            link.close().await;
        }
        tracing::info!(workspace = %record.id, host = %remote.host, "removed a remote workspace");
        Ok(())
    }

    /// Makes the tunnels of the remote workspaces saved in the registry (at start), in a task
    /// that first asks `ssh -V`, so that each tunnel's ssh gets prompts only if it may
    /// ([`gate`]). One that cannot be made is `unreachable`, saying why.
    pub fn resume(&self) {
        let saved: Vec<(String, RemoteConnection)> = self
            .core
            .registry
            .records()
            .into_iter()
            .filter_map(|record| match record.connection {
                Connection::Remote(remote) => Some((record.id, *remote)),
                Connection::Local => None,
            })
            .collect();
        if saved.is_empty() {
            return;
        }
        let core = self.core.clone();
        self.core.runtime.spawn(async move {
            if saved.iter().any(|(_, remote)| remote.target.is_none()) && core.programs().is_ok() {
                core.ssh_check().ask().await;
            }
            for (id, remote) in saved {
                core.reconnect(&id, &remote, None);
            }
        });
    }

    /// `gateway_workspace_retry`: tries remote workspace `workspace`'s connection again now
    /// (after a cancelled sign-in, say), with a fresh tunnel, which is `connecting` until its
    /// first attempt ends. Retries while that attempt runs make one more attempt after it, if it
    /// did not connect, not one each. A connected workspace is left alone. Returns once the
    /// attempt has started; the state follows on `gateway://workspaces`.
    ///
    /// # Errors
    /// `unknown_workspace`; `invalid` for the local workspace.
    pub fn retry(&self, workspace: &str) -> Result<(), GatewayError> {
        let record = self
            .core
            .registry
            .record(workspace)
            .ok_or_else(|| GatewayError::unknown_workspace(workspace))?;
        let Connection::Remote(remote) = &record.connection else {
            return Err(GatewayError::invalid(
                "the local workspace has no remote connection to try again",
            ));
        };
        tracing::info!(workspace = %record.id, host = %remote.host, "trying the connection again");
        self.core.retry(&record.id, remote);
        Ok(())
    }

    /// Tells every tunnel to check its way now (the computer woke, or the network changed).
    pub fn wake(&self) {
        wake_all(&self.core.links);
    }

    /// Watches for the computer waking from sleep: a timer that fires much later than asked
    /// means this process was not running, and every tunnel checks its way at once. (The
    /// tunnel notices a jump of the wall clock itself; on Windows the monotonic clock runs
    /// during sleep, which this catches.)
    pub fn watch_wakes(&self) {
        let links = Arc::clone(&self.core.links);
        let task = self.core.runtime.spawn(async move {
            loop {
                let before = Instant::now();
                tokio::time::sleep(WAKE_TICK).await;
                if before.elapsed() > WAKE_TICK + WAKE_LATE {
                    tracing::info!("the computer woke; checking the remote connections");
                    wake_all(&links);
                }
            }
        });
        if let Some(old) = self
            .waker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .replace(task)
        {
            old.abort();
        }
    }

    /// Closes every tunnel (the app is quitting). From then on no link is put in (a coalesced
    /// retry's next attempt, a resume or a pairing still running): the flag is set under the
    /// links lock, which [`Core::install`] checks it under.
    pub async fn shutdown(&self) {
        if let Some(task) = self
            .waker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            task.abort();
        }
        let links: Vec<Arc<Link>> = {
            // The membership lock first, as pairing takes it: one running finishes first, and
            // its link is closed here; one that comes after sees the flag.
            let _membership = self.core.lock_membership();
            let mut links = self.core.lock_links();
            self.core.closed.store(true, Ordering::SeqCst);
            links.drain().map(|(_, l)| l).collect()
        };
        self.core.lock_retries().clear();
        for link in links {
            link.close().await;
        }
    }

    // ─── Adding ─────────────────────────────────────────────────────────────────────────────

    /// Carries out `plan`'s steps. When `cancel` fires, the step waiting stops (one starting the
    /// helper or submitting its job finishes first), and what this add started is undone.
    async fn carry_out(
        &self,
        plan: Plan,
        progress: &Progress,
        cancel: &Cancel,
    ) -> Result<GatewayWorkspace, GatewayError> {
        let Plan {
            host,
            wsl,
            launcher,
            target,
            helper,
            script,
            site,
            job,
            last_hop,
            steps,
        } = plan;
        // Nothing is started yet: the upload just stops.
        step(
            progress,
            &steps.deploy,
            or_cancelled(
                cancel,
                &host,
                self.deploy(&target, &helper, &steps.deploy, progress),
            ),
        )
        .await?;
        let (started, undo) = step(
            progress,
            &steps.launch,
            self.launch(&target, launcher, script, &steps.launch, progress, cancel),
        )
        .await?;
        let tunnel = match step(
            progress,
            &steps.connect,
            self.connect(&target, started, last_hop, None, cancel),
        )
        .await
        {
            Ok(tunnel) => tunnel,
            Err(e) => return Err(with_note(e, self.undo(&target, undo).await)),
        };
        let record = RemoteConnection {
            target: wsl,
            host: host.clone(),
            launcher,
            root: target.layout().root().to_owned(),
            platform: target.platform().target().to_owned(),
            site,
            job,
            last_hop: last_hop.map(hop_kind),
            transport: tunnel.transport(),
        };
        // Pairing registers the workspace only in its last, synchronous part, so a cancel that
        // drops it while it waits leaves nothing registered.
        match step(
            progress,
            &steps.pair,
            or_cancelled(cancel, &host, self.pair(&target, tunnel.clone(), record)),
        )
        .await
        {
            Ok(workspace) => Ok(workspace),
            Err(e) => {
                tunnel.close().await;
                Err(with_note(e, self.undo(&target, undo).await))
            }
        }
    }

    /// Copies the helper there, checked against its sha256 here and on the machine.
    async fn deploy(
        &self,
        target: &Target,
        found: &HelperRef,
        name: &str,
        progress: &Progress,
    ) -> Result<(), GatewayError> {
        let host = target.host().to_owned();
        let read = found.clone();
        let helper = tokio::task::spawn_blocking(move || Helpers::load(&read))
            .await
            .map_err(|e| GatewayError::internal(format!("reading the helper failed: {e}")))?
            .map_err(|e| helper_error(&host, &e))?;
        let reported = Arc::new(AtomicU64::new(0));
        let sent = Arc::clone(progress);
        let told = Arc::clone(progress);
        let step_name = name.to_owned();
        let name = name.to_owned();
        let options = DeployOptions {
            // The live log's lines: what the deploy is doing, between the upload's percentages.
            step: Some(Arc::new(move |step: DeployStep| {
                told(&AddProgress::new(
                    &step_name,
                    StepState::Running,
                    Some(deploy_detail(step).to_owned()),
                ));
            })),
            progress: Some(Arc::new(move |p: pitcrew_remote::helper::Progress| {
                let percent = p
                    .sent
                    .saturating_mul(100)
                    .checked_div(p.total)
                    .unwrap_or(100);
                // Every 10 %, and once at the end.
                let step = percent / 10;
                if reported.fetch_max(step + 1, Ordering::Relaxed) <= step {
                    sent(&AddProgress::new(
                        &name,
                        StepState::Running,
                        Some(format!("{percent}% sent")),
                    ));
                }
            })),
            ..DeployOptions::default()
        };
        let deployed = pitcrew_remote::deploy(target, &helper, &options)
            .await
            .map_err(|e| self.core.helper_err(&host, &e))?;
        tracing::info!(host, version = %deployed.version, uploaded = deployed.uploaded, "deployed the helper");
        Ok(())
    }

    /// Starts the helper; for SLURM, submits the plan's script and waits for the job to run.
    /// Returns the launcher the tunnel asks where the helper is, and what to stop if a later
    /// step fails. Starting or submitting is not cut short by `cancel`, so that what it started
    /// is known; the cancel is honoured right after, and while waiting for the job.
    async fn launch(
        &self,
        target: &Target,
        kind: LauncherKind,
        script: Option<JobScript>,
        name: &str,
        progress: &Progress,
        cancel: &Cancel,
    ) -> Result<(Arc<dyn Launcher>, Undo), GatewayError> {
        let host = target.host();
        if cancel.is_set() {
            return Err(cancelled(host));
        }
        if kind != LauncherKind::Slurm {
            let how = if kind == LauncherKind::Tmux {
                "starting it in tmux"
            } else {
                "starting it in the background"
            };
            progress(&AddProgress::new(
                name,
                StepState::Running,
                Some(how.to_owned()),
            ));
            let launcher = launcher_of(kind);
            let started = launcher
                .start(target)
                .await
                .map_err(|e| self.core.helper_err(host, &e))?;
            tracing::info!(
                host,
                launcher = kind.as_str(),
                started_now = started.started_now,
                "the helper runs"
            );
            progress(&AddProgress::new(
                name,
                StepState::Running,
                Some(
                    if started.started_now {
                        "it runs and listens"
                    } else {
                        "it was already running"
                    }
                    .to_owned(),
                ),
            ));
            let undo = if started.started_now {
                Undo::Helper(Arc::clone(&launcher))
            } else {
                Undo::Nothing
            };
            if cancel.is_set() {
                return Err(with_note(cancelled(host), self.undo(target, undo).await));
            }
            return Ok((launcher, undo));
        }
        let slurm = SlurmLauncher::default();
        let (job, undo) = match script {
            Some(script) => {
                progress(&AddProgress::new(
                    name,
                    StepState::Running,
                    Some("submitting the job script shown".to_owned()),
                ));
                let submitted = slurm
                    .clone()
                    .with_script(script)
                    .submit(target)
                    .await
                    .map_err(|e| self.core.helper_err(host, &e))?;
                tracing::info!(
                    host,
                    job = submitted.job,
                    submitted_now = submitted.submitted_now,
                    "the helper's job"
                );
                if !submitted.submitted_now {
                    progress(&AddProgress::new(
                        name,
                        StepState::Running,
                        Some(format!(
                            "job {} was already queued for the helper; it is used",
                            submitted.job
                        )),
                    ));
                }
                let undo = if submitted.submitted_now {
                    Undo::Job(Arc::new(slurm.clone()), submitted.job)
                } else {
                    Undo::Nothing
                };
                (submitted.job, undo)
            }
            None => {
                let status = slurm
                    .job_status(target)
                    .await
                    .map_err(|e| self.core.helper_err(host, &e))?;
                match status.job {
                    Some(job)
                        if matches!(
                            status.state,
                            JobState::Pending { .. } | JobState::Running { .. }
                        ) =>
                    {
                        (job, Undo::Nothing)
                    }
                    _ => {
                        return Err(GatewayError::invalid(format!(
                            "the helper's job the plan would use is gone ({}); plan again",
                            tidy(&status.describe())
                        )));
                    }
                }
            }
        };
        let waited = or_cancelled(
            cancel,
            host,
            self.wait_for_job(target, &slurm, job, name, progress),
        )
        .await;
        match waited {
            Ok(()) => Ok((Arc::new(slurm), undo)),
            Err(e) => Err(with_note(e, self.undo(target, undo).await)),
        }
    }

    /// Waits for SLURM job `job` to run with its helper listening, saying how it stands.
    async fn wait_for_job(
        &self,
        target: &Target,
        slurm: &SlurmLauncher,
        job: u64,
        name: &str,
        progress: &Progress,
    ) -> Result<(), GatewayError> {
        let host = target.host();
        let deadline = Instant::now() + self.core.options.job_wait;
        let mut said = String::new();
        loop {
            let status = slurm
                .job_status(target)
                .await
                .map_err(|e| self.core.helper_err(host, &e))?;
            if status.ready() {
                return Ok(());
            }
            let now = tidy(&status.describe());
            if now != said {
                progress(&AddProgress::new(
                    name,
                    StepState::Running,
                    Some(now.clone()),
                ));
                said.clone_from(&now);
            }
            match status.state {
                JobState::Ended { .. } | JobState::NotOurs { .. } | JobState::NoJob => {
                    return Err(GatewayError::invalid(format!(
                        "the helper's job on {host} did not start: {now}"
                    )));
                }
                _ => {}
            }
            if Instant::now() >= deadline {
                return Err(GatewayError::unreachable(format!(
                    "the helper's job {job} on {host} has not started within {} minutes: {now}",
                    self.core.options.job_wait.as_secs().div_ceil(60)
                )));
            }
            tokio::time::sleep(self.core.options.job_poll).await;
        }
    }

    /// Stops what a failed add started, as far as it can. `None` when there was nothing to stop
    /// or it stopped; else what the person should know, for the add's error.
    async fn undo(&self, target: &Target, undo: Undo) -> Option<String> {
        let host = target.host();
        let (launcher, left) = match undo {
            Undo::Nothing => return None,
            Undo::Helper(launcher) => (
                launcher,
                format!("PitCrew's helper may still be running on {host}"),
            ),
            Undo::Job(launcher, job) => (
                launcher,
                format!("job {job} may still be queued on {host}; cancel it with scancel {job}"),
            ),
        };
        let why = match tokio::time::timeout(UNDO_WAIT, launcher.stop(target)).await {
            Ok(Ok(_)) => {
                tracing::info!(
                    host,
                    launcher = launcher.name(),
                    "stopped what the failed add started"
                );
                return None;
            }
            Ok(Err(e)) => tidy(&e.to_string()),
            Err(_) => format!("no answer within {} s", UNDO_WAIT.as_secs()),
        };
        tracing::warn!(host, error = %why, "cannot stop what the failed add started");
        Some(left)
    }

    /// Starts the tunnel and waits for it to connect, or for `cancel` (the tunnel is closed).
    async fn connect(
        &self,
        target: &Target,
        launcher: Arc<dyn Launcher>,
        last_hop: Option<LastHop>,
        transport: Option<Transport>,
        cancel: &Cancel,
    ) -> Result<Tunnel, GatewayError> {
        let host = target.host().to_owned();
        let daemon =
            Daemon::new(target.clone(), launcher).with_last_hop(last_hop.unwrap_or_default());
        let tunnel = {
            let _runtime = self.core.runtime.enter();
            Tunnel::start(daemon, self.core.connector_options(transport))
                .map_err(|e| link::tunnel_error(&host, &e))?
        };
        let mut watch = tunnel.watch();
        let waited = tokio::select! {
            biased;
            () = cancel.fired() => None,
            reached = tokio::time::timeout(
                self.core.options.connect_wait,
                watch.wait_for(|s| {
                    s.is_connected()
                        || matches!(s, LinkState::Unreachable { .. } | LinkState::Closed)
                }),
            ) => Some(reached.map(|r| r.map(|state| state.clone()))),
        };
        let Some(reached) = waited else {
            tunnel.close().await;
            return Err(cancelled(&host));
        };
        match reached {
            Ok(Ok(state)) if state.is_connected() => Ok(tunnel),
            Ok(Ok(state)) => {
                tunnel.close().await;
                let mut why = tidy(&state.to_string());
                if matches!(
                    state,
                    LinkState::Unreachable {
                        why: Unreachable::SignIn,
                        ..
                    }
                ) && let Some(refusal) = self.core.ssh_check().refusal().await
                {
                    why = format!("{why}; {refusal}");
                }
                Err(GatewayError::unreachable(format!(
                    "cannot reach the helper on {host}: {why}"
                )))
            }
            Ok(Err(_)) => Err(GatewayError::unreachable(format!(
                "the connection to {host} closed"
            ))),
            Err(_) => {
                let state = tunnel.state();
                tunnel.close().await;
                Err(GatewayError::unreachable(format!(
                    "no connection to the helper on {host} within {} s: {}",
                    self.core.options.connect_wait.as_secs(),
                    tidy(&state.to_string())
                )))
            }
        }
    }

    /// Reads the hub's token over SSH, checks it, and registers the workspace with it
    /// ([`Core::keep`]).
    async fn pair(
        &self,
        target: &Target,
        tunnel: Tunnel,
        remote: RemoteConnection,
    ) -> Result<GatewayWorkspace, GatewayError> {
        let host = target.host().to_owned();
        let token = self.core.read_token(target).await?;
        let pairing = Pairing {
            tunnel: tunnel.clone(),
            token: token.clone(),
        };
        let (id, name) = crate::daemon::hosted_workspace(&pairing)
            .await
            .map_err(|e| {
                GatewayError::unreachable(format!(
                    "the helper on {host} did not answer with its workspace: {}",
                    tidy(&e.message)
                ))
            })?;
        drop(pairing);
        self.core.keep(&host, tunnel, remote, &id, &name, token)
    }

    // ─── Parts ──────────────────────────────────────────────────────────────────────────────

    /// A SLURM site recipe: the built-in `generic` one, or one of the person's.
    fn site(&self, name: Option<&str>) -> Result<Site, GatewayError> {
        let name = name.unwrap_or("generic");
        if name == "generic" {
            return Ok(generic());
        }
        let Some(dir) = self.core.options.sites_dir.clone().or_else(sites_dir) else {
            return Err(GatewayError::invalid(
                "there is no home folder to find site recipes in",
            ));
        };
        for loaded in load_sites(&dir) {
            match loaded {
                Ok(site) if site.name == name => return Ok(site),
                Err(e)
                    if std::path::Path::new(&e.file)
                        .file_stem()
                        .is_some_and(|stem| stem == name) =>
                {
                    return Err(GatewayError::invalid(tidy(&e.to_string())));
                }
                _ => {}
            }
        }
        Err(GatewayError::invalid(format!(
            "there is no site recipe {:?}: add {}",
            crate::gateway::error::shorten(name),
            dir.join(format!("{name}.toml")).display()
        )))
    }

    fn lock_adds(&self) -> MutexGuard<'_, HashMap<String, watch::Sender<bool>>> {
        self.adds
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn lock_plans(&self) -> MutexGuard<'_, PlanStore<Plan>> {
        self.plans
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// What reaches the saved workspaces' machines: their ssh, their tunnels and links, and their
/// retries. Cheap to clone, so a link's follower can start a retry's next attempt.
#[derive(Clone)]
struct Core {
    options: Arc<RemoteOptions>,
    registry: Arc<Registry>,
    tokens: Arc<dyn TokenStore>,
    prompts: Arc<PromptHub>,
    versions: Arc<SshVersions>,
    links: Links,
    retries: Retries,
    /// Held while a workspace's entry, token and link change together: pairing's claim, token
    /// and link ([`Core::keep`]), and remove's token, entry and link ([`Remotes::remove`]). All
    /// synchronous; links close after it is released.
    membership: Arc<Mutex<()>>,
    /// Set (under the links lock) when the app quits: no link is put in after that.
    closed: Arc<AtomicBool>,
    runtime: tokio::runtime::Handle,
    #[cfg(test)]
    seams: Seams,
}

/// Unit tests' handles on races: a hook called at named points (to pause there), and every
/// tunnel made (to see that each one ends closed).
#[cfg(test)]
#[derive(Clone, Default)]
struct Seams {
    hook: Arc<Mutex<Option<Hook>>>,
    tunnels: Arc<Mutex<Vec<Tunnel>>>,
}

/// Called with each named point the code reaches.
#[cfg(test)]
type Hook = Arc<dyn Fn(&'static str) + Send + Sync>;

impl Core {
    /// A named point of an add, a remove, a pairing or a reconnect, where a unit test may pause
    /// to order a race. Nothing outside tests.
    #[cfg(test)]
    fn at(&self, point: &'static str) {
        let hook = self
            .seams
            .hook
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let Some(hook) = hook {
            hook(point);
        }
    }

    #[cfg(not(test))]
    #[inline]
    fn at(&self, _point: &'static str) {}

    /// The ssh program and `pitcrew-askpass`, or why remote calls cannot run.
    fn programs(&self) -> Result<(PathBuf, PathBuf), GatewayError> {
        let program = self
            .options
            .ssh
            .clone()
            .map_err(|why| GatewayError::internal(tidy(&why)))?;
        let askpass = self
            .options
            .askpass
            .clone()
            .map_err(|why| GatewayError::internal(tidy(&why)))?;
        Ok((program, askpass))
    }

    /// The WSL transport: wsl.exe, without SSH configuration, prompts or host keys.
    fn wsl(&self) -> Ssh {
        let mut transport = Ssh::wsl(&self.options.wsl);
        if let Some(dir) = &self.options.runtime_dir {
            transport = transport.with_runtime_dir(dir);
        }
        transport
    }

    /// The machine a probe or plan is about, and how to reach it: a registered WSL2 distro
    /// (started first if it is stopped, within [`pitcrew_remote::wsl::START_LIMITS`], so the
    /// probe's 30 s are not spent on WSL's cold start), or an ssh host.
    async fn transport(
        &self,
        host: &str,
        target: Option<&crate::registry::WslTarget>,
    ) -> Result<(String, Ssh), GatewayError> {
        if let Some(crate::registry::WslTarget::Wsl { distro }) = target {
            if !host.is_empty() {
                return Err(GatewayError::invalid("choose SSH or WSL, not both"));
            }
            let list = pitcrew_remote::Wsl::new(&self.options.wsl)
                .distros()
                .await
                .map_err(|e| GatewayError::unreachable(tidy(&e.to_string())))?;
            let found = list
                .distros
                .iter()
                .find(|d| d.name == *distro)
                .ok_or_else(|| GatewayError::invalid("the WSL distribution is not registered"))?;
            if found.version != 2 {
                return Err(GatewayError::invalid(
                    "WSL1 is unsupported; select a WSL2 distribution",
                ));
            }
            let wsl = self.wsl();
            if !found.running {
                wsl.start_wsl(distro)
                    .await
                    .map_err(|e| ssh_error(distro, &e))?;
            }
            Ok((distro.clone(), wsl))
        } else {
            check_host(host)?;
            Ok((host.to_owned(), self.checked_ssh().await?))
        }
    }

    /// The ssh every remote call uses: the person's OpenSSH, with only the environment it needs.
    /// Prompts go through `pitcrew-askpass` to the hub, behind the version gate, only once
    /// `ssh -V` said 8.4 or newer; otherwise ssh runs in `BatchMode` and never asks ([`gate`]).
    /// This takes the verdict as known now: [`Core::checked_ssh`] asks first.
    fn ssh(&self) -> Result<Ssh, GatewayError> {
        let (program, askpass) = self.programs()?;
        let mut ssh = Ssh::new(program.clone()).with_env_passthrough(Vec::<String>::new());
        if self.ssh_check().prompts_allowed() {
            let prompts = GatedPrompts::new(
                program,
                Arc::clone(&self.versions),
                Arc::clone(&self.prompts),
            );
            ssh = ssh.with_prompts(askpass, Arc::new(prompts) as Arc<dyn PromptHandler>);
        }
        if let Some(dir) = &self.options.runtime_dir {
            ssh = ssh.with_runtime_dir(dir);
        }
        if let Some(on) = self.options.multiplex {
            ssh = ssh.with_multiplex(on);
        }
        Ok(ssh)
    }

    /// [`Core::ssh`], once `ssh -V` has been asked: it gets prompts only if they may be shown,
    /// and a refused sign-in's error can say why.
    async fn checked_ssh(&self) -> Result<Ssh, GatewayError> {
        self.programs()?;
        self.ssh_check().ask().await;
        self.ssh()
    }

    /// Whether this computer's ssh may answer prompts.
    fn ssh_check(&self) -> SshCheck {
        SshCheck::new(
            Arc::clone(&self.versions),
            self.options.ssh.as_ref().ok().cloned(),
        )
    }

    /// An ssh failure as a gateway error ([`ssh_error`]); when this computer's ssh may answer no
    /// prompt, a sign-in that failed says so. (No prompt from that ssh is ever shown, so none
    /// was cancelled by the person: the gate refused it.)
    fn ssh_err(&self, host: &str, error: &SshError) -> GatewayError {
        let mapped = ssh_error(host, error);
        let Some(refusal) = self.ssh_check().known_refusal() else {
            return mapped;
        };
        match error {
            SshError::Cancelled => GatewayError::unreachable(format!("{host}: {refusal}")),
            SshError::AuthFailed { .. } | SshError::HostKeyRejected { .. } => {
                with_note(mapped, Some(refusal))
            }
            _ => mapped,
        }
    }

    /// A helper failure as a gateway error ([`helper_error`]), its ssh failures as
    /// [`Core::ssh_err`] says.
    fn helper_err(&self, host: &str, error: &HelperError) -> GatewayError {
        match error {
            HelperError::Ssh(e) => self.ssh_err(host, e),
            other => helper_error(host, other),
        }
    }

    fn connector_options(&self, transport: Option<Transport>) -> ConnectorOptions {
        let mut options = self.options.connector.clone();
        options.transport = transport;
        if options.ssh_config.is_none() {
            options.ssh_config.clone_from(&self.options.ssh_config);
        }
        options
    }

    /// The machine of a saved remote workspace.
    fn target_of(&self, remote: &RemoteConnection) -> Result<Target, GatewayError> {
        let platform = Platform::ALL
            .into_iter()
            .find(|p| p.target() == remote.platform)
            .ok_or_else(|| {
                GatewayError::internal(format!(
                    "the saved platform {:?} is not one PitCrew knows",
                    tidy(&remote.platform)
                ))
            })?;
        let layout = Layout::at(&remote.root).map_err(|e| helper_error(&remote.host, &e))?;
        Target::with_layout(
            if let Some(crate::registry::WslTarget::Wsl { distro }) = &remote.target {
                if *distro != remote.host
                    || remote.launcher == LauncherKind::Slurm
                    || remote.last_hop.is_some()
                    || platform == Platform::MacOs
                {
                    return Err(GatewayError::invalid("invalid saved WSL target"));
                }
                self.wsl()
            } else {
                self.ssh()?
            },
            &remote.host,
            layout,
            platform,
        )
        .map_err(|e| helper_error(&remote.host, &e))
    }

    /// A tunnel for a saved remote workspace (not connected yet).
    fn tunnel_for(&self, remote: &RemoteConnection) -> Result<Tunnel, GatewayError> {
        let target = self.target_of(remote)?;
        let last_hop = match remote.last_hop {
            Some(HopKind::Srun) => LastHop::SrunOverlap,
            _ => LastHop::Ssh,
        };
        let daemon = Daemon::new(target, launcher_of(remote.launcher)).with_last_hop(last_hop);
        let _runtime = self.runtime.enter();
        let tunnel = Tunnel::start(daemon, self.connector_options(remote.transport))
            .map_err(|e| link::tunnel_error(&remote.host, &e))?;
        #[cfg(test)]
        self.seams
            .tunnels
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(tunnel.clone());
        Ok(tunnel)
    }

    /// What a link's follower keeps in step.
    fn follow(&self) -> Follow {
        Follow {
            registry: Arc::clone(&self.registry),
            tokens: Arc::clone(&self.tokens),
            ssh: self.ssh_check(),
        }
    }

    /// Makes saved remote workspace `id`'s tunnel and the link following it ([`Core::install`]).
    /// One that cannot be made leaves the workspace `unreachable`, saying why. `ended` hears how
    /// the tunnel's first attempt ends. Whether a link was put in.
    fn reconnect(&self, id: &str, remote: &RemoteConnection, ended: Option<Ended>) -> bool {
        let tunnel = match self.tunnel_for(remote) {
            Ok(tunnel) => tunnel,
            Err(e) => {
                tracing::warn!(workspace = %id, error = %e, "cannot reach a remote workspace");
                self.registry
                    .set_remote_state(id, WorkspaceState::Unreachable, Some(e.message));
                return false;
            }
        };
        self.at("reconnect: tunnel made");
        let connector = Arc::new(RemoteConnector::new(
            id.to_owned(),
            tunnel.clone(),
            Arc::clone(&self.tokens),
        ));
        self.install(id, tunnel, connector, ended)
    }

    /// Puts in a link following `tunnel` for workspace `id`, with `connector` attached, if `id`
    /// is still a registered remote workspace and the app is not quitting; the link it replaces
    /// closes. Checked under the links lock, and [`Remotes::remove`] takes the workspace out of
    /// the registry before it takes its link out: a link put in before is closed by the remove,
    /// and one after finds no workspace. [`Remotes::shutdown`] sets `closed` under the same lock.
    /// Otherwise `tunnel` closes. Whether the link was put in.
    fn install(
        &self,
        id: &str,
        tunnel: Tunnel,
        connector: Arc<dyn Connector>,
        ended: Option<Ended>,
    ) -> bool {
        let mut links = self.lock_links();
        let quitting = self.closed.load(Ordering::SeqCst);
        let remote = self
            .registry
            .record(id)
            .is_some_and(|r| r.kind == WorkspaceKind::Remote);
        if quitting || !remote {
            drop(links);
            if quitting {
                tracing::info!(workspace = %id, "the app is quitting; a new connection is closed");
            } else {
                tracing::info!(workspace = %id, "the workspace was removed meanwhile; its new connection is closed");
            }
            self.runtime.spawn(async move { tunnel.close().await });
            return false;
        }
        self.registry.attach_remote(id, connector);
        let link = Link::start(id.to_owned(), tunnel, self.follow(), ended, &self.runtime);
        let old = links.insert(id.to_owned(), Arc::new(link));
        drop(links);
        if let Some(old) = old {
            old.detach();
            self.runtime.spawn(async move { old.close().await });
        }
        true
    }

    /// A retry of workspace `id` ([`Remotes::retry`]): a fresh attempt, unless one runs (then one
    /// more comes after it) or the workspace is connected.
    fn retry(&self, id: &str, remote: &RemoteConnection) {
        if self
            .lock_links()
            .get(id)
            .is_some_and(|link| link.is_connected())
        {
            tracing::debug!(workspace = %id, "connected: nothing to try again");
            return;
        }
        {
            let mut retries = self.lock_retries();
            if let Some(again) = retries.get_mut(id) {
                *again = true;
                tracing::debug!(workspace = %id, "an attempt runs: one more comes after it");
                return;
            }
            retries.insert(id.to_owned(), false);
        }
        self.attempt(id, remote);
    }

    /// Starts a retry's attempt: a fresh tunnel, whose first attempt's end the link's follower
    /// tells [`Core::attempt_ended`].
    fn attempt(&self, id: &str, remote: &RemoteConnection) {
        let core = self.clone();
        let workspace = id.to_owned();
        let ended: Ended = Box::new(move |outcome| core.attempt_ended(&workspace, outcome));
        if !self.reconnect(id, remote, Some(ended)) {
            self.lock_retries().remove(id);
        }
    }

    /// A retry's attempt ended: one more if it failed and a retry came meanwhile; else the
    /// workspace takes retries afresh.
    fn attempt_ended(&self, id: &str, outcome: Outcome) {
        let again = {
            let mut retries = self.lock_retries();
            let again = outcome == Outcome::Failed && retries.get(id) == Some(&true);
            if again {
                retries.insert(id.to_owned(), false);
            } else {
                retries.remove(id);
            }
            again
        };
        if !again {
            return;
        }
        match self.registry.record(id).map(|r| r.connection) {
            Some(Connection::Remote(remote)) => {
                tracing::info!(workspace = %id, "trying the connection once more, as asked meanwhile");
                self.attempt(id, &remote);
            }
            _ => {
                self.lock_retries().remove(id);
            }
        }
    }

    /// The last, synchronous part of pairing: claims workspace `id` (named `name` by its hub,
    /// which is not trusted) for the machine `remote` reaches, keeps `token` in the keychain
    /// under it, and puts in the link following `tunnel`.
    ///
    /// The id is claimed in the registry first, in one step that refuses another workspace's id
    /// (the local one's, or a remote one's on another machine), and only then is the token kept
    /// under it; if keeping it fails, the claim is undone. Claim, token and link go under the
    /// membership lock, as remove's token, entry and link do, so a remove of the same id comes
    /// wholly before or wholly after; so does the app quitting ([`Remotes::shutdown`] takes it
    /// too), which makes pairing fail before it claims anything.
    fn keep(
        &self,
        host: &str,
        tunnel: Tunnel,
        remote: RemoteConnection,
        id: &str,
        name: &str,
        token: DeviceToken,
    ) -> Result<GatewayWorkspace, GatewayError> {
        let _membership = self.lock_membership();
        if self.closed.load(Ordering::SeqCst) {
            return Err(quitting(host));
        }
        let name = workspace_name(name);
        let connector: Arc<dyn Connector> = Arc::new(RemoteConnector::new(
            id.to_owned(),
            tunnel.clone(),
            Arc::clone(&self.tokens),
        ));
        let record = WorkspaceRecord {
            id: id.to_owned(),
            name: name.clone(),
            kind: WorkspaceKind::Remote,
            connection: Connection::Remote(Box::new(remote)),
        };
        let claimed = self
            .registry
            .claim_remote(record, Arc::clone(&connector), WorkspaceState::Connecting)
            .map_err(|taken| {
                tracing::warn!(host, workspace = %id, "a hub claimed the id of a workspace already here");
                let held = crate::gateway::error::shorten(&taken.name);
                GatewayError::invalid(if taken.local {
                    format!(
                        "the hub on {host} reports the id of this computer's own workspace, \
                         already added as {held}"
                    )
                } else {
                    format!(
                        "the hub on {host} reports the id of a workspace already added as \
                         {held}; remove it first"
                    )
                })
            })?;
        self.at("pair: claimed");
        if let Err(e) = self.tokens.set(id, &token) {
            self.registry.unclaim(claimed);
            return Err(GatewayError::internal(format!(
                "cannot keep the workspace's token: {e}"
            )));
        }
        drop(token);
        // Under the membership lock the entry cannot go, nor the app start quitting: this puts
        // the link in. Should it not, nothing of this pairing stays.
        if !self.install(id, tunnel, connector, None) {
            if let Err(e) = self.tokens.delete(id) {
                tracing::warn!(workspace = %id, error = %e, "cannot delete the token of a pairing that did not finish");
            }
            self.registry.unclaim(claimed);
            return Err(quitting(host));
        }
        self.registry
            .set_remote_state(id, WorkspaceState::Ready, None);
        Ok(self
            .registry
            .list()
            .into_iter()
            .find(|w| w.id == id)
            .unwrap_or_else(|| GatewayWorkspace {
                id: id.to_owned(),
                name,
                kind: WorkspaceKind::Remote,
                host: Some(host.to_owned()),
                state: WorkspaceState::Ready,
                detail: None,
            }))
    }

    /// Reads the hub's device token over SSH, between this call's random markers (so whatever a
    /// login shell's start-up files print around it does not count). An error never holds it.
    async fn read_token(&self, target: &Target) -> Result<DeviceToken, GatewayError> {
        let host = target.host();
        let helper = target.layout().current_binary();
        let tag = new_id();
        let begin = format!("@@pitcrew-token-begin-{tag}");
        let end = format!("@@pitcrew-token-end-{tag}");
        let output = target
            .ssh()
            .run_limited(
                host,
                &[
                    "sh",
                    "-c",
                    READ_TOKEN,
                    "sh",
                    helper.as_str(),
                    begin.as_str(),
                    end.as_str(),
                ],
                TOKEN_LIMITS,
            )
            .await
            .map_err(|e| self.ssh_err(host, &e))?;
        if output.success() {
            std::str::from_utf8(&output.stdout)
                .ok()
                .and_then(|text| between(text, &begin, &end))
                .and_then(|token| DeviceToken::new(token).ok())
                .ok_or_else(|| {
                    GatewayError::internal(format!(
                        "the helper's token file on {host} does not hold a token"
                    ))
                })
        } else {
            let said = String::from_utf8_lossy(&output.stderr);
            let last = said
                .lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("");
            Err(GatewayError::unreachable(format!(
                "cannot read the helper's device token on {host} (exit code {:?}): {}",
                output.code,
                tidy(last)
            )))
        }
    }

    fn lock_links(&self) -> MutexGuard<'_, HashMap<String, Arc<Link>>> {
        self.links
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn lock_membership(&self) -> MutexGuard<'_, ()> {
        self.membership
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn lock_retries(&self) -> MutexGuard<'_, HashMap<String, bool>> {
        self.retries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Runs one step of an add, saying when it starts and how it ended.
async fn step<T>(
    progress: &Progress,
    name: &str,
    work: impl Future<Output = Result<T, GatewayError>>,
) -> Result<T, GatewayError> {
    progress(&AddProgress::new(name, StepState::Running, None));
    let result = work.await;
    match &result {
        Ok(_) => progress(&AddProgress::new(name, StepState::Done, None)),
        Err(e) => progress(&AddProgress::new(
            name,
            StepState::Failed,
            Some(e.message.clone()),
        )),
    }
    result
}

fn wake_all(links: &Links) {
    let links: Vec<Arc<Link>> = links
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .values()
        .cloned()
        .collect();
    for link in links {
        link.wake();
    }
}

/// The launcher for a saved workspace. Status and stop need no script, and no tmux version
/// (tmux's was checked when the workspace was added).
fn launcher_of(kind: LauncherKind) -> Arc<dyn Launcher> {
    match kind {
        LauncherKind::Direct => Arc::new(DirectLauncher::default()),
        LauncherKind::Tmux => match TmuxLauncher::new(Some("3.2"), LaunchOptions::default()) {
            Ok(tmux) => Arc::new(tmux),
            Err(_) => Arc::new(DirectLauncher::default()),
        },
        LauncherKind::Slurm => Arc::new(SlurmLauncher::default()),
    }
}

fn hop_kind(hop: LastHop) -> HopKind {
    match hop {
        LastHop::SrunOverlap => HopKind::Srun,
        _ => HopKind::Ssh,
    }
}

/// A deploy step as a progress detail, for the wizard's live log.
fn deploy_detail(step: DeployStep) -> &'static str {
    match step {
        DeployStep::Checking => "checking for a copy already there",
        DeployStep::AlreadyInstalled => "already there, and verified (sha256 and version)",
        DeployStep::Uploading => "uploading",
        DeployStep::Verifying => "verifying the sha256 and the version on the machine",
        DeployStep::Installed => "installed and verified",
        _ => "working",
    }
}

/// What of PitCrew is on the machine already: the helper running there, or installed.
async fn helper_status(target: &Target) -> Option<HelperFound> {
    let status = match DirectLauncher::default().status(target).await {
        Ok(status) => status,
        Err(e) => {
            tracing::debug!(host = target.host(), error = %tidy(&e.to_string()), "no helper status");
            return None;
        }
    };
    let status = if status
        .endpoint
        .as_ref()
        .is_some_and(|e| e.launcher == LauncherKind::Slurm.as_str())
    {
        SlurmLauncher::default().status(target).await.ok()?
    } else {
        status
    };
    let running = status.running();
    let version = status
        .running_version()
        .map(str::to_owned)
        .or_else(|| status.installed.clone())
        .filter(|v| !v.is_empty())?;
    Some(HelperFound {
        version: tidy(&version),
        running,
    })
}

/// The text between the line `begin` and the line `end`, trimmed: the one the remote command
/// printed between its markers.
fn between<'a>(text: &'a str, begin: &str, end: &str) -> Option<&'a str> {
    let start = text.find(&format!("{begin}\n"))? + begin.len() + 1;
    let rest = text.get(start..)?;
    let stop = rest.find(&format!("\n{end}"))?;
    rest.get(..stop).map(str::trim)
}

/// A workspace's name as the hub gives it, which is not trusted: cleaned as a notification's
/// text is (no control, bidi or invisible characters, whitespace collapsed) and cut to 80
/// characters.
pub(crate) fn workspace_name(name: &str) -> String {
    let name = crate::notify::clean(name, 80);
    if name.is_empty() {
        "Remote workspace".to_owned()
    } else {
        name
    }
}

/// Refuses a host name ssh would not take, before anything runs.
fn check_host(host: &str) -> Result<(), GatewayError> {
    pitcrew_remote::quote::validate_host(host)
        .map_err(|e| GatewayError::invalid(tidy(&e.to_string())))
}

/// An ssh failure as a gateway error.
fn ssh_error(host: &str, error: &SshError) -> GatewayError {
    let message = tidy(&format!("{host}: {error}"));
    match error {
        SshError::InvalidHost(_) | SshError::InvalidArgument(_) | SshError::UnsupportedShell(_) => {
            GatewayError::invalid(message)
        }
        SshError::Setup(_) | SshError::UnexpectedOutput(_) => GatewayError::internal(message),
        SshError::Cancelled => {
            GatewayError::unreachable(format!("signing in to {host} was cancelled"))
        }
        _ => GatewayError::unreachable(message),
    }
}

/// A deploy, launch, status or stop failure as a gateway error: what the machine or PitCrew
/// refuses is `invalid`, a lost connection or an unanswering scheduler `unreachable`.
fn helper_error(host: &str, error: &HelperError) -> GatewayError {
    if let HelperError::Ssh(e) = error {
        return ssh_error(host, e);
    }
    let message = tidy(&format!("{host}: {error}"));
    match error {
        HelperError::UnsupportedPlatform { .. }
        | HelperError::WrongPlatform { .. }
        | HelperError::InvalidArgument(_)
        | HelperError::LocalHashMismatch
        | HelperError::NoHome
        | HelperError::UnsafeDirectory(_)
        | HelperError::NoHashTool
        | HelperError::NotRunnable { .. }
        | HelperError::VersionMismatch { .. }
        | HelperError::Tmux(_)
        | HelperError::NotDeployed(_)
        | HelperError::OtherHost { .. }
        | HelperError::InUse(_)
        | HelperError::SubmitFailed(_)
        | HelperError::StartFailed(_) => GatewayError::invalid(message),
        HelperError::Busy(_)
        | HelperError::LockLost(_)
        | HelperError::Incomplete { .. }
        | HelperError::Slurm(_)
        | HelperError::Queued { .. } => GatewayError::unreachable(message),
        _ => GatewayError::internal(message),
    }
}

/// Text from ssh, the machine or the tunnel as it may go into a message or a log line: anything
/// token-shaped removed ([`crate::redact`]), control characters as spaces, bidi and invisible
/// characters removed ([`crate::notify::is_invisible`]), at most 1000 characters.
#[must_use]
pub fn tidy(text: &str) -> String {
    const MAX: usize = 1000;
    let redacted = crate::redact::redact(text);
    let mut kept = redacted
        .chars()
        .filter(|c| !crate::notify::is_invisible(*c))
        .map(|c| if c.is_control() { ' ' } else { c });
    let mut out: String = kept.by_ref().take(MAX).collect();
    if kept.next().is_some() {
        out.push('…');
    }
    out.trim().to_owned()
}

/// A random id for a plan or a prompt: 32 hex digits.
pub(crate) fn new_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut bytes = [0u8; 16];
    if getrandom::fill(&mut bytes).is_err() {
        // Unique, if not unpredictable.
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        bytes[..8].copy_from_slice(&n.to_le_bytes());
        bytes[8..].copy_from_slice(&nanos.to_le_bytes()[..8]);
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests;
