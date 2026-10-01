//! Where the daemon is, from its endpoint record, checked again before every (re)connection.
//!
//! The record (`run/endpoint.json`, and for a SLURM job what squeue says) comes from the
//! machine, so every value that ends up on an ssh command line is checked here first: the
//! socket's path, the version (for the bridge's path), and a compute node's name.

use crate::helper::slurm::{JobState, LastHop};
use crate::helper::{HelperError, HelperState, Launcher, Layout, Status, Target};
use std::fmt;
use std::sync::Arc;

/// The longest socket path (`sun_path` holds 104 bytes on macOS, 108 on Linux).
const MAX_SOCKET_PATH: usize = 100;

/// The longest compute node name accepted.
const MAX_NODE: usize = 64;

/// The daemon a [`super::Connector`] reaches: the helper that `launcher` started on `target`
/// (on the machine itself, or for the SLURM launcher on a compute node of its job), and for a
/// job, how the login node reaches the node: the site recipe's last hop.
#[derive(Clone)]
pub struct Daemon {
    pub(crate) target: Target,
    pub(crate) launcher: Arc<dyn Launcher>,
    pub(crate) last_hop: LastHop,
}

impl fmt::Debug for Daemon {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Daemon")
            .field("host", &self.target.host())
            .field("launcher", &self.launcher.name())
            .field("last_hop", &self.last_hop)
            .finish()
    }
}

impl Daemon {
    /// The helper `launcher` runs for `target`. A job's node is reached with ssh from the
    /// login node unless [`Daemon::with_last_hop`] says otherwise.
    #[must_use]
    pub fn new(target: Target, launcher: Arc<dyn Launcher>) -> Self {
        Self {
            target,
            launcher,
            last_hop: LastHop::Ssh,
        }
    }

    /// How the login node reaches a job's compute node: the site recipe's
    /// ([`crate::SiteRecipe::last_hop`]).
    #[must_use]
    pub fn with_last_hop(mut self, last_hop: LastHop) -> Self {
        self.last_hop = last_hop;
        self
    }

    /// The machine, as given to ssh.
    #[must_use]
    pub fn host(&self) -> &str {
        self.target.host()
    }
}

/// Where the daemon listens and how to get there. Every value has passed the checks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Route {
    /// The helper's socket.
    pub(crate) socket: String,
    /// The helper binary that runs the bridge (`<root>/bin/<version>/pitcrewd`).
    pub(crate) bridge: String,
    /// For a SLURM job, its node and how to get there.
    pub(crate) node: Option<Node>,
}

/// A compute node inside a job.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Node {
    pub(crate) name: String,
    pub(crate) job: u64,
    pub(crate) last_hop: LastHop,
}

/// Why there is no route now.
#[derive(Debug)]
pub(crate) enum NoRoute {
    /// Not yet: a job queued, or a helper starting. Asking again soon may help.
    Waiting(String),
    /// The helper is not running (stopped, or its job ended), or runs where this machine cannot
    /// check it. Only a new endpoint changes that.
    NotRunning(String),
    /// The record failed a check: it is never used.
    Invalid(String),
    /// Asking failed.
    Failed(HelperError),
}

/// Asks the launcher where the helper is, through `target` (the tunnel's own connection).
pub(crate) async fn resolve(daemon: &Daemon, target: &Target) -> Result<Route, NoRoute> {
    let status = daemon
        .launcher
        .status(target)
        .await
        .map_err(NoRoute::Failed)?;
    route_from(&status, target.layout(), daemon.last_hop, target.host())
}

/// The route `status` describes, if every value in it passes the checks: for a job, that it is
/// still ours and running, that its endpoint names it, and that the node squeue names is the
/// one the endpoint records and has a plain name.
pub(crate) fn route_from(
    status: &Status,
    layout: &Layout,
    last_hop: LastHop,
    host: &str,
) -> Result<Route, NoRoute> {
    if let Some(slurm) = &status.slurm {
        let node = match &slurm.state {
            JobState::Running { node } => node,
            JobState::Pending { .. } | JobState::Other { .. } => {
                return Err(NoRoute::Waiting(slurm.describe()));
            }
            JobState::NoJob => {
                return Err(NoRoute::NotRunning(format!(
                    "no helper job is recorded on {host}"
                )));
            }
            _ => return Err(NoRoute::NotRunning(slurm.describe())),
        };
        let Some(endpoint) = &slurm.endpoint else {
            return Err(NoRoute::Waiting(slurm.describe()));
        };
        check_node(node).map_err(NoRoute::Invalid)?;
        if endpoint.host != *node || endpoint.job != slurm.job || endpoint.launcher != "slurm" {
            return Err(NoRoute::Invalid(format!(
                "the helper's record names node {:?} for job {:?}, but the scheduler runs job \
                 {:?} on {node:?}",
                clean(&endpoint.host),
                endpoint.job,
                slurm.job
            )));
        }
        let Some(job) = slurm.job else {
            return Err(NoRoute::Invalid("the job has no id".to_owned()));
        };
        if last_hop == LastHop::SrunOverlap && slurm.cluster.is_some() {
            return Err(NoRoute::Invalid(
                "the job runs on another cluster of a federation, which srun cannot reach from \
                 here"
                    .to_owned(),
            ));
        }
        return Ok(Route {
            socket: checked_socket(&endpoint.socket)?,
            bridge: bridge(layout, &endpoint.version)?,
            node: Some(Node {
                name: node.clone(),
                job,
                last_hop,
            }),
        });
    }
    match &status.state {
        HelperState::Running => {}
        HelperState::Pending => {
            return Err(NoRoute::Waiting(format!(
                "the helper on {host} is starting"
            )));
        }
        HelperState::OtherHost(other) => {
            return Err(NoRoute::NotRunning(format!(
                "the helper is recorded on {}, another host sharing this home; reach that host \
                 by its own name",
                clean(other)
            )));
        }
        _ => {
            return Err(NoRoute::NotRunning(format!(
                "the helper is not running on {host}"
            )));
        }
    }
    let Some(endpoint) = &status.endpoint else {
        return Err(NoRoute::Waiting(format!(
            "the helper on {host} has not recorded its endpoint"
        )));
    };
    Ok(Route {
        socket: checked_socket(&endpoint.socket)?,
        bridge: bridge(layout, &endpoint.version)?,
        node: None,
    })
}

fn checked_socket(socket: &str) -> Result<String, NoRoute> {
    check_socket(socket).map_err(NoRoute::Invalid)?;
    Ok(socket.to_owned())
}

/// The helper binary of `version`, whose `connect` runs the bridge.
fn bridge(layout: &Layout, version: &str) -> Result<String, NoRoute> {
    crate::helper::validate_version(version).map_err(|_| {
        NoRoute::Invalid(format!(
            "the helper's record names version {:?}",
            clean(version)
        ))
    })?;
    Ok(layout.binary(version))
}

/// Replaces control characters and bounds the length, for messages.
fn clean(text: &str) -> String {
    crate::helper::script::clean(text)
}

/// A socket path from a record: absolute, at most 100 bytes, `<dir>/pitcrewd.sock`, and made of
/// characters that neither ssh (which expands `%`, `$` and `~` in forwarded socket paths, and
/// splits `-L` at `:`) nor a shell reads specially: `A-Z a-z 0-9 . _ + - /`, with no empty,
/// `.` or `..` component.
pub(crate) fn check_socket(socket: &str) -> Result<(), String> {
    // Not the path itself: it carries the user's name.
    let bad = |why: &str| Err(format!("the helper's socket path {why}"));
    if !socket.starts_with('/') {
        return bad("is not absolute");
    }
    if socket.len() > MAX_SOCKET_PATH {
        return bad("is longer than 100 bytes");
    }
    if !socket.ends_with("/pitcrewd.sock") {
        return bad("does not end in /pitcrewd.sock");
    }
    if !socket
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '-' | '/'))
    {
        return bad("has characters other than A-Z a-z 0-9 . _ + - /");
    }
    if socket
        .split('/')
        .skip(1)
        .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return bad("has an empty, . or .. component");
    }
    Ok(())
}

/// A compute node's name: 1 to 64 characters of `A-Z a-z 0-9 . _ -`, starting with a letter
/// or a digit (so never an option, and never a pattern or a list such as `node[1-2]`).
pub(crate) fn check_node(node: &str) -> Result<(), String> {
    let plain = node
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    let starts_well = node
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphanumeric());
    if node.is_empty() || node.len() > MAX_NODE || !plain || !starts_well {
        return Err(format!(
            "the node name {:?} is not 1 to {MAX_NODE} characters of A-Z a-z 0-9 . _ - \
             starting with a letter or digit",
            clean(node)
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::helper::Endpoint;
    use crate::helper::slurm::{JobExit, SlurmStatus};

    fn layout() -> Layout {
        Layout::in_home("/home/sam").unwrap()
    }

    fn endpoint(host: &str, socket: &str, job: Option<u64>) -> Endpoint {
        Endpoint {
            pid: 77,
            host: host.to_owned(),
            version: "1.4.0".to_owned(),
            started: 1_790_850_391_000,
            launcher: if job.is_some() { "slurm" } else { "direct" }.to_owned(),
            socket: socket.to_owned(),
            job,
        }
    }

    fn status(state: HelperState, endpoint: Option<Endpoint>) -> Status {
        Status {
            state,
            endpoint,
            installed: Some("1.4.0".to_owned()),
            socket_ready: true,
            tmux_session: None,
            slurm: None,
        }
    }

    fn job(state: JobState, endpoint: Option<Endpoint>) -> Status {
        Status {
            state: HelperState::Running,
            endpoint: endpoint.clone(),
            installed: Some("1.4.0".to_owned()),
            socket_ready: true,
            tmux_session: None,
            slurm: Some(SlurmStatus {
                job: Some(4242),
                job_name: Some("pitcrew-helper-0123abcd".to_owned()),
                submitted: None,
                cluster: None,
                state,
                time_left: None,
                time_limit: None,
                endpoint,
                installed: Some("1.4.0".to_owned()),
                output: None,
            }),
        }
    }

    const SOCKET: &str = "/home/sam/.pitcrew/run/pitcrewd.sock";
    const NODE_SOCKET: &str = "/tmp/pitcrew-4242.77/pitcrewd.sock";

    #[test]
    fn a_helper_on_the_host() {
        let ok = status(
            HelperState::Running,
            Some(endpoint("login01+007f0101", SOCKET, None)),
        );
        let route = route_from(&ok, &layout(), LastHop::Ssh, "hpc-login").unwrap();
        assert_eq!(route.socket, SOCKET);
        assert_eq!(route.bridge, "/home/sam/.pitcrew/bin/1.4.0/pitcrewd");
        assert_eq!(route.node, None);

        let stopped = status(HelperState::NotRunning, None);
        assert!(matches!(
            route_from(&stopped, &layout(), LastHop::Ssh, "hpc-login"),
            Err(NoRoute::NotRunning(_))
        ));
        let elsewhere = status(HelperState::OtherHost("login02".into()), None);
        assert!(matches!(
            route_from(&elsewhere, &layout(), LastHop::Ssh, "hpc-login"),
            Err(NoRoute::NotRunning(why)) if why.contains("login02")
        ));
        // A record whose values could reach ssh's command line unchecked.
        for (socket, version) in [
            ("/home/sam/.pitcrew/run/x:y/pitcrewd.sock", "1.4.0"),
            ("/home/sam/%d/pitcrewd.sock", "1.4.0"),
            ("/home/$USER/pitcrewd.sock", "1.4.0"),
            ("/home/sam/../x/pitcrewd.sock", "1.4.0"),
            ("relative/pitcrewd.sock", "1.4.0"),
            ("/home/sam/.ssh/agent.sock", "1.4.0"),
            (SOCKET, "1.4.0 -oProxyCommand=x"),
            (SOCKET, "../../tmp"),
        ] {
            let mut bad = endpoint("login01", socket, None);
            bad.version = version.to_owned();
            let bad = status(HelperState::Running, Some(bad));
            assert!(
                matches!(
                    route_from(&bad, &layout(), LastHop::Ssh, "hpc-login"),
                    Err(NoRoute::Invalid(_))
                ),
                "{socket} {version}"
            );
        }
    }

    #[test]
    fn a_job_is_checked_again() {
        let running = |node: &str| JobState::Running {
            node: node.to_owned(),
        };
        let good = job(
            running("node017"),
            Some(endpoint("node017", NODE_SOCKET, Some(4242))),
        );
        let route = route_from(&good, &layout(), LastHop::SrunOverlap, "hpc-login").unwrap();
        assert_eq!(
            route.node,
            Some(Node {
                name: "node017".to_owned(),
                job: 4242,
                last_hop: LastHop::SrunOverlap
            })
        );
        assert_eq!(route.socket, NODE_SOCKET);

        // Queued, or running while the helper starts: wait.
        for waiting in [
            job(
                JobState::Pending {
                    reason: "Priority".into(),
                },
                None,
            ),
            job(running("node017"), None),
        ] {
            assert!(matches!(
                route_from(&waiting, &layout(), LastHop::Ssh, "hpc-login"),
                Err(NoRoute::Waiting(_))
            ));
        }
        // Ended, or the id now names someone else's job: not running.
        for gone in [
            JobState::Ended {
                state: Some("CANCELLED".into()),
                exit: Some(JobExit {
                    code: 0,
                    signal: 15,
                }),
            },
            JobState::NotOurs {
                uid: 4243,
                name: "x".into(),
            },
            JobState::NoJob,
        ] {
            let gone = job(gone, None);
            assert!(matches!(
                route_from(&gone, &layout(), LastHop::Ssh, "hpc-login"),
                Err(NoRoute::NotRunning(_))
            ));
        }
        // The node squeue names must be the one recorded, and plain.
        for (squeue, recorded) in [
            ("node018", "node017"),
            ("node[017-018]", "node[017-018]"),
            ("-oProxyCommand=x", "-oProxyCommand=x"),
            ("node017;id", "node017;id"),
            ("", ""),
        ] {
            let bad = job(
                running(squeue),
                Some(endpoint(recorded, NODE_SOCKET, Some(4242))),
            );
            assert!(
                matches!(
                    route_from(&bad, &layout(), LastHop::Ssh, "hpc-login"),
                    Err(NoRoute::Invalid(_))
                ),
                "{squeue} {recorded}"
            );
        }
        // An endpoint of another launcher in the job's place.
        let mut direct = endpoint("node017", NODE_SOCKET, Some(4242));
        direct.launcher = "direct".to_owned();
        let bad = job(running("node017"), Some(direct));
        assert!(matches!(
            route_from(&bad, &layout(), LastHop::Ssh, "hpc-login"),
            Err(NoRoute::Invalid(_))
        ));
    }

    #[test]
    fn node_names() {
        for good in ["node017", "gpu-a100-03", "n1.cluster.example.org", "c_1"] {
            check_node(good).unwrap();
        }
        let long = "n".repeat(65);
        for bad in [
            "",
            "-node",
            ".node",
            "node 1",
            "node[1-2]",
            "a,b",
            "n%h",
            "n$x",
            "n:22",
            "n/1",
            "ä",
            &long,
        ] {
            assert!(check_node(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn socket_paths() {
        check_socket(SOCKET).unwrap();
        check_socket(NODE_SOCKET).unwrap();
        check_socket("/scratch/u+1/.pitcrew/run/pitcrewd.sock").unwrap();
        let long = format!("/{}/pitcrewd.sock", "d".repeat(90));
        for bad in [
            "",
            "pitcrewd.sock",
            "/home/a b/pitcrewd.sock",
            "/home/~sam/pitcrewd.sock",
            "/home//sam/pitcrewd.sock",
            "/home/./pitcrewd.sock",
            "/home/sam/pitcrewd.sock.old",
            "/home/sam/pitcrewd.sock\n",
            long.as_str(),
        ] {
            assert!(check_socket(bad).is_err(), "{bad:?}");
        }
    }
}
