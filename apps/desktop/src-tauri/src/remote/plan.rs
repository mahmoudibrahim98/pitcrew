//! Plans: what adding a remote machine would do, held until the person says go.
//!
//! `gateway_remote_plan` computes one and returns its steps (and for SLURM the exact job
//! script) under an opaque id; `gateway_remote_add` takes it. A plan is used once: taking it
//! removes it, whatever happens next. It expires after [`PLAN_TTL`]. At most [`MAX_PLANS`] are
//! held; the oldest goes first.

use crate::gateway::GatewayError;
use crate::registry::{JobRequest, LauncherKind};
use pitcrew_remote::JobOptions;
use pitcrew_remote::helper::slurm::{WallTime, parse_wall_time};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// How long a plan stays valid.
pub const PLAN_TTL: Duration = Duration::from_secs(10 * 60);
/// The most plans held at once.
pub const MAX_PLANS: usize = 32;

/// `gateway_remote_plan`'s argument (the contract's `RemotePlanRequest`).
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RemotePlanRequest {
    /// The host, as given to ssh.
    pub host: String,
    /// WSL target, mutually exclusive with a nonempty SSH host.
    #[serde(default)]
    pub target: Option<crate::registry::WslTarget>,
    /// How to start the helper.
    pub launcher: LauncherKind,
    /// A site recipe's name (SLURM); `generic` when absent.
    #[serde(default)]
    pub site: Option<String>,
    /// The job's options (SLURM).
    #[serde(default)]
    pub job: Option<JobRequest>,
}

/// What `gateway_remote_plan` resolves with (the contract's `RemotePlan`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemotePlan {
    /// The plan's id, for `gateway_remote_add`.
    pub plan: String,
    /// What adding will do, in order, for people.
    pub steps: Vec<String>,
    /// SLURM: exactly the text that will be submitted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job_script: Option<String>,
}

/// Why a plan cannot be taken.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// No plan has this id: it was never made, was used already, or was pushed out.
    Unknown,
    /// It is older than its time to live.
    Expired,
}

impl From<Refused> for GatewayError {
    fn from(why: Refused) -> Self {
        GatewayError::invalid(match why {
            Refused::Unknown => "no such plan: it was used already, or never made; plan again",
            Refused::Expired => "the plan has expired (plans last 10 minutes); plan again",
        })
    }
}

/// Plans by id, each with when it was made.
#[derive(Debug)]
pub struct PlanStore<P> {
    plans: HashMap<String, (P, Instant)>,
    ttl: Duration,
}

impl<P> PlanStore<P> {
    /// An empty store whose plans last `ttl`.
    #[must_use]
    pub fn new(ttl: Duration) -> Self {
        Self {
            plans: HashMap::new(),
            ttl,
        }
    }

    /// Keeps `plan` under `id`, made at `now`. Expired plans are dropped, and past
    /// [`MAX_PLANS`] the oldest.
    pub fn insert(&mut self, id: String, plan: P, now: Instant) {
        let ttl = self.ttl;
        self.plans
            .retain(|_, (_, made)| now.saturating_duration_since(*made) <= ttl);
        while self.plans.len() >= MAX_PLANS {
            let oldest = self
                .plans
                .iter()
                .min_by_key(|(_, (_, made))| *made)
                .map(|(id, _)| id.clone());
            match oldest {
                Some(id) => {
                    self.plans.remove(&id);
                }
                None => break,
            }
        }
        self.plans.insert(id, (plan, now));
    }

    /// Takes plan `id` at `now`: it is removed, used or not.
    ///
    /// # Errors
    /// [`Refused`].
    pub fn take(&mut self, id: &str, now: Instant) -> Result<P, Refused> {
        let (plan, made) = self.plans.remove(id).ok_or(Refused::Unknown)?;
        if now.saturating_duration_since(made) > self.ttl {
            return Err(Refused::Expired);
        }
        Ok(plan)
    }

    /// How many plans are held.
    #[must_use]
    pub fn len(&self) -> usize {
        self.plans.len()
    }

    /// Whether none is held.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.plans.is_empty()
    }
}

/// The job options `job` asks for, checked as the job script will check them.
///
/// # Errors
/// `invalid` for a time SLURM would not take, `UNLIMITED`, or a value
/// [`JobOptions::check`] refuses.
pub fn job_options(job: Option<&JobRequest>) -> Result<JobOptions, GatewayError> {
    let Some(job) = job else {
        return Ok(JobOptions::default());
    };
    let time = match job.time.as_deref() {
        None => None,
        Some(text) => match parse_wall_time(text) {
            Some(WallTime::Limited(time)) => Some(time),
            Some(WallTime::Unlimited) => {
                return Err(GatewayError::invalid(
                    "the wall time must be a limit such as 08:00:00, not UNLIMITED",
                ));
            }
            None => {
                return Err(GatewayError::invalid(format!(
                    "the wall time {:?} is not one SLURM takes, such as 08:00:00 or 2-00:00:00",
                    crate::gateway::error::shorten(text)
                )));
            }
        },
    };
    let options = JobOptions {
        partition: job.partition.clone(),
        account: job.account.clone(),
        qos: job.qos.clone(),
        time,
        cpus: job.cpus,
        memory: job.memory.clone(),
        gres: job.gpus.as_deref().map(gres),
        ..JobOptions::default()
    };
    options
        .check()
        .map_err(|e| GatewayError::invalid(e.to_string()))?;
    Ok(options)
}

/// Checks a site recipe's name as `pitcrew-remote` does (it is also a file name): 1 to 64
/// characters of `a-z 0-9 _ -`, starting with a letter or digit.
///
/// # Errors
/// `invalid` for any other name.
pub fn check_site_name(name: &str) -> Result<(), GatewayError> {
    let ok = (1..=64).contains(&name.len())
        && name.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '-'));
    if ok {
        Ok(())
    } else {
        Err(GatewayError::invalid(format!(
            "the site {}: a site recipe's name is 1 to 64 characters of a-z 0-9 _ -, starting \
             with a letter or digit",
            crate::gateway::error::shorten(name)
        )))
    }
}

/// `--gres` for `gpus`: a count is `gpu:<n>`, a type and count `gpu:<type>:<n>`; a value that
/// already starts with `gpu` is taken as written.
fn gres(gpus: &str) -> String {
    if gpus == "gpu" || gpus.starts_with("gpu:") {
        gpus.to_owned()
    } else {
        format!("gpu:{gpus}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn site_names_are_plain() {
        for ok in ["generic", "lab-cluster", "site_2", "0x"] {
            assert!(check_site_name(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "../etc/passwd",
            "Lab",
            "-x",
            "_x",
            "a/b",
            "a.toml",
            &"a".repeat(65),
        ] {
            assert!(check_site_name(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_plan_is_used_once_and_expires() {
        let start = Instant::now();
        let mut store = PlanStore::new(PLAN_TTL);
        store.insert("a".into(), 1, start);
        store.insert("b".into(), 2, start);
        assert_eq!(store.take("a", start + Duration::from_secs(1)), Ok(1));
        assert_eq!(
            store.take("a", start + Duration::from_secs(2)),
            Err(Refused::Unknown),
            "used once"
        );
        assert_eq!(
            store.take("b", start + PLAN_TTL + Duration::from_secs(1)),
            Err(Refused::Expired)
        );
        assert_eq!(store.take("b", start), Err(Refused::Unknown), "gone too");
        assert_eq!(store.take("never", start), Err(Refused::Unknown));
        // Just inside the limit is fine.
        store.insert("c".into(), 3, start);
        assert_eq!(store.take("c", start + PLAN_TTL), Ok(3));
        let e: GatewayError = Refused::Expired.into();
        assert_eq!(e.code, crate::gateway::ErrorCode::Invalid);
    }

    #[test]
    fn old_plans_are_dropped() {
        let start = Instant::now();
        let mut store = PlanStore::new(PLAN_TTL);
        store.insert("old".into(), 0, start);
        store.insert("new".into(), 1, start + PLAN_TTL);
        store.insert("later".into(), 2, start + PLAN_TTL + Duration::from_secs(1));
        assert_eq!(store.len(), 2, "the expired one went");
        for n in 0..MAX_PLANS + 5 {
            store.insert(
                format!("p{n}"),
                n,
                start + PLAN_TTL + Duration::from_secs(2 + n as u64),
            );
        }
        assert_eq!(store.len(), MAX_PLANS);
        assert_eq!(
            store.take("p0", start + PLAN_TTL + Duration::from_secs(100)),
            Err(Refused::Unknown),
            "the oldest went first"
        );
        let last = format!("p{}", MAX_PLANS + 4);
        assert!(
            store
                .take(&last, start + PLAN_TTL + Duration::from_secs(100))
                .is_ok()
        );
    }

    #[test]
    fn job_requests_become_checked_options() {
        let job = JobRequest {
            partition: Some("gpu".into()),
            account: Some("proj0001".into()),
            time: Some("08:00:00".into()),
            cpus: Some(4),
            memory: Some("16G".into()),
            gpus: Some("a100:2".into()),
            ..JobRequest::default()
        };
        let options = job_options(Some(&job)).unwrap();
        assert_eq!(options.time, Some(Duration::from_secs(8 * 3600)));
        assert_eq!(options.gres.as_deref(), Some("gpu:a100:2"));
        assert_eq!(options.cpus, Some(4));
        assert_eq!(gres("2"), "gpu:2");
        assert_eq!(gres("gpu:1"), "gpu:1");
        assert_eq!(job_options(None).unwrap(), JobOptions::default());
        for bad in [
            JobRequest {
                time: Some("UNLIMITED".into()),
                ..JobRequest::default()
            },
            JobRequest {
                time: Some("soon".into()),
                ..JobRequest::default()
            },
            JobRequest {
                partition: Some("gpu\n#SBATCH --uid=0".into()),
                ..JobRequest::default()
            },
            JobRequest {
                account: Some("--wrap=evil".into()),
                ..JobRequest::default()
            },
            JobRequest {
                gpus: Some("1 --hetjob".into()),
                ..JobRequest::default()
            },
        ] {
            let e = job_options(Some(&bad)).unwrap_err();
            assert_eq!(e.code, crate::gateway::ErrorCode::Invalid, "{bad:?}");
        }
    }

    #[test]
    fn requests_are_read_strictly() {
        let req: RemotePlanRequest = serde_json::from_value(serde_json::json!({
            "host": "hpc-login", "launcher": "slurm", "site": "generic",
            "job": { "partition": "gpu", "time": "01:00:00", "gpus": "1" }
        }))
        .unwrap();
        assert_eq!(req.launcher, LauncherKind::Slurm);
        assert_eq!(req.job.unwrap().gpus.as_deref(), Some("1"));
        for bad in [
            serde_json::json!({ "host": "hpc-login", "launcher": "systemd" }),
            serde_json::json!({ "host": "hpc-login", "launcher": "direct", "extra": 1 }),
            serde_json::json!({ "host": "hpc-login", "launcher": "slurm", "job": { "nodes": 2 } }),
            serde_json::json!({ "launcher": "direct" }),
        ] {
            assert!(
                serde_json::from_value::<RemotePlanRequest>(bad.clone()).is_err(),
                "{bad}"
            );
        }
        let plan = RemotePlan {
            plan: "p".into(),
            steps: vec!["a".into()],
            job_script: Some("#!/bin/sh\n".into()),
        };
        assert_eq!(
            serde_json::to_value(&plan).unwrap(),
            serde_json::json!({ "plan": "p", "steps": ["a"], "jobScript": "#!/bin/sh\n" })
        );
    }
}
