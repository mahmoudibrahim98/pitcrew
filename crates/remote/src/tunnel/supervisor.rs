//! The ladder: the links, where the daemon is, the transport, watching, and what happens when the
//! way is lost. See the module docs of [`super`].

#[cfg(unix)]
use super::forward;
use super::link::{Link, LinkSpec, Via};
use super::route::{self, NoRoute, Route};
use super::{
    Active, Clock, ConnectorOptions, Daemon, Event, LinkState, Shared, Transport, TunnelError,
    Unreachable, backoff, jitter,
};
use crate::helper::HelperError;
use crate::helper::slurm::LastHop;
use crate::{Ssh, SshError};
use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

/// How long the endpoint check may take through a master (squeue may be slow). No prompt comes
/// there (`BatchMode`). Without a master (Windows) the check keeps its own limit, which pauses
/// while a prompt is open.
const STATUS_WAIT: Duration = Duration::from_secs(60);

/// How long a forwarded socket may stay silent before a patient link is given up, though ssh
/// still holds it.
#[cfg(unix)]
const SILENCE: Duration = Duration::from_secs(30);

/// The least time between two checks of the masters that failed connections ask for: a burst
/// of failures makes one.
const SUSPECT_GAP: Duration = Duration::from_secs(2);

/// The least time between two endpoint checks that failed connections ask for (each is a
/// session).
const ROUTE_GAP: Duration = Duration::from_secs(10);

/// A connector directory with no lock file this old is stale (one being made has its lock file
/// within a moment).
#[cfg(unix)]
const STALE_UNLOCKED: Duration = Duration::from_secs(60);

/// The connector's private directory: `<runtime dir>/t<16 hex digits>`, made 0700 (never one
/// that exists already), with a `lock` file locked while the connector lives (Unix), so that a
/// later connector can tell this one died and stop its master. Removed when dropped.
#[derive(Debug)]
pub(super) struct PrivateDir {
    pub(super) path: PathBuf,
    _lock: Option<std::fs::File>,
}

impl PrivateDir {
    pub(super) fn new(ssh: &Ssh) -> Result<Self, TunnelError> {
        let base = ssh.runtime_dir_for(cfg!(unix))?;
        let tag = crate::askpass::random::<8>().map_err(SshError::Setup)?;
        let path = base.join(format!("t{}", crate::askpass::to_hex(&tag)));
        create_exclusive(&path).map_err(SshError::Setup)?;
        let made = crate::private::check_private_dir(&path).and_then(|()| lock(&path));
        match made {
            Ok(lock) => Ok(Self { path, _lock: lock }),
            Err(e) => {
                let _ = std::fs::remove_dir_all(&path);
                Err(SshError::Setup(e).into())
            }
        }
    }
}

impl Drop for PrivateDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[cfg(unix)]
fn create_exclusive(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt as _;
    std::fs::DirBuilder::new().mode(0o700).create(path)
}

#[cfg(not(unix))]
fn create_exclusive(path: &Path) -> io::Result<()> {
    std::fs::create_dir(path)
}

/// Makes and locks `<dir>/lock`.
#[cfg(unix)]
fn lock(dir: &Path) -> io::Result<Option<std::fs::File>> {
    lock_with(dir, || {})
}

/// [`lock`], calling `between` after the file is made and before it is locked. The file is
/// made as `lock.new` and renamed `lock` only once locked: another connector's sweep never sees
/// a `lock` that is free while this one lives (without one, a young directory is not stale).
#[cfg(unix)]
fn lock_with(dir: &Path, between: impl FnOnce()) -> io::Result<Option<std::fs::File>> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let new = dir.join("lock.new");
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&new)?;
    between();
    rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive)
        .map_err(io::Error::from)?;
    std::fs::rename(&new, dir.join("lock"))?;
    Ok(Some(file))
}

#[cfg(not(unix))]
fn lock(_dir: &Path) -> io::Result<Option<std::fs::File>> {
    // Windows ends a dead connector's ssh with its Job Object; only logs could be left.
    Ok(None)
}

/// Why an attempt failed, and what to do about it.
#[derive(Debug)]
pub(super) struct Failure {
    kind: Kind,
    reason: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    /// May work soon: back off and try again.
    Transient,
    /// A job queued or a helper starting: try again, as for transient failures.
    Waiting,
    NotRunning,
    SignIn,
    Refused,
}

impl Failure {
    fn new(kind: Kind, reason: impl Into<String>) -> Self {
        Self {
            kind,
            reason: reason.into(),
        }
    }

    /// An ssh failure: signing in needs the person; bad input is a refusal; the rest may pass.
    fn ssh(error: &SshError, doing: &str) -> Self {
        let kind = match error {
            SshError::AuthFailed { .. }
            | SshError::HostKeyRejected { .. }
            | SshError::HostKeyChanged { .. }
            | SshError::Cancelled
            | SshError::Bridge(_) => Kind::SignIn,
            SshError::InvalidHost(_)
            | SshError::InvalidArgument(_)
            | SshError::UnsupportedShell(_) => Kind::Refused,
            _ => Kind::Transient,
        };
        Self::new(kind, format!("{doing}: {error}"))
    }

    fn tunnel(error: &TunnelError, doing: &str) -> Self {
        match error {
            // At the first connection: the endpoint check's session may not be over yet (a
            // site with `MaxSessions 1`), or another app's are open. Soon, then.
            TunnelError::Ssh(SshError::SessionRefused { .. }) => Self::new(
                Kind::Transient,
                format!("{doing}: the server allows no more sessions now (its MaxSessions)"),
            ),
            TunnelError::Ssh(e) => Self::ssh(e, doing),
            TunnelError::Refused(why) => Self::new(Kind::Refused, format!("{doing}: {why}")),
            TunnelError::NoDaemon(why) => Self::new(Kind::NotRunning, format!("{doing}: {why}")),
            other => Self::new(Kind::Transient, format!("{doing}: {other}")),
        }
    }
}

/// How the supervisor's wait between attempts ended.
enum Resume {
    /// Its time came.
    Timer,
    /// A retry, a wake or a clock jump.
    Asked,
    Close,
}

/// How watching a connected way ended.
enum End {
    Close,
    /// A connection could not sign in (Windows): wait for a retry.
    SignIn(String),
    /// The way is lost. `keep_login`: the login link still runs, and is kept to ask again
    /// without signing in again.
    Lost {
        reason: String,
        keep_login: bool,
    },
}

/// What watching was asked to do, gathered from the events and the clock.
#[derive(Default)]
struct Asked {
    check: bool,
    probe: bool,
    route: bool,
    link_suspect: bool,
}

/// An endpoint check under way (see [`Supervisor::resolving`]).
type Resolving = Pin<Box<dyn Future<Output = Result<Route, Failure>> + Send>>;

/// A forward on one of the links.
#[cfg(unix)]
#[derive(Debug)]
struct Forwarding {
    local: PathBuf,
    remote: String,
    master: PathBuf,
}

/// Owns the links and runs the ladder.
pub(super) struct Supervisor {
    daemon: Daemon,
    options: ConnectorOptions,
    shared: Arc<Shared>,
    /// With the person's prompt handler and the minimal environment.
    ssh: Ssh,
    dir: PrivateDir,
    /// ControlMasters (Unix, with connection reuse), else heartbeats.
    master: bool,
    login: Option<Link>,
    /// A compute node's link, and its name.
    node: Option<(String, Link)>,
    #[cfg(unix)]
    forward: Option<Forwarding>,
    /// Forwards made, for fresh names.
    #[cfg(unix)]
    forwards: u32,
    /// The site refused forwarding: remembered.
    forbidden: bool,
    /// A forward worked (or was remembered): a new link can be patient.
    forward_known: bool,
    /// The caller remembered stdio.
    prefer_stdio: bool,
    /// How many prompts the person has seen from this connector.
    prompts: Arc<AtomicU64>,
    /// An endpoint check has needed a prompt (without a master each one logs in): checks then
    /// come at most every `retry_every`.
    status_prompts: bool,
    clock: Clock,
}

impl Supervisor {
    pub(super) fn new(
        daemon: Daemon,
        options: ConnectorOptions,
        shared: Arc<Shared>,
        ssh: Ssh,
        dir: PrivateDir,
    ) -> Self {
        let clock = Clock::new(&options.wall_clock);
        let prompts = Arc::new(AtomicU64::new(0));
        Self {
            master: cfg!(unix) && ssh.multiplexes(),
            prefer_stdio: ssh.is_wsl() || options.transport == Some(Transport::Stdio),
            forward_known: options.transport == Some(Transport::Forwarded),
            ssh: ssh.counting_prompts(prompts.clone()),
            prompts,
            status_prompts: false,
            daemon,
            options,
            shared,
            dir,
            login: None,
            node: None,
            #[cfg(unix)]
            forward: None,
            #[cfg(unix)]
            forwards: 0,
            forbidden: false,
            clock,
        }
    }

    /// Prompts shown so far.
    fn prompted(&self) -> u64 {
        self.prompts.load(Ordering::Relaxed)
    }

    pub(super) async fn run(mut self, mut events: mpsc::UnboundedReceiver<Event>) {
        self.sweep_stale().await;
        let mut attempt: u32 = 0;
        // Since when attempts fail, and the prompts shown by then.
        let mut failing_since: Option<(Instant, u64)> = None;
        // A connection that held only briefly since the failing began.
        let mut flapped = false;
        loop {
            // An attempt from unreachable shows, so that its end is a change too, even when it
            // fails again for the same reason.
            if matches!(*self.shared.state.borrow(), LinkState::Unreachable { .. }) {
                self.shared.set(LinkState::Connecting);
            }
            let outcome = {
                let establishing = self.establish();
                tokio::pin!(establishing);
                loop {
                    tokio::select! {
                        outcome = &mut establishing => break Some(outcome),
                        event = events.recv() => match event {
                            None | Some(Event::Close) => break None,
                            // Already on it.
                            Some(_) => {}
                        },
                    }
                }
            };
            let Some(outcome) = outcome else {
                break;
            };
            let resume = match outcome {
                Ok(active) => {
                    let connected_at = Instant::now();
                    let transport = active.transport;
                    self.shared.active.send_replace(Some(active.clone()));
                    self.shared.set(LinkState::Connected { transport });
                    let end = self.monitor(&active, &mut events).await;
                    self.shared.active.send_replace(None);
                    match end {
                        End::Close => break,
                        End::SignIn(reason) => {
                            self.shared.set(LinkState::Unreachable {
                                why: Unreachable::SignIn,
                                reason,
                            });
                            self.stop_links().await;
                            self.pause(None, &mut events, true).await
                        }
                        End::Lost { reason, keep_login } => {
                            self.shared.set(LinkState::Unverifiable {
                                reason: reason.clone(),
                            });
                            self.stop(keep_login).await;
                            if connected_at.elapsed() >= self.options.give_up_after {
                                // It held: start over at once.
                                attempt = 0;
                                failing_since = None;
                                flapped = false;
                                continue;
                            }
                            // Dropped soon after connecting: that counts as failing, so a way
                            // that keeps dropping ends up unreachable, not asking for a code
                            // forever.
                            flapped = true;
                            let now = self.prompted();
                            let (since, prompts) =
                                *failing_since.get_or_insert_with(|| (Instant::now(), now));
                            if since.elapsed() >= self.options.give_up_after {
                                self.shared.set(LinkState::Unreachable {
                                    why: Unreachable::Network,
                                    reason: format!("{reason} (the connection keeps dropping)"),
                                });
                                self.stop_links().await;
                                // Reconnecting asked the person: wait for a wake or a retry.
                                // Else try again now and then.
                                let wait = (self.prompted() == prompts)
                                    .then_some(self.options.retry_every);
                                self.pause(wait, &mut events, false).await
                            } else {
                                let wait = backoff(
                                    attempt,
                                    self.options.backoff_min,
                                    self.options.backoff_max,
                                    jitter(),
                                );
                                attempt = attempt.saturating_add(1);
                                self.pause(Some(wait), &mut events, false).await
                            }
                        }
                    }
                }
                Err(failure) => {
                    let now = self.prompted();
                    let (since, prompts) =
                        *failing_since.get_or_insert_with(|| (Instant::now(), now));
                    // A way that kept dropping, and asked the person each time.
                    let costly = flapped && self.prompted() > prompts;
                    self.after(failure, since, costly, &mut attempt, &mut events)
                        .await
                }
            };
            match resume {
                Resume::Close => break,
                // Asked to (a retry, a wake, a clock jump): as from the start.
                Resume::Asked => {
                    attempt = 0;
                    failing_since = None;
                    flapped = false;
                }
                Resume::Timer => {}
            }
        }
        self.shared.closing.send_replace(true);
        self.shared.active.send_replace(None);
        self.stop_links().await;
        self.shared.set(LinkState::Closed);
    }

    /// Sets the state for a failed attempt and waits for the next one. `costly`: the way kept
    /// dropping, and reconnecting asked the person.
    async fn after(
        &mut self,
        failure: Failure,
        since: Instant,
        costly: bool,
        attempt: &mut u32,
        events: &mut mpsc::UnboundedReceiver<Event>,
    ) -> Resume {
        let Failure { kind, reason } = failure;
        let unreachable = |why| LinkState::Unreachable {
            why,
            reason: reason.clone(),
        };
        let given_up = since.elapsed() >= self.options.give_up_after;
        let next = backoff(
            *attempt,
            self.options.backoff_min,
            self.options.backoff_max,
            jitter(),
        );
        *attempt = attempt.saturating_add(1);
        // The state, how long to wait (`None`: for a retry, or a wake), and whether the login
        // link stays (to ask again through it, without signing in again).
        let (state, wait, keep_login) = match kind {
            Kind::SignIn => (unreachable(Unreachable::SignIn), None, false),
            Kind::NotRunning => (
                unreachable(Unreachable::NotRunning),
                Some(self.options.retry_every),
                true,
            ),
            Kind::Refused => (
                unreachable(Unreachable::Refused),
                Some(self.options.retry_every),
                true,
            ),
            // A queued job: keep asking, as often as the back-off allows.
            Kind::Waiting if given_up => (unreachable(Unreachable::NotRunning), Some(next), true),
            Kind::Waiting => (LinkState::Unverifiable { reason }, Some(next), true),
            // A way that kept dropping, asking for a code each time, waits for a wake. The
            // login stays while it answers (squeue down, say: no new sign-in for that).
            Kind::Transient if given_up => (
                unreachable(Unreachable::Network),
                (!costly).then_some(self.options.retry_every),
                self.login_answers().await,
            ),
            Kind::Transient => (
                LinkState::Unverifiable { reason },
                Some(next),
                self.login_answers().await,
            ),
        };
        // Without a master each endpoint check logs in: one that asked the person is not
        // repeated more often than `retry_every`.
        let wait = if !self.master && self.status_prompts {
            wait.map(|w| w.max(self.options.retry_every))
        } else {
            wait
        };
        self.shared.set(state);
        self.stop(keep_login).await;
        self.pause(wait, events, kind == Kind::SignIn).await
    }

    /// Whether the login link still runs and (with a master) answers its check.
    async fn login_answers(&mut self) -> bool {
        let Some(link) = self.login.as_mut() else {
            return false;
        };
        if !link.alive() {
            return false;
        }
        #[cfg(unix)]
        if let Some(control) = link.control() {
            return super::link::control(
                &self.ssh,
                &self.dir.path,
                control,
                link.host(),
                "check",
                &[],
            )
            .await
            .is_ok();
        }
        true
    }

    /// Waits `wait` (or for ever), until a retry, a wake (unless `retry_only`) or a clock jump
    /// says to go now.
    async fn pause(
        &mut self,
        wait: Option<Duration>,
        events: &mut mpsc::UnboundedReceiver<Event>,
        retry_only: bool,
    ) -> Resume {
        let deadline = wait.map(|w| tokio::time::Instant::now() + w);
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            let until = async {
                match deadline {
                    Some(at) => tokio::time::sleep_until(at).await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                () = until => return Resume::Timer,
                event = events.recv() => match event {
                    None | Some(Event::Close) => return Resume::Close,
                    Some(Event::Retry) => return Resume::Asked,
                    Some(Event::Wake) if !retry_only => return Resume::Asked,
                    Some(_) => {}
                },
                _ = tick.tick() => {
                    if self.clock.jumped() && !retry_only {
                        return Resume::Asked;
                    }
                }
            }
        }
    }

    /// Whether a new link can be patient: a forward will be probed through it. Not where a job
    /// is reached through srun (never forwarded).
    fn patient(&self) -> bool {
        self.master
            && self.forward_known
            && !self.forbidden
            && !self.prefer_stdio
            && self.daemon.last_hop != LastHop::SrunOverlap
    }

    /// Starts the login link (stopping any links left).
    async fn start_login(&mut self) -> Result<(), Failure> {
        self.stop_links().await;
        let host = self.daemon.host().to_owned();
        let link = Link::start(
            LinkSpec {
                ssh: &self.ssh,
                dir: &self.dir.path,
                name: "login",
                host: &host,
                via: None,
                master: self.master,
                patient: self.patient(),
            },
            self.options.link_wait,
        )
        .await
        .map_err(|e| Failure::ssh(&e, &format!("connecting to {host}")))?;
        self.login = Some(link);
        if self.ssh.is_wsl() {
            // A new distro process follows a reboot or shutdown. The launcher is idempotent,
            // so restore the deployed helper before resolving its new socket.
            self.daemon
                .launcher
                .start(&self.status_target())
                .await
                .map_err(|e| no_route(NoRoute::Failed(e), &host))?;
        }
        Ok(())
    }

    /// One attempt: the login link, the endpoint, the node's link, the transport.
    async fn establish(&mut self) -> Result<Arc<Active>, Failure> {
        // The login link, kept if it still runs.
        if !self.login.as_mut().is_some_and(Link::alive) {
            self.start_login().await?;
        }
        let prompts = self.prompted();
        let route = self.resolve().await;
        if self.prompted() > prompts {
            self.status_prompts = true;
        }
        let route = route?;
        let srun = route
            .node
            .as_ref()
            .is_some_and(|n| n.last_hop == LastHop::SrunOverlap);
        // Only the root's socket is forwarded. A node-local one is in a directory that its job
        // removes when it ends, and that someone else on the node could make again: a forward
        // checks nothing on the far side, where the bridge checks who listens.
        #[cfg(unix)]
        let forwardable = self.master
            && !srun
            && !self.forbidden
            && !self.prefer_stdio
            && route.socket == self.daemon.target.layout().socket()
            && forward::fits(&route.socket);
        #[cfg(not(unix))]
        let forwardable = {
            let _ = srun;
            false
        };
        if !forwardable {
            // A node's link is not made patient for a forward that will not be.
            self.forward_known = false;
        }
        // A job's node, reached with ssh: a link of its own, through the login link.
        let node_ssh = route
            .node
            .as_ref()
            .filter(|n| n.last_hop == LastHop::Ssh)
            .map(|n| n.name.clone());
        match &node_ssh {
            Some(name) => {
                route::check_not_an_alias(name, &self.aliases())
                    .map_err(|why| Failure::new(Kind::Refused, why))?;
                self.node_link(name).await?;
            }
            None => self.stop_node().await,
        }
        #[cfg(unix)]
        if forwardable {
            match self.try_forward(&route).await {
                Ok(active) => {
                    self.forward_known = true;
                    self.shared.remember(Transport::Forwarded);
                    return Ok(active);
                }
                Err(forward::Broken::Forbidden) => {
                    // The site's choice: remember it, and use the bridge from now on.
                    self.forbidden = true;
                    self.forward_known = false;
                    self.shared.remember(Transport::Stdio);
                }
                // Something else (no daemon behind it, say): the bridge says what. Not
                // remembered: the forward is tried again next time; made anew if its local
                // socket is what failed.
                Err(broken) => {
                    self.forward_known = false;
                    if matches!(broken, forward::Broken::Failed(_)) {
                        self.drop_forward();
                    }
                }
            }
        }
        // The bridge it is. A patient link would notice a lost network only after 30 s, and
        // nothing else watches a stdio transport: start the daemon's link again, impatient.
        self.forward_known = false;
        if self.daemon_link().is_some_and(Link::patient) {
            match &node_ssh {
                Some(name) => {
                    self.stop_node().await;
                    self.node_link(name).await?;
                }
                None => self.start_login().await?,
            }
        }
        let active = Arc::new(self.stdio(&route, node_ssh.as_deref()));
        // The first connection proves the way (within `bridge_wait`, prompts excluded).
        match active.open().await {
            Ok(stream) => drop(stream),
            Err(e) => {
                let doing = format!("reaching the helper on {}", self.target_name(&route));
                return Err(Failure::tunnel(&e, &doing));
            }
        }
        if self.ssh.is_wsl() {
            self.shared.remember(Transport::Stdio);
        }
        Ok(active)
    }

    /// Where the daemon is, asked through the login link.
    async fn resolve(&mut self) -> Result<Route, Failure> {
        self.resolving().await
    }

    /// [`Self::resolve`] as a future of its own, which borrows nothing: watching runs it beside
    /// the rest.
    fn resolving(&self) -> Resolving {
        let daemon = self.daemon.clone();
        let target = self.status_target();
        let master = self.master;
        Box::pin(async move {
            let host = daemon.host().to_owned();
            let asking = route::resolve(&daemon, &target);
            let asked = if master {
                match tokio::time::timeout(STATUS_WAIT, asking).await {
                    Ok(asked) => asked,
                    Err(_) => {
                        return Err(Failure::new(
                            Kind::Transient,
                            format!(
                                "asking {host} where the helper is: no answer within \
                                 {STATUS_WAIT:?}"
                            ),
                        ));
                    }
                }
            } else {
                asking.await
            };
            asked.map_err(|no| no_route(no, &host))
        })
    }

    /// The target, reached through the login link's master (Unix), or as given (Windows).
    fn status_target(&self) -> crate::Target {
        let ssh = match self.login.as_ref().and_then(Link::control) {
            Some(control) => self.ssh.through_master(control),
            None => self.ssh.clone(),
        };
        self.daemon.target.with_ssh(ssh)
    }

    /// The user's concrete `Host` names (see [`ConnectorOptions::ssh_config`]).
    fn aliases(&self) -> Vec<String> {
        let home = crate::home_dir().unwrap_or_default();
        match &self.options.ssh_config {
            Some(path) => crate::list_hosts_in(path, &home).hosts,
            None => crate::list_hosts().hosts,
        }
    }

    /// Makes sure the link to `node` runs, replacing one to another node (and its forward).
    async fn node_link(&mut self, name: &str) -> Result<(), Failure> {
        if let Some((current, link)) = &mut self.node
            && current == name
            && link.alive()
        {
            return Ok(());
        }
        self.stop_node().await;
        let login = self.daemon.host().to_owned();
        let control = self
            .login
            .as_ref()
            .and_then(Link::control)
            .map(Path::to_path_buf);
        let via = match &control {
            Some(control) => Via::Master {
                control,
                login: &login,
            },
            None => Via::Jump(&login),
        };
        let link = Link::start(
            LinkSpec {
                ssh: &self.ssh,
                dir: &self.dir.path,
                name: "node",
                host: name,
                via: Some(via),
                master: self.master,
                patient: self.patient(),
            },
            self.options.link_wait,
        )
        .await
        .map_err(|e| Failure::ssh(&e, &format!("connecting to {name} through {login}")))?;
        self.node = Some((name.to_owned(), link));
        Ok(())
    }

    /// The link that reaches the daemon's host: the node's, else the login one.
    fn daemon_link(&self) -> Option<&Link> {
        match &self.node {
            Some((_, link)) => Some(link),
            None => self.login.as_ref(),
        }
    }

    fn target_name(&self, route: &Route) -> String {
        match &route.node {
            Some(node) => format!("{} (job {})", node.name, node.job),
            None => self.daemon.host().to_owned(),
        }
    }

    /// Adds (or finds) the forward on the daemon's link and checks it.
    #[cfg(unix)]
    async fn try_forward(&mut self, route: &Route) -> Result<Arc<Active>, forward::Broken> {
        let no_link = || forward::Broken::Failed("no link".to_owned());
        let (master, log_from, host) = {
            let link = self.daemon_link().ok_or_else(no_link)?;
            let master = link.control().ok_or_else(no_link)?.to_path_buf();
            (master, link.log_len(), link.host().to_owned())
        };
        // A master keeps its forwards: one it already has for this socket is used again.
        let local = match &self.forward {
            Some(f) if f.master == master && f.remote == route.socket => f.local.clone(),
            _ => {
                self.drop_forward();
                let local = self.dir.path.join(format!("f{}", self.forwards));
                self.forwards = self.forwards.wrapping_add(1);
                let link = self.daemon_link().ok_or_else(no_link)?;
                forward::add(&self.ssh, &self.dir.path, link, &local, &route.socket)
                    .await
                    .map_err(|e| forward::Broken::Failed(e.to_string()))?;
                self.forward = Some(Forwarding {
                    local: local.clone(),
                    remote: route.socket.clone(),
                    master,
                });
                local
            }
        };
        let link = self.daemon_link().ok_or_else(no_link)?;
        forward::check(&local, link, log_from, self.options.probe_timeout).await?;
        Ok(Arc::new(Active {
            transport: Transport::Forwarded,
            route: route.clone(),
            local: Some(local),
            ssh: self.ssh.clone(),
            dir: self.dir.path.clone(),
            host,
            argv: Vec::new(),
            framed: false,
            extra: Vec::new(),
            wait: self.options.bridge_wait,
            closing: self.shared.closing.subscribe(),
        }))
    }

    /// How to run the bridge for `route`.
    fn stdio(&self, route: &Route, node_ssh: Option<&str>) -> Active {
        let login = self.daemon.host().to_owned();
        let bridge = vec![
            route.bridge.clone(),
            "connect".to_owned(),
            "--socket".to_owned(),
            route.socket.clone(),
        ];
        let srun = route
            .node
            .as_ref()
            .filter(|node| node.last_hop == LastHop::SrunOverlap);
        let (host, argv) = match (srun, &route.node) {
            (Some(node), _) => {
                // A job step on the node; the bridge's output framed against srun's line
                // buffering. srun takes options from the login node's environment too: labels
                // before each line (`SLURM_LABELIO`), or stdio sent elsewhere, would break the
                // frames.
                let mut argv: Vec<String> = vec![
                    "exec".to_owned(),
                    "env".to_owned(),
                    "-u".to_owned(),
                    "SLURM_LABELIO".to_owned(),
                    "-u".to_owned(),
                    "SLURM_STDINMODE".to_owned(),
                    "-u".to_owned(),
                    "SLURM_STDOUTMODE".to_owned(),
                    "-u".to_owned(),
                    "SLURM_STDERRMODE".to_owned(),
                    "srun".to_owned(),
                    format!("--jobid={}", node.job),
                    "--overlap".to_owned(),
                    "--nodes=1".to_owned(),
                    "--ntasks=1".to_owned(),
                    format!("--nodelist={}", node.name),
                    "--quiet".to_owned(),
                ];
                argv.extend(bridge);
                (login.clone(), argv)
            }
            (None, Some(node)) => {
                let mut argv = vec!["exec".to_owned()];
                argv.extend(bridge);
                (node.name.clone(), argv)
            }
            (None, None) => {
                let mut argv = vec!["exec".to_owned()];
                argv.extend(bridge);
                (login.clone(), argv)
            }
        };
        let (ssh, extra) = match self.daemon_link().and_then(Link::control) {
            Some(control) => (self.ssh.through_master(control), Vec::new()),
            // Without a master each connection logs in; a node through the login node.
            None => (
                self.ssh.clone(),
                match node_ssh {
                    Some(_) => vec!["-J".to_owned(), login],
                    None => Vec::new(),
                },
            ),
        };
        Active {
            transport: Transport::Stdio,
            route: route.clone(),
            #[cfg(unix)]
            local: None,
            ssh,
            dir: self.dir.path.clone(),
            host,
            argv,
            framed: srun.is_some(),
            extra,
            wait: self.options.bridge_wait,
            closing: self.shared.closing.subscribe(),
        }
    }

    /// Watches the way while connected (see the module docs of [`super`]).
    async fn monitor(
        &mut self,
        active: &Arc<Active>,
        events: &mut mpsc::UnboundedReceiver<Event>,
    ) -> End {
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut last_check = Instant::now();
        // Probes are Unix's (forwarded sockets).
        #[cfg_attr(not(unix), allow(unused_mut))]
        let mut last_probe = Instant::now();
        let mut last_suspect: Option<Instant> = None;
        let mut last_route: Option<Instant> = None;
        // Asked for within their gaps: done once the gap is over, not dropped.
        let mut suspect_due = false;
        let mut route_due = false;
        // The endpoint check under way. It runs beside the rest (it may take a while), so the
        // links and the probes are still watched meanwhile.
        let mut routing: Option<Resolving> = None;
        // Since when the forwarded socket has been silent.
        #[cfg_attr(not(unix), allow(unused_mut))]
        let mut silent_since: Option<Instant> = None;
        // Only a forwarded socket is probed: anything else would be a session (MaxSessions) or
        // a job step.
        let probing = active.transport == Transport::Forwarded;
        loop {
            let mut asked = Asked::default();
            let mut routed = None;
            {
                let login_host = self.daemon.host().to_owned();
                let Self { login, node, .. } = &mut *self;
                let login_exit = async {
                    match login {
                        Some(link) => link.exited().await,
                        None => std::future::pending().await,
                    }
                };
                let node_exit = async {
                    match node {
                        Some((name, link)) => (name.clone(), link.exited().await),
                        None => std::future::pending().await,
                    }
                };
                let route_checked = async {
                    match routing.as_mut() {
                        Some(check) => check.as_mut().await,
                        None => std::future::pending().await,
                    }
                };
                tokio::select! {
                    result = route_checked => routed = Some(result),
                    reason = login_exit => {
                        return End::Lost {
                            reason: format!("lost the connection to {login_host}: {reason}"),
                            keep_login: false,
                        };
                    }
                    (name, reason) = node_exit => {
                        return End::Lost {
                            reason: format!("lost the connection to {name}: {reason}"),
                            keep_login: true,
                        };
                    }
                    event = events.recv() => {
                        if let Some(end) = take(event, &mut asked) {
                            return end;
                        }
                    }
                    _ = tick.tick() => {
                        asked.check = last_check.elapsed() >= self.options.check_every;
                        // While the socket is silent, asked again each second: the state comes
                        // back as soon as the answers do.
                        let every = match silent_since {
                            Some(_) => Duration::from_secs(1),
                            None => self.options.probe_every,
                        };
                        asked.probe = probing && last_probe.elapsed() >= every;
                    }
                }
            }
            if let Some(result) = routed {
                routing = None;
                match result {
                    Ok(route) if route == active.route => {}
                    Ok(_) => {
                        return End::Lost {
                            reason: "the helper moved".to_owned(),
                            keep_login: true,
                        };
                    }
                    // Busy (no session free now) or slow: the connections say more later.
                    Err(failure) if failure.kind == Kind::Transient => {}
                    Err(failure) => {
                        return End::Lost {
                            reason: failure.reason,
                            keep_login: true,
                        };
                    }
                }
            }
            // Whatever else is queued is part of the same burst.
            while let Ok(event) = events.try_recv() {
                if let Some(end) = take(Some(event), &mut asked) {
                    return end;
                }
            }
            if self.clock.jumped() {
                asked.check = true;
                asked.probe = probing;
            }
            suspect_due |= asked.link_suspect;
            if suspect_due && last_suspect.is_none_or(|t| t.elapsed() >= SUSPECT_GAP) {
                suspect_due = false;
                last_suspect = Some(Instant::now());
                asked.check = true;
                asked.probe = probing;
            }
            if asked.check {
                last_check = Instant::now();
                if let Err(reason) = self.check_masters().await {
                    return End::Lost {
                        reason,
                        keep_login: false,
                    };
                }
            }
            #[cfg(unix)]
            if asked.probe && probing {
                last_probe = Instant::now();
                match self.probe_forward(active).await {
                    Ok(()) => {
                        if silent_since.take().is_some() {
                            self.shared.set(LinkState::Connected {
                                transport: active.transport,
                            });
                        }
                    }
                    // The server answered, but nothing answers behind the socket.
                    Err(forward::Broken::Closed) => asked.route = true,
                    Err(forward::Broken::Forbidden) => {
                        self.forbidden = true;
                        self.forward_known = false;
                        self.shared.remember(Transport::Stdio);
                        return End::Lost {
                            reason: "the site now forbids forwarding unix sockets".to_owned(),
                            keep_login: true,
                        };
                    }
                    // The local socket is gone: its master, if that is dead too; else the
                    // forward alone, added again with the login kept (no new sign-in).
                    Err(forward::Broken::Failed(why)) => {
                        if let Err(reason) = self.check_masters().await {
                            return End::Lost {
                                reason,
                                keep_login: false,
                            };
                        }
                        self.drop_forward();
                        return End::Lost {
                            reason: format!("the forwarded socket failed: {why}"),
                            keep_login: true,
                        };
                    }
                    Err(forward::Broken::Silent) => {
                        let since = *silent_since.get_or_insert_with(|| {
                            // Unverifiable at once; the link waits for a longer silence.
                            self.shared.set(LinkState::Unverifiable {
                                reason: format!(
                                    "no answer from the helper on {} for now",
                                    active.host
                                ),
                            });
                            Instant::now()
                        });
                        if since.elapsed() >= SILENCE {
                            return End::Lost {
                                reason: format!(
                                    "no answer from the helper on {} for {SILENCE:?}",
                                    active.host
                                ),
                                keep_login: false,
                            };
                        }
                    }
                }
            }
            route_due |= asked.route;
            if route_due && routing.is_none() && last_route.is_none_or(|t| t.elapsed() >= ROUTE_GAP)
            {
                route_due = false;
                last_route = Some(Instant::now());
                routing = Some(self.resolving());
            }
        }
    }

    /// Asks each master whether it lives (`ssh -O check`; Unix).
    async fn check_masters(&self) -> Result<(), String> {
        #[cfg(unix)]
        {
            let links = self
                .login
                .iter()
                .chain(self.node.as_ref().map(|(_, link)| link));
            for link in links {
                if let Some(control) = link.control() {
                    let checked = super::link::control(
                        &self.ssh,
                        &self.dir.path,
                        control,
                        link.host(),
                        "check",
                        &[],
                    )
                    .await;
                    if checked.is_err() {
                        return Err(format!(
                            "the connection to {} stopped answering its check",
                            link.host()
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    /// One request through the forwarded socket.
    #[cfg(unix)]
    async fn probe_forward(&self, active: &Active) -> Result<(), forward::Broken> {
        let (Some(local), Some(link)) = (&active.local, self.daemon_link()) else {
            return Err(forward::Broken::Failed("no forwarded socket".to_owned()));
        };
        forward::check(local, link, link.log_len(), self.options.probe_timeout).await
    }

    /// Stops the node's link (and its forward), or all links.
    async fn stop(&mut self, keep_login: bool) {
        if keep_login {
            self.stop_node().await;
        } else {
            self.stop_links().await;
        }
    }

    /// Stops the node's link, and a forward on it.
    async fn stop_node(&mut self) {
        if let Some((_, link)) = self.node.take() {
            #[cfg(unix)]
            if self
                .forward
                .as_ref()
                .is_some_and(|f| Some(f.master.as_path()) == link.control())
            {
                self.drop_forward();
            }
            link.stop().await;
        }
    }

    /// Forgets the forward (its master is going, or it is replaced), removing its socket.
    #[cfg(unix)]
    fn drop_forward(&mut self) {
        if let Some(forward) = self.forward.take() {
            let _ = std::fs::remove_file(forward.local);
        }
    }

    /// Stops the links (the node's first), and with them every connection.
    async fn stop_links(&mut self) {
        self.stop_node().await;
        if let Some(link) = self.login.take() {
            link.stop().await;
        }
        #[cfg(unix)]
        self.drop_forward();
    }

    /// Stops the masters of connectors that died (their directory's lock is free) and removes
    /// their directories (Unix; on Windows the Job Object ends a dead app's ssh, and its
    /// directory, logs only, stays).
    async fn sweep_stale(&self) {
        #[cfg(unix)]
        {
            let Some(base) = self.dir.path.parent() else {
                return;
            };
            let Ok(entries) = std::fs::read_dir(base) else {
                return;
            };
            for entry in entries.filter_map(Result::ok) {
                let path = entry.path();
                let name = entry.file_name();
                let ours = name.to_str().is_some_and(|n| {
                    n.strip_prefix('t').is_some_and(|hex| {
                        hex.len() >= 8 && hex.bytes().all(|b| b.is_ascii_hexdigit())
                    })
                });
                if !ours || path == self.dir.path || !is_stale(&path) {
                    continue;
                }
                for socket in ["login", "node"] {
                    let control = path.join(socket);
                    if super::link::is_socket(&control) {
                        let _ = super::link::control(
                            &self.ssh,
                            &self.dir.path,
                            &control,
                            "pitcrew-stale",
                            "exit",
                            &[],
                        )
                        .await;
                    }
                }
                let _ = std::fs::remove_dir_all(&path);
            }
        }
    }
}

/// The failure for an endpoint check of `host` that found no route.
fn no_route(no: NoRoute, host: &str) -> Failure {
    match no {
        NoRoute::Waiting(why) => Failure::new(Kind::Waiting, why),
        NoRoute::NotRunning(why) => Failure::new(Kind::NotRunning, why),
        NoRoute::Invalid(why) => Failure::new(
            Kind::Refused,
            format!("the helper's record on {host} failed a check: {why}"),
        ),
        NoRoute::Failed(error) => match &error {
            HelperError::Ssh(e) => Failure::ssh(e, &format!("asking {host} where the helper is")),
            HelperError::UnsafeDirectory(_) | HelperError::InvalidArgument(_) => Failure::new(
                Kind::Refused,
                format!("asking {host} where the helper is: {error}"),
            ),
            _ => Failure::new(
                Kind::Transient,
                format!("asking {host} where the helper is: {error}"),
            ),
        },
    }
}

/// Adds what `event` asks for; `Some` for an event that ends the watching.
fn take(event: Option<Event>, asked: &mut Asked) -> Option<End> {
    match event {
        None | Some(Event::Close) => return Some(End::Close),
        Some(Event::SignIn(reason)) => return Some(End::SignIn(reason)),
        Some(Event::Wake) => {
            asked.check = true;
            asked.probe = true;
        }
        Some(Event::LinkSuspect) => asked.link_suspect = true,
        Some(Event::RouteSuspect) => asked.route = true,
        // Connected: nothing to retry.
        Some(Event::Retry) => {}
    }
    None
}

/// Whether the connector directory `dir` is a dead connector's: its lock is free, or (made by
/// an older connector, or being made) it has no lock file and is a minute old.
#[cfg(unix)]
fn is_stale(dir: &Path) -> bool {
    let lock = dir.join("lock");
    match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&lock)
    {
        Ok(file) => {
            rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive).is_ok()
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => std::fs::symlink_metadata(dir)
            .and_then(|m| m.modified())
            .is_ok_and(|at| at.elapsed().is_ok_and(|age| age >= STALE_UNLOCKED)),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failures_are_sorted() {
        let sign_in = [
            SshError::AuthFailed {
                stderr: String::new(),
            },
            SshError::HostKeyChanged {
                stderr: String::new(),
            },
            SshError::Cancelled,
        ];
        for e in &sign_in {
            assert_eq!(Failure::ssh(e, "x").kind, Kind::SignIn, "{e:?}");
        }
        let transient = [
            SshError::ConnectTimeout {
                stderr: String::new(),
            },
            SshError::Unreachable {
                stderr: String::new(),
            },
            SshError::TimedOut(Duration::from_secs(1)),
            SshError::SessionRefused {
                stderr: String::new(),
            },
        ];
        for e in &transient {
            assert_eq!(Failure::ssh(e, "x").kind, Kind::Transient, "{e:?}");
        }
        assert_eq!(
            Failure::tunnel(&TunnelError::Refused("x".into()), "y").kind,
            Kind::Refused
        );
        assert_eq!(
            Failure::tunnel(&TunnelError::NoDaemon("x".into()), "y").kind,
            Kind::NotRunning
        );
        let refused = TunnelError::Ssh(SshError::SessionRefused {
            stderr: String::new(),
        });
        let failure = Failure::tunnel(&refused, "y");
        assert_eq!(failure.kind, Kind::Transient);
        assert!(failure.reason.contains("MaxSessions"), "{}", failure.reason);
    }

    #[cfg(unix)]
    #[test]
    fn private_dirs_are_new_and_locked() {
        let tmp = pitcrew_fixtures::temp::short_tempdir().unwrap();
        let ssh = Ssh::new("ssh").with_runtime_dir(tmp.path().join("rt"));
        let a = PrivateDir::new(&ssh).unwrap();
        let name = a.path.file_name().unwrap().to_str().unwrap().to_owned();
        assert_eq!(name.len(), 1 + 16, "{name}");
        // Its lock is held: not stale.
        assert!(!is_stale(&a.path));
        let path = a.path.clone();
        drop(a);
        assert!(!path.exists());
        // A dead connector's directory: its lock file is there and free.
        let b = PrivateDir::new(&ssh).unwrap();
        let dead = tmp.path().join("rt").join("t0123456789abcdef");
        std::fs::create_dir(&dead).unwrap();
        std::fs::write(dead.join("lock"), "").unwrap();
        assert!(is_stale(&dead));
        // A new one without a lock file yet is not.
        let young = tmp.path().join("rt").join("tfedcba9876543210");
        std::fs::create_dir(&young).unwrap();
        assert!(!is_stale(&young));
        drop(b);
    }

    /// A sweep that comes while a connector is taking its lock (the file made, not locked yet)
    /// finds no free `lock` to take.
    #[cfg(unix)]
    #[test]
    fn a_lock_being_taken_is_never_free() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("t0123456789abcdef");
        std::fs::create_dir(&dir).unwrap();
        let file = lock_with(&dir, || {
            assert!(!is_stale(&dir), "stale while being locked")
        })
        .unwrap()
        .unwrap();
        assert!(!is_stale(&dir));
        assert!(!dir.join("lock.new").exists());
        drop(file);
        assert!(is_stale(&dir));
    }
}
