//! The SLURM launcher: the helper as a batch job on a compute node, reached through the login
//! node (ADR-0009; stream J work packages 3 and 4). Nothing on the cluster needs root, internet
//! or a compiler.
//!
//! ```text
//! let site = slurm::generic();                     // or a recipe from ~/.pitcrew/sites
//! let options = JobOptions { partition: Some("gpu".into()), ..JobOptions::default() };
//! let script = JobSpec::new(&site, &options)?.render(&target)?;
//! // Show script.text() to the user, who reads it and confirms.
//! let launcher = SlurmLauncher::default().with_script(script);
//! launcher.submit(&target).await?;                 // sbatch, at once
//! launcher.job_status(&target).await?;             // pending (Priority), running on a node…
//! launcher.cancel(&target).await?;                 // scancel, wait, forget
//! ```
//!
//! **Nothing is submitted unseen.** Submitting takes a [`JobScript`], which only
//! [`JobSpec::render`] makes, and PitCrew sends exactly its [`JobScript::text`]. The script is
//! fixed text (`job.sh`) behind `#SBATCH` lines and shell assignments made of checked values.
//!
//! **On the login node**, `helper.sh` (the script of the other launchers, under the same launch
//! lock, after the same checks of the way to the root) takes the job script from stdin, checks
//! that it arrived whole and unchanged (its length, first lines, last line and sha256), and
//! refuses while another launcher's helper uses the root. It unsets the `SBATCH_*`, `SQUEUE_*`,
//! `SCANCEL_*` and `SACCT_*` variables (which would override the directives, hide a job from
//! `squeue -j`, or make scancel ask or skip), and runs `sbatch --parsable` under `umask 077`, with
//! the job name, working directory (the root) and output (`run/slurm-<id>.out`) on the command
//! line too. It reads the job id from sbatch's standard output only, keeps the cluster sbatch
//! names (`<id>;<cluster>`, then asked about with `-M`), and records the job in `run/slurm.json`
//! at once.
//!
//! **On the compute node** the job checks the way to the root and the root as the launchers do,
//! waits for that record (so a job whose submission was cut off ends on its own), and ends
//! without touching anything while `endpoint.json` records another launcher's helper. It checks
//! the recipe's module set-up script as it checks the root (the file and the way to it belong
//! to root or the user, and no one else can write them), loads the modules, and starts
//! `bin/<current>/pitcrewd serve --listen unix:<socket>` with the user's umask. Once the socket
//! is there it writes `run/endpoint.json`, with `host` the node (`SLURMD_NODENAME`, else
//! `hostname -f`) and `job` its id. On SIGTERM (`scancel`, or the time limit) it stops the
//! helper, and removes the endpoint and the socket it started; SIGUSR1 and SIGUSR2 do not end
//! it.
//!
//! **Whose job:** a job id is acted on only while `squeue` lists it with the recorded name and
//! this user's uid (`%j`, `%U`), and scancel is given that name and uid too. An id that names any
//! other job (reused after the cluster's state was lost, say) is reported as
//! [`JobState::NotOurs`] and never cancelled; PitCrew only forgets its own record of it. sacct
//! is asked only for records with that name and uid, and prints no names.
//!
//! **Bounds:** squeue is asked at most every 2 seconds while waiting for a start, every second
//! while waiting for a cancelled job to leave the queue, and each call is bounded by the
//! [`LaunchOptions`] plus a minute for the scheduler.

mod site;
mod spec;

pub use site::{
    LastHop, MAX_SITE_FILE, SITE_KEYS, Site, SiteError, SiteRecipe, SocketPlace, builtin_sites,
    generic, load_site, load_sites, sites_dir,
};
pub use spec::{
    ALLOWED_SBATCH, DEFAULT_JOB_WAIT, JobOptions, JobScript, JobSpec, SBATCH_FLAGS, WallTime,
    check_sbatch_option, default_job_name, format_wall_time, parse_wall_time,
};

use super::deploy::{minutes, seconds};
use super::launch::{
    Endpoint, HelperFuture, HelperState, LaunchOptions, Launcher, Started, Status, Stopped,
};
use super::script::{self, Call, Report};
use super::{HelperError, Target};
use crate::probe::SlurmTools;
use pitcrew_protocol::model::TimestampMs;
use std::time::Duration;

/// Time allowed on top of the launch options for the scheduler to answer.
const SCHEDULER_SLACK: Duration = Duration::from_secs(60);

/// What the scheduler says about the recorded job.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum JobState {
    /// Nothing is recorded: no job was submitted, or the last one was stopped.
    NoJob,
    /// Waiting in the queue, for SLURM's `reason` (e.g. `Priority`, `Resources`).
    Pending {
        /// Why it waits.
        reason: String,
    },
    /// Running on `node`. The helper may still be starting: see [`SlurmStatus::ready`].
    Running {
        /// The node it runs on (`squeue %N`).
        node: String,
    },
    /// Queued in another state: `COMPLETING`, `CONFIGURING`, `SUSPENDED`, `REQUEUE_HOLD`, …
    Other {
        /// SLURM's state.
        state: String,
        /// SLURM's reason, if any.
        reason: String,
    },
    /// Finished. `state` is SLURM's final state (`COMPLETED`, `FAILED`, `CANCELLED`,
    /// `TIMEOUT`, …) from squeue while it still lists the job, else from sacct; `None` when
    /// neither knows any more (no sacct, or accounting off).
    Ended {
        /// The final state, if known.
        state: Option<String>,
        /// The exit status, if sacct knows it.
        exit: Option<JobExit>,
    },
    /// The recorded id now names someone else's job, or one with another name: PitCrew's own
    /// job is gone. That job is never acted on.
    NotOurs {
        /// Its owner's uid.
        uid: u32,
        /// Its name.
        name: String,
    },
}

/// A job's exit status, as sacct reports it (`code:signal`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JobExit {
    /// The batch script's exit code.
    pub code: u32,
    /// The signal that ended it, or 0.
    pub signal: u32,
}

impl JobExit {
    /// Reads sacct's `ExitCode`, e.g. `1:0` or `0:15`.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let (code, signal) = text.trim().split_once(':')?;
        Some(Self {
            code: code.parse().ok()?,
            signal: signal.parse().ok()?,
        })
    }
}

/// The recorded job and what the scheduler says about it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SlurmStatus {
    /// The job id PitCrew recorded, if any.
    pub job: Option<u64>,
    /// The name it was submitted with.
    pub job_name: Option<String>,
    /// When it was submitted (by the login node's clock).
    pub submitted: Option<TimestampMs>,
    /// The cluster sbatch named in its answer (`<id>;<cluster>`), which squeue, scancel and
    /// sacct are then asked about with `-M`; `None` for the default cluster.
    pub cluster: Option<String>,
    /// What the scheduler says.
    pub state: JobState,
    /// Time left before the time limit (`squeue %L`); for a pending job, the whole limit.
    pub time_left: Option<WallTime>,
    /// The time limit (`squeue %l`).
    pub time_limit: Option<WallTime>,
    /// What the job wrote once the helper's socket was up, while it names this job.
    pub endpoint: Option<Endpoint>,
    /// The version `bin/current` points to, if any.
    pub installed: Option<String>,
    /// For an ended job, the last lines of `run/slurm-<id>.out` (control characters replaced).
    pub output: Option<String>,
}

impl SlurmStatus {
    /// Whether the helper runs and can be reached: the job runs and has written its endpoint.
    #[must_use]
    pub fn ready(&self) -> bool {
        matches!(self.state, JobState::Running { .. }) && self.endpoint.is_some()
    }

    /// The state in a few words, for messages: `pending (Priority)`, `running on node017`, …
    #[must_use]
    pub fn describe(&self) -> String {
        let job = self.job.map_or_else(String::new, |id| format!("job {id} "));
        let what = match &self.state {
            JobState::NoJob => return "no job".to_owned(),
            JobState::Pending { reason } => format!("pending ({reason})"),
            JobState::Running { node } if self.endpoint.is_some() => format!("running on {node}"),
            JobState::Running { node } => {
                format!("running on {node}; the helper is starting")
            }
            JobState::Other { state, reason } if reason.is_empty() || reason == "None" => {
                state.clone()
            }
            JobState::Other { state, reason } => format!("{state} ({reason})"),
            JobState::Ended { state, exit } => {
                let mut text = format!("ended ({}", state.as_deref().unwrap_or("state unknown"));
                if let Some(exit) = exit {
                    text.push_str(&format!(", exit {}:{}", exit.code, exit.signal));
                }
                text.push(')');
                if let Some(output) = self.output.as_deref().filter(|o| !o.is_empty()) {
                    text.push_str(&format!("; its output ends: {output}"));
                }
                text
            }
            JobState::NotOurs { uid, name } => {
                format!("gone; the id now names another job ({name}, uid {uid})")
            }
        };
        format!("{job}{what}")
    }
}

/// What [`SlurmLauncher::submit`] did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Submitted {
    /// The job's id.
    pub job: u64,
    /// False when the recorded job was still queued or running, and nothing was submitted.
    pub submitted_now: bool,
    /// The job as the scheduler sees it.
    pub status: SlurmStatus,
}

/// What [`SlurmLauncher::cancel`] did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cancelled {
    /// The job that was recorded, if any.
    pub job: Option<u64>,
    /// Whether `scancel` was sent: the job was ours and still queued or running.
    pub cancelled: bool,
    /// The helper's pid on the node, when it had started.
    pub pid: Option<u32>,
    /// How the job was last seen: ended, or not ours ([`JobState::NoJob`] when nothing was
    /// recorded).
    pub state: JobState,
}

/// Runs the helper as a SLURM batch job. Status and stop need nothing more; start and submit
/// need the [`JobScript`] the user has seen ([`SlurmLauncher::with_script`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SlurmLauncher {
    options: LaunchOptions,
    script: Option<JobScript>,
}

impl Default for SlurmLauncher {
    /// [`LaunchOptions::default`], but waiting a minute for the job to start (the queue
    /// allowing) and a minute for a cancelled job to leave the queue (SLURM gives a job 30
    /// seconds between SIGTERM and SIGKILL by default).
    fn default() -> Self {
        Self::new(LaunchOptions {
            ready_timeout: Duration::from_secs(60),
            stop_timeout: Duration::from_secs(60),
            ..LaunchOptions::default()
        })
    }
}

impl SlurmLauncher {
    /// A SLURM launcher. `ready_timeout` is how long [`Launcher::start`] waits for the job to
    /// run and its helper to listen; `stop_timeout` how long `stop` waits for a cancelled job to
    /// leave the queue.
    #[must_use]
    pub fn new(options: LaunchOptions) -> Self {
        Self {
            options,
            script: None,
        }
    }

    /// The script to submit: the one shown to the user.
    #[must_use]
    pub fn with_script(mut self, script: JobScript) -> Self {
        self.script = Some(script);
        self
    }

    /// The script it submits, if any.
    #[must_use]
    pub fn script(&self) -> Option<&JobScript> {
        self.script.as_ref()
    }

    /// Submits the job script unless the recorded job is still queued or running, and returns
    /// at once.
    ///
    /// # Errors
    /// - [`HelperError::InvalidArgument`] without a script or with one made for another target;
    /// - [`HelperError::SubmitFailed`] when sbatch refuses it; [`HelperError::NotDeployed`];
    /// - [`HelperError::Slurm`] when the scheduler cannot be asked;
    /// - [`HelperError::InUse`] while a helper of the direct or tmux launcher runs from this
    ///   root on this host: stop it with that launcher first;
    /// - [`HelperError::OtherHost`] while one is recorded on another host sharing the home:
    ///   stop it there, or, once that host is known to be gone, forget the record with a
    ///   [`super::DirectLauncher`] stop under [`LaunchOptions::take_over`];
    /// - and the errors of the other launchers (unsafe directories, a busy lock, …).
    pub async fn submit(&self, target: &Target) -> Result<Submitted, HelperError> {
        self.submit_and_wait(target, Duration::ZERO).await
    }

    /// The recorded job and what the scheduler says about it. Takes no lock and changes nothing.
    ///
    /// # Errors
    /// [`HelperError::Slurm`] when squeue fails (the scheduler may be unreachable: nothing is
    /// concluded), and the errors of the other launchers.
    pub async fn job_status(&self, target: &Target) -> Result<SlurmStatus, HelperError> {
        let report = script::run(
            target,
            Call {
                command: "slurm-status",
                args: Vec::new(),
                payload: None,
                progress: None,
                timeout: self.options.call_timeout(SCHEDULER_SLACK),
            },
        )
        .await?;
        parse_status(&report)
    }

    /// Cancels the recorded job if it is ours and still queued, waits up to `stop_timeout` for
    /// it to leave the queue, and forgets it (its record, endpoint and socket). A recorded id
    /// that now names another job is never cancelled, only forgotten. Idempotent.
    ///
    /// # Errors
    /// [`HelperError::StopFailed`] when the job is still queued after the wait (the record is
    /// kept, so stopping again finishes the job); [`HelperError::Slurm`] when squeue fails.
    pub async fn cancel(&self, target: &Target) -> Result<Cancelled, HelperError> {
        self.options.check()?;
        let report = script::run(
            target,
            Call {
                command: "slurm-stop",
                args: vec![
                    seconds(self.options.lock_wait).to_string(),
                    minutes(self.options.stale_lock).to_string(),
                    seconds(self.options.stop_timeout).to_string(),
                ],
                payload: None,
                progress: None,
                timeout: self
                    .options
                    .call_timeout(self.options.stop_timeout.saturating_add(SCHEDULER_SLACK)),
            },
        )
        .await?;
        let status = parse_status(&report)?;
        let pid = match report.get("pid") {
            Some(pid) => Some(pid.parse().map_err(|_| {
                HelperError::UnexpectedOutput(format!("the stop reported pid {pid:?}"))
            })?),
            None => status.endpoint.as_ref().map(|e| e.pid),
        };
        Ok(Cancelled {
            job: status.job,
            cancelled: report.get("cancelled") == Some("1"),
            pid,
            state: status.state,
        })
    }

    async fn submit_and_wait(
        &self,
        target: &Target,
        wait: Duration,
    ) -> Result<Submitted, HelperError> {
        let Some(script) = &self.script else {
            return Err(HelperError::InvalidArgument(
                "the SLURM launcher submits only a job script the user has seen: make one with \
                 JobSpec::render and pass it with SlurmLauncher::with_script"
                    .to_owned(),
            ));
        };
        script.check_target(target)?;
        self.options.check()?;
        let report = script::run(
            target,
            Call {
                command: "slurm-submit",
                args: vec![
                    seconds(self.options.lock_wait).to_string(),
                    minutes(self.options.stale_lock).to_string(),
                    script.text().len().to_string(),
                    script.sha256(),
                    script.job_name().to_owned(),
                    seconds(wait).to_string(),
                ],
                payload: Some(script.text().as_bytes()),
                progress: None,
                timeout: self
                    .options
                    .call_timeout(wait.saturating_add(SCHEDULER_SLACK)),
            },
        )
        .await?;
        let submitted_now = match report.get("started") {
            Some("1") => true,
            Some("0") => false,
            other => {
                return Err(HelperError::UnexpectedOutput(format!(
                    "the submit reported started {other:?}"
                )));
            }
        };
        let status = parse_status(&report)?;
        let job = status.job.ok_or_else(|| {
            HelperError::UnexpectedOutput("the submit reported no job".to_owned())
        })?;
        Ok(Submitted {
            job,
            submitted_now,
            status,
        })
    }
}

impl Launcher for SlurmLauncher {
    fn name(&self) -> &'static str {
        "slurm"
    }

    /// Submits the job (unless the recorded one is still queued or running) and waits up to
    /// `ready_timeout` for it to run and its helper to listen. A job still pending then is
    /// [`HelperError::Queued`]: it stays queued, and starting again waits for the same job.
    fn start<'a>(&'a self, target: &'a Target) -> HelperFuture<'a, Started> {
        Box::pin(async move {
            let submitted = self
                .submit_and_wait(target, self.options.ready_timeout)
                .await?;
            let status = &submitted.status;
            match (&status.state, &status.endpoint) {
                (JobState::Running { .. }, Some(endpoint)) => Ok(Started {
                    endpoint: endpoint.clone(),
                    started_now: submitted.submitted_now,
                }),
                (JobState::Ended { .. } | JobState::NotOurs { .. }, _) => {
                    Err(HelperError::StartFailed(status.describe()))
                }
                (JobState::Other { state, .. }, _)
                    if state == "COMPLETING" && !submitted.submitted_now =>
                {
                    Err(HelperError::Busy(format!(
                        "{}; start again once it has left the queue",
                        status.describe()
                    )))
                }
                _ => Err(HelperError::Queued {
                    job: submitted.job,
                    state: status.describe(),
                }),
            }
        })
    }

    fn status<'a>(&'a self, target: &'a Target) -> HelperFuture<'a, Status> {
        Box::pin(async move {
            let slurm = self.job_status(target).await?;
            let state = match &slurm.state {
                JobState::Running { .. } if slurm.ready() => HelperState::Running,
                JobState::Pending { .. } | JobState::Running { .. } => HelperState::Pending,
                JobState::Other { state, .. } if !is_terminal(state) && state != "COMPLETING" => {
                    HelperState::Pending
                }
                _ => HelperState::NotRunning,
            };
            Ok(Status {
                state,
                endpoint: slurm.endpoint.clone(),
                installed: slurm.installed.clone(),
                socket_ready: slurm.ready(),
                tmux_session: None,
                slurm: Some(slurm),
            })
        })
    }

    fn stop<'a>(&'a self, target: &'a Target) -> HelperFuture<'a, Stopped> {
        Box::pin(async move {
            let cancelled = self.cancel(target).await?;
            Ok(Stopped {
                pid: cancelled.pid.filter(|_| cancelled.cancelled),
                forced: false,
            })
        })
    }
}

/// Whether the SLURM launcher can run on a machine with these tools: sbatch, squeue and
/// scancel, and for the `srun` last hop, `srun --overlap`.
///
/// # Errors
/// [`HelperError::Slurm`] naming what is missing.
pub fn check_tools(tools: &SlurmTools, last_hop: LastHop) -> Result<(), HelperError> {
    let missing: Vec<&str> = [
        ("sbatch", &tools.sbatch),
        ("squeue", &tools.squeue),
        ("scancel", &tools.scancel),
    ]
    .into_iter()
    .filter(|(_, version)| version.is_none())
    .map(|(name, _)| name)
    .collect();
    if !missing.is_empty() {
        return Err(HelperError::Slurm(format!(
            "{} not found on the machine",
            missing.join(", ")
        )));
    }
    if last_hop == LastHop::SrunOverlap && !tools.srun_overlap {
        return Err(HelperError::Slurm(
            "the site recipe reaches compute nodes with srun --overlap, which this SLURM does \
             not have (it came in 20.11)"
                .to_owned(),
        ));
    }
    Ok(())
}

/// SLURM's final job states, as `helper.sh`'s `pc_terminal` lists them.
fn is_terminal(state: &str) -> bool {
    matches!(
        state,
        "BOOT_FAIL"
            | "CANCELLED"
            | "COMPLETED"
            | "DEADLINE"
            | "FAILED"
            | "NODE_FAIL"
            | "OUT_OF_MEMORY"
            | "PREEMPTED"
            | "REVOKED"
            | "SPECIAL_EXIT"
            | "TIMEOUT"
    )
}

/// Reads a `slurm-*` report: the recorded job, what squeue said (`queue`: `ours`, `gone`,
/// `foreign` or `error`), and how it ended.
fn parse_status(report: &Report) -> Result<SlurmStatus, HelperError> {
    let unexpected = |why: String| HelperError::UnexpectedOutput(why);
    let installed = report.get("installed").map(str::to_owned);
    let Some(job) = report.get("job") else {
        return Ok(SlurmStatus {
            job: None,
            job_name: None,
            submitted: None,
            cluster: None,
            state: JobState::NoJob,
            time_left: None,
            time_limit: None,
            endpoint: None,
            installed,
            output: None,
        });
    };
    let job: u64 = job
        .parse()
        .map_err(|_| unexpected(format!("the job id {job:?}")))?;
    let text = |key: &str| report.get(key).unwrap_or("").to_owned();
    let exit = report.get("acct_exit").and_then(JobExit::parse);
    let acct_state = report
        .get("acct_state")
        .and_then(|s| s.split_whitespace().next())
        .map(str::to_owned);
    let ours = report.get("queue") == Some("ours");
    let state = match report.get("queue") {
        Some("ours") => match report.get("state").unwrap_or("") {
            "PENDING" => JobState::Pending {
                reason: text("reason"),
            },
            "RUNNING" => JobState::Running { node: text("node") },
            s if is_terminal(s) => JobState::Ended {
                state: Some(s.to_owned()),
                exit,
            },
            s => JobState::Other {
                state: s.to_owned(),
                reason: text("reason"),
            },
        },
        Some("gone") => JobState::Ended {
            state: acct_state,
            exit,
        },
        Some("foreign") => JobState::NotOurs {
            uid: report
                .get("owner")
                .and_then(|u| u.parse().ok())
                .ok_or_else(|| unexpected(format!("the owner {:?}", report.get("owner"))))?,
            name: text("other_name"),
        },
        Some("error") => return Err(HelperError::Slurm(text("squeue_error"))),
        other => return Err(unexpected(format!("the queue state {other:?}"))),
    };
    let endpoint = match report.get("endpoint") {
        Some(line) => {
            let endpoint: Endpoint = serde_json::from_str(line)
                .map_err(|e| unexpected(format!("endpoint.json does not parse: {e}")))?;
            (endpoint.job == Some(job)).then_some(endpoint)
        }
        None => None,
    };
    Ok(SlurmStatus {
        job: Some(job),
        job_name: report.get("name").map(str::to_owned),
        submitted: report.get("submitted").and_then(|s| s.parse().ok()),
        cluster: report.get("cluster").map(str::to_owned),
        state,
        time_left: report
            .get("left")
            .filter(|_| ours)
            .and_then(parse_wall_time),
        time_limit: report
            .get("limit")
            .filter(|_| ours)
            .and_then(parse_wall_time),
        endpoint,
        installed,
        output: report.get("output").map(str::to_owned),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(pairs: &[(&str, &str)]) -> Report {
        Report::from_pairs(pairs)
    }

    #[test]
    fn statuses_are_read() {
        let none = parse_status(&report(&[("installed", "1.0.0")])).unwrap();
        assert_eq!(none.state, JobState::NoJob);
        assert_eq!(none.installed.as_deref(), Some("1.0.0"));
        assert_eq!(none.describe(), "no job");

        let base = [
            ("job", "4242"),
            ("name", "pitcrew-helper-0123abcd"),
            ("submitted", "1790850391000"),
        ];
        let with = |more: &[(&str, &str)]| {
            let mut pairs = base.to_vec();
            pairs.extend_from_slice(more);
            parse_status(&report(&pairs)).unwrap()
        };
        let pending = with(&[
            ("queue", "ours"),
            ("state", "PENDING"),
            ("reason", "Priority"),
            ("left", "8:00:00"),
            ("limit", "8:00:00"),
        ]);
        assert_eq!(
            pending.state,
            JobState::Pending {
                reason: "Priority".into()
            }
        );
        assert_eq!(pending.job, Some(4242));
        assert_eq!(pending.submitted, Some(1_790_850_391_000));
        assert_eq!(
            pending.time_left,
            Some(WallTime::Limited(Duration::from_secs(8 * 3600)))
        );
        assert_eq!(pending.describe(), "job 4242 pending (Priority)");
        assert!(!pending.ready());

        let endpoint = r#"{"pid":77,"host":"node017","version":"1.0.0","started":1790850391000,"launcher":"slurm","socket":"/tmp/pitcrew-4242.9/pitcrewd.sock","job":4242}"#;
        let running = with(&[
            ("queue", "ours"),
            ("state", "RUNNING"),
            ("node", "node017"),
            ("left", "1-02:03:04"),
            ("limit", "UNLIMITED"),
            ("endpoint", endpoint),
        ]);
        assert!(running.ready());
        assert_eq!(running.endpoint.as_ref().unwrap().host, "node017");
        assert_eq!(running.time_limit, Some(WallTime::Unlimited));
        assert_eq!(running.describe(), "job 4242 running on node017");
        // An endpoint another job wrote does not count.
        let other = endpoint.replace(":4242}", ":4241}");
        let starting = with(&[
            ("queue", "ours"),
            ("state", "RUNNING"),
            ("node", "node017"),
            ("endpoint", &other),
        ]);
        assert_eq!(starting.endpoint, None);
        assert!(starting.describe().ends_with("the helper is starting"));

        let ended = with(&[
            ("queue", "gone"),
            ("acct_state", "CANCELLED by 1000"),
            ("acct_exit", "0:15"),
            ("output", "pitcrew: the helper exited (143)"),
        ]);
        assert_eq!(
            ended.state,
            JobState::Ended {
                state: Some("CANCELLED".into()),
                exit: Some(JobExit {
                    code: 0,
                    signal: 15
                })
            }
        );
        assert!(
            ended.describe().contains("exit 0:15"),
            "{}",
            ended.describe()
        );
        assert_eq!(ended.time_left, None);
        let failed = with(&[("queue", "ours"), ("state", "FAILED"), ("acct_exit", "1:0")]);
        assert!(
            matches!(failed.state, JobState::Ended { state: Some(s), exit: Some(_) } if s == "FAILED")
        );
        let unknown = with(&[("queue", "gone")]);
        assert_eq!(
            unknown.state,
            JobState::Ended {
                state: None,
                exit: None
            }
        );
        let completing = with(&[
            ("queue", "ours"),
            ("state", "COMPLETING"),
            ("reason", "None"),
        ]);
        assert_eq!(completing.describe(), "job 4242 COMPLETING");

        let foreign = with(&[
            ("queue", "foreign"),
            ("owner", "4243"),
            ("other_name", "someone-elses-job"),
        ]);
        assert_eq!(
            foreign.state,
            JobState::NotOurs {
                uid: 4243,
                name: "someone-elses-job".into()
            }
        );

        let err = parse_status(&report(&[
            ("job", "4242"),
            ("queue", "error"),
            ("squeue_error", "Unable to contact slurm controller"),
        ]))
        .unwrap_err();
        assert!(
            matches!(&err, HelperError::Slurm(d) if d.contains("controller")),
            "{err:?}"
        );
        for bad in [
            &[("job", "x"), ("queue", "gone")][..],
            &[("job", "1"), ("queue", "maybe")],
            &[("job", "1"), ("queue", "foreign"), ("owner", "x")],
            &[("job", "1"), ("queue", "gone"), ("endpoint", "{")],
        ] {
            assert!(parse_status(&report(bad)).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn exits_are_read() {
        assert_eq!(JobExit::parse("1:0"), Some(JobExit { code: 1, signal: 0 }));
        assert_eq!(
            JobExit::parse(" 0:9 "),
            Some(JobExit { code: 0, signal: 9 })
        );
        for bad in ["", "1", "a:b", "1:", "-1:0"] {
            assert_eq!(JobExit::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn tools_are_checked() {
        let all = SlurmTools {
            sbatch: Some("slurm 23.02.7".into()),
            squeue: Some("slurm 23.02.7".into()),
            scancel: Some("slurm 23.02.7".into()),
            sacct: None,
            srun: Some("slurm 23.02.7".into()),
            srun_overlap: true,
            default_partition: None,
        };
        check_tools(&all, LastHop::Ssh).unwrap();
        check_tools(&all, LastHop::SrunOverlap).unwrap();
        let old = SlurmTools {
            srun_overlap: false,
            ..all.clone()
        };
        check_tools(&old, LastHop::Ssh).unwrap();
        assert!(check_tools(&old, LastHop::SrunOverlap).is_err());
        let err = check_tools(
            &SlurmTools {
                scancel: None,
                squeue: None,
                ..all
            },
            LastHop::Ssh,
        )
        .unwrap_err();
        assert!(err.to_string().contains("squeue, scancel"), "{err}");
    }

    #[test]
    fn starting_needs_a_script() {
        let launcher = SlurmLauncher::default();
        assert!(launcher.script().is_none());
        assert_eq!(launcher.name(), "slurm");
        let target = Target::with_layout(
            crate::Ssh::new("ssh"),
            "example-cluster",
            crate::helper::Layout::in_home("/home/someone").unwrap(),
            crate::helper::Platform::LinuxX86_64,
        )
        .unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let err = rt.block_on(launcher.submit(&target)).unwrap_err();
        assert!(matches!(err, HelperError::InvalidArgument(_)), "{err:?}");
        // A script made for another root is refused before any call.
        let other = Target::with_layout(
            crate::Ssh::new("ssh"),
            "example-cluster",
            crate::helper::Layout::in_home("/home/other").unwrap(),
            crate::helper::Platform::LinuxX86_64,
        )
        .unwrap();
        let script = JobSpec::new(&generic(), &JobOptions::default())
            .unwrap()
            .render(&other)
            .unwrap();
        let launcher = launcher.with_script(script);
        let err = rt.block_on(launcher.start(&target)).unwrap_err();
        assert!(matches!(err, HelperError::InvalidArgument(_)), "{err:?}");
        SlurmLauncher::default().options.check().unwrap();
    }
}
