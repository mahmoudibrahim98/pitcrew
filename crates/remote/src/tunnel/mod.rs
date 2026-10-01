//! The tunnel: byte streams from the laptop to the person's `pitcrewd` on a remote machine (a
//! login node, or a compute node inside a SLURM job), over system OpenSSH, noticing within ten
//! seconds when the way is lost and recovering by itself.
//!
//! ```text
//! let daemon = Daemon::new(target, Arc::new(launcher)).with_last_hop(site.last_hop());
//! let connector = Connector::start(daemon, ConnectorOptions::default())?;
//! let mut state = connector.watch();                  // connecting, connected, unverifiable, …
//! let stream = connector.connect().await?;            // one HTTP or WebSocket connection's bytes
//! connector.close().await;
//! ```
//!
//! **The link.** A connector keeps its own ssh connection to the machine (`ssh -N`): on Unix a
//! ControlMaster in a private directory of its own, so the machine is logged in to once per
//! (re)connection and every connection or check is a channel of it; on Windows (no
//! ControlMaster) a heartbeat. Its keepalives ([`KEEPALIVE_INTERVAL`], [`KEEPALIVE_COUNT`]) end
//! it within six seconds of silence. The endpoint check and the stdio bridges read no ssh config
//! of their own (`-F none`): the master has the user's.
//!
//! **Where the daemon is** comes from the endpoint record, asked through the link with the
//! launcher's `status` before every (re)connection, and checked: the socket's path and the
//! helper's version (they reach ssh's command line), and for a job that it is still ours and
//! running, that its endpoint names it, and that the node squeue names is the one recorded and
//! has a plain name. A record that fails is never used. A job's node is reached as the site
//! recipe says ([`crate::LastHop`]): with ssh through the login link (Unix: a `ProxyCommand`
//! that is a channel of the login link, so the login node is not logged in to again; Windows:
//! `-J`), or with `srun --jobid <id> --overlap` on the login node, which also reaches a socket
//! on the node's own disk.
//!
//! **Two transports** ([`Transport`]), chosen per machine:
//! - **Forwarded** (Unix): the link's master listens on a socket in the connector's 0700
//!   directory and forwards each connection to the daemon's socket. Connections share it. A site
//!   that forbids it (`AllowStreamLocalForwarding no`: ssh logs "administratively prohibited")
//!   is remembered, and the connector falls back.
//! - **Stdio**: each connection runs `pitcrewd connect` ([`crate::bridge`]) on the machine, a
//!   channel of the link on Unix. Always used on Windows: OpenSSH there forwards no unix
//!   sockets, and neither std nor tokio has them on Windows; a TCP port instead would be open to
//!   every local user. Always used through `srun`.
//!
//! The choice is in [`LinkState::Connected`] and [`Connector::transport`]; pass it back as
//! [`ConnectorOptions::transport`] to remember it across runs.
//!
//! **The ladder.** Connected, the connector watches the link's exit (keepalives), asks the
//! master (`ssh -O check`, Unix) every [`ConnectorOptions::check_every`], and probes through the
//! transport every [`ConnectorOptions::probe_every`] (Unix, where a probe is a channel; on
//! Windows it would be a login, so the keepalives watch alone), and at once after a failed
//! connection or [`Connector::wake`]. A wall-clock jump against the monotonic clock (the laptop
//! slept) probes at once too. Any failure makes it [`LinkState::Unverifiable`], stops the link,
//! and starts again: at once if the connection had held for
//! [`ConnectorOptions::backoff_max`], else after a back-off; then with back-off and jitter from
//! [`ConnectorOptions::backoff_min`] to `backoff_max`, the endpoint asked afresh each time (a
//! job that moved is followed), until [`ConnectorOptions::give_up_after`] makes it
//! [`LinkState::Unreachable`] (it keeps trying every [`ConnectorOptions::retry_every`]). A helper
//! that is not running (or a job that ended) is unreachable at once and asked about again every
//! `retry_every`; a failed sign-in or a cancelled prompt waits for [`Connector::retry`]. Prompts
//! while reconnecting go through the askpass bridge as for any call; nothing is stored.
//! Resuming the API stream (`since=`) is the caller's.
//!
//! **Windows** works, with fewer comforts: no ControlMaster, so each connection (and each probe)
//! logs in, and a password or one-time code is asked each time; keys are the way there. Its
//! tests have not run on Windows (only clippy for the target).
//!
//! **Security**, as for every call: agent and X11 forwarding, local commands and the user's
//! configured forwardings are off; every `-o` is ours; whatever comes from the machine passes the
//! checks above before it reaches an ssh command line; local sockets are in a private 0700
//! directory, removed on close; ssh gets only [`crate::MINIMAL_ENV`] (and what
//! [`crate::Ssh::with_env_passthrough`] adds). Reasons never carry socket paths or secrets.

#[cfg(unix)]
mod forward;
mod link;
mod route;
mod stdio;

pub use link::{KEEPALIVE_COUNT, KEEPALIVE_INTERVAL};
pub use route::Daemon;

use crate::helper::HelperError;
use crate::helper::slurm::LastHop;
use crate::{Ssh, SshError};
use link::{Link, LinkSpec, Via};
use route::{NoRoute, Route};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant, SystemTime};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::{mpsc, watch};

/// A clock difference this large between the wall clock and the monotonic one, over one tick,
/// means the laptop slept (or its clock was set): check the link at once.
const JUMP: Duration = Duration::from_secs(5);

/// How long the endpoint check may take (squeue may be slow), prompts excluded.
const STATUS_WAIT: Duration = Duration::from_secs(60);

/// How connections reach the daemon.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Transport {
    /// A unix socket the link forwards (`-L`). Unix only.
    Forwarded,
    /// `pitcrewd connect` on the machine, one per connection.
    Stdio,
}

/// Why a machine is unreachable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Unreachable {
    /// No way through for [`ConnectorOptions::give_up_after`]. Still tried every
    /// [`ConnectorOptions::retry_every`].
    Network,
    /// Signing in failed: authentication, a host key, or a prompt the person cancelled. Waits
    /// for [`Connector::retry`].
    SignIn,
    /// The helper is not running there, or its job ended (or is still queued). Asked about again
    /// every [`ConnectorOptions::retry_every`], so a new endpoint is picked up.
    NotRunning,
    /// Something failed a check: an endpoint record, or the bridge refused the socket. Asked
    /// about again every [`ConnectorOptions::retry_every`].
    Refused,
}

/// Where a connector stands.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum LinkState {
    /// Starting: not connected yet, and no failure yet.
    Connecting,
    /// Connections go through.
    Connected {
        /// How.
        transport: Transport,
    },
    /// The way was lost, or does not work yet; the connector is trying again.
    Unverifiable {
        /// Why, for people.
        reason: String,
    },
    /// The machine cannot be reached (see [`Unreachable`] for what happens next).
    Unreachable {
        /// What kind of failure.
        why: Unreachable,
        /// Why, for people.
        reason: String,
    },
    /// [`Connector::close`] was called, or the connector dropped.
    Closed,
}

impl LinkState {
    /// Whether connections go through.
    #[must_use]
    pub fn is_connected(&self) -> bool {
        matches!(self, Self::Connected { .. })
    }
}

impl fmt::Display for LinkState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Connecting => f.write_str("connecting"),
            Self::Connected { transport } => write!(f, "connected ({transport:?})"),
            Self::Unverifiable { reason } => write!(f, "unverifiable: {reason}"),
            Self::Unreachable { reason, .. } => write!(f, "unreachable: {reason}"),
            Self::Closed => f.write_str("closed"),
        }
    }
}

/// Where the connector reads the wall clock, to notice that the laptop slept. Tests, and
/// embedders with a clock of their own, replace it.
#[derive(Clone)]
pub struct WallClock(Arc<dyn Fn() -> SystemTime + Send + Sync>);

impl WallClock {
    /// A clock read by calling `now`.
    pub fn new(now: impl Fn() -> SystemTime + Send + Sync + 'static) -> Self {
        Self(Arc::new(now))
    }

    fn now(&self) -> SystemTime {
        (self.0)()
    }
}

impl Default for WallClock {
    /// [`SystemTime::now`].
    fn default() -> Self {
        Self::new(SystemTime::now)
    }
}

impl fmt::Debug for WallClock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("WallClock")
    }
}

/// How a [`Connector`] behaves. The defaults suit a desktop.
#[derive(Clone, Debug)]
pub struct ConnectorOptions {
    /// The transport to use, as remembered from an earlier run; `None` chooses (forwarded
    /// where it works, else stdio). A forwarded socket the site refuses still falls back.
    pub transport: Option<Transport>,
    /// How long a link may take to be ready, prompts excluded. Default 30 s.
    pub link_wait: Duration,
    /// How long [`Connector::connect`] waits for the link while it is (re)connecting. Default
    /// 10 s.
    pub connect_wait: Duration,
    /// How long a stdio connection may take to start (srun may be slow), prompts excluded.
    /// Default 20 s.
    pub bridge_wait: Duration,
    /// How often the master is asked whether it lives (`ssh -O check`; Unix). Default 5 s.
    pub check_every: Duration,
    /// How often a connection is tried through the transport (Unix; see the module docs).
    /// Default 30 s.
    pub probe_every: Duration,
    /// How long that try may take. Default 5 s.
    pub probe_timeout: Duration,
    /// The first wait between attempts. Default 1 s.
    pub backoff_min: Duration,
    /// The longest wait between attempts. Default 30 s.
    pub backoff_max: Duration,
    /// How long attempts may fail before the machine is unreachable. Default 2 minutes.
    pub give_up_after: Duration,
    /// Once unreachable (or the helper not running), how often to try again. Default 1 minute.
    pub retry_every: Duration,
    /// The wall clock (see [`WallClock`]).
    pub wall_clock: WallClock,
}

impl Default for ConnectorOptions {
    fn default() -> Self {
        Self {
            transport: None,
            link_wait: Duration::from_secs(30),
            connect_wait: Duration::from_secs(10),
            bridge_wait: Duration::from_secs(20),
            check_every: Duration::from_secs(5),
            probe_every: Duration::from_secs(30),
            probe_timeout: Duration::from_secs(5),
            backoff_min: Duration::from_secs(1),
            backoff_max: Duration::from_secs(30),
            give_up_after: Duration::from_secs(120),
            retry_every: Duration::from_secs(60),
            wall_clock: WallClock::default(),
        }
    }
}

/// Why a connection could not be made.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TunnelError {
    /// The connector is not connected (unreachable, closed, or still trying after
    /// [`ConnectorOptions::connect_wait`]).
    #[error("not connected: {0}")]
    NotConnected(LinkState),
    /// An ssh call failed.
    #[error(transparent)]
    Ssh(#[from] SshError),
    /// The bridge refused the daemon's socket (not this user's alone), or the helper could not
    /// run it.
    #[error("the bridge refused: {0}")]
    Refused(String),
    /// No daemon listens on the socket, or the helper is not there.
    #[error("no daemon answers: {0}")]
    NoDaemon(String),
    /// The bridge failed otherwise.
    #[error("the bridge failed: {0}")]
    Bridge(String),
    /// A local socket failed.
    #[error(transparent)]
    Io(#[from] io::Error),
    /// The way the connector chose does not exist on this system.
    #[error("unsupported: {0}")]
    Unsupported(String),
}

/// One connection to the daemon: a byte stream. Shutting down its write side half-closes it
/// (the daemon reads end of file, and its answer still comes). Dropping it closes it.
#[derive(Debug)]
pub struct TunnelStream {
    inner: Stream,
}

#[derive(Debug)]
enum Stream {
    #[cfg(unix)]
    Socket(tokio::net::UnixStream),
    Stdio(Box<stdio::StdioStream>),
}

impl TunnelStream {
    /// How it goes.
    #[must_use]
    pub fn transport(&self) -> Transport {
        match &self.inner {
            #[cfg(unix)]
            Stream::Socket(_) => Transport::Forwarded,
            Stream::Stdio(_) => Transport::Stdio,
        }
    }
}

impl AsyncRead for TunnelStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match &mut self.get_mut().inner {
            #[cfg(unix)]
            Stream::Socket(s) => Pin::new(s).poll_read(cx, buf),
            Stream::Stdio(s) => Pin::new(s).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for TunnelStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match &mut self.get_mut().inner {
            #[cfg(unix)]
            Stream::Socket(s) => Pin::new(s).poll_write(cx, buf),
            Stream::Stdio(s) => Pin::new(s).poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match &mut self.get_mut().inner {
            #[cfg(unix)]
            Stream::Socket(s) => Pin::new(s).poll_flush(cx),
            Stream::Stdio(s) => Pin::new(s).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match &mut self.get_mut().inner {
            #[cfg(unix)]
            Stream::Socket(s) => Pin::new(s).poll_shutdown(cx),
            Stream::Stdio(s) => Pin::new(s).poll_shutdown(cx),
        }
    }
}

/// What the supervisor is told.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Event {
    /// Check now: the computer woke, or the network changed.
    Wake,
    /// Start over now, whatever the state.
    Retry,
    /// A connection failed: probe now.
    Suspect,
    /// Stop.
    Close,
}

/// How connections are made while connected.
#[derive(Debug)]
struct Active {
    transport: Transport,
    /// Forwarded: the local socket.
    #[cfg(unix)]
    local: Option<PathBuf>,
    /// Stdio: how to run the bridge.
    ssh: Ssh,
    dir: PathBuf,
    host: String,
    argv: Vec<String>,
    extra: Vec<String>,
    wait: Duration,
}

impl Active {
    async fn open(&self) -> Result<TunnelStream, TunnelError> {
        match self.transport {
            #[cfg(unix)]
            Transport::Forwarded => {
                let Some(local) = &self.local else {
                    return Err(TunnelError::Unsupported("no forwarded socket".to_owned()));
                };
                let socket = tokio::net::UnixStream::connect(local).await?;
                Ok(TunnelStream {
                    inner: Stream::Socket(socket),
                })
            }
            #[cfg(not(unix))]
            Transport::Forwarded => Err(TunnelError::Unsupported(
                "forwarded sockets need unix sockets on this computer".to_owned(),
            )),
            Transport::Stdio => {
                let stream = stdio::open(stdio::Opening {
                    ssh: &self.ssh,
                    dir: &self.dir,
                    host: &self.host,
                    argv: self.argv.clone(),
                    extra: self.extra.clone(),
                    wait: self.wait,
                })
                .await?;
                Ok(TunnelStream {
                    inner: Stream::Stdio(Box::new(stream)),
                })
            }
        }
    }
}

/// What the connector and its supervisor share.
#[derive(Debug)]
struct Shared {
    host: String,
    state: watch::Sender<LinkState>,
    active: watch::Sender<Option<Arc<Active>>>,
    transport: Mutex<Option<Transport>>,
    connect_wait: Duration,
}

impl Shared {
    fn set(&self, state: LinkState) {
        self.state.send_if_modified(|current| {
            let changed = *current != state;
            *current = state;
            changed
        });
    }
}

/// Keeps one machine's daemon reachable, and opens connections to it. Cheap to clone; the
/// connector stops (as [`Connector::close`]) when the last clone is dropped.
#[derive(Clone)]
pub struct Connector {
    inner: Arc<Inner>,
}

struct Inner {
    shared: Arc<Shared>,
    events: mpsc::UnboundedSender<Event>,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        let _ = self.events.send(Event::Close);
    }
}

impl fmt::Debug for Connector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Connector")
            .field("host", &self.inner.shared.host)
            .field("state", &*self.inner.shared.state.borrow())
            .finish()
    }
}

impl Connector {
    /// Starts keeping `daemon` reachable, in the background. Must be called inside a tokio
    /// runtime.
    ///
    /// # Errors
    /// No tokio runtime, or the private directory could not be made.
    pub fn start(daemon: Daemon, options: ConnectorOptions) -> Result<Self, TunnelError> {
        let runtime = tokio::runtime::Handle::try_current()
            .map_err(|e| TunnelError::Unsupported(format!("no tokio runtime: {e}")))?;
        let ssh = daemon.target.ssh().minimal_env();
        let dir = PrivateDir::new(&ssh)?;
        let shared = Arc::new(Shared {
            host: daemon.host().to_owned(),
            state: watch::Sender::new(LinkState::Connecting),
            active: watch::Sender::new(None),
            transport: Mutex::new(options.transport),
            connect_wait: options.connect_wait,
        });
        let (events, inbox) = mpsc::unbounded_channel();
        let clock = Clock::new(&options.wall_clock);
        let supervisor = Supervisor {
            daemon,
            options,
            shared: shared.clone(),
            ssh,
            dir,
            login: None,
            node: None,
            #[cfg(unix)]
            forward: None,
            #[cfg(unix)]
            forwards: 0,
            forbidden: false,
            clock,
        };
        let task = runtime.spawn(supervisor.run(inbox));
        Ok(Self {
            inner: Arc::new(Inner {
                shared,
                events,
                task: Mutex::new(Some(task)),
            }),
        })
    }

    /// Opens one connection to the daemon. While the connector (re)connects, waits up to
    /// [`ConnectorOptions::connect_wait`] for it. A failure makes the connector check its way
    /// at once.
    ///
    /// # Errors
    /// [`TunnelError::NotConnected`] when unreachable, closed, or still not connected after the
    /// wait; else what opening failed with.
    pub async fn connect(&self) -> Result<TunnelStream, TunnelError> {
        let active = self.wait_active().await?;
        let opened = active.open().await;
        if opened.is_err() {
            let _ = self.inner.events.send(Event::Suspect);
        }
        opened
    }

    async fn wait_active(&self) -> Result<Arc<Active>, TunnelError> {
        let shared = &self.inner.shared;
        let mut active = shared.active.subscribe();
        let mut state = shared.state.subscribe();
        let waiting = async {
            loop {
                if let Some(active) = active.borrow_and_update().clone() {
                    return Ok(active);
                }
                let now = state.borrow_and_update().clone();
                if matches!(now, LinkState::Unreachable { .. } | LinkState::Closed) {
                    return Err(TunnelError::NotConnected(now));
                }
                tokio::select! {
                    changed = active.changed() => if changed.is_err() {
                        return Err(TunnelError::NotConnected(LinkState::Closed));
                    },
                    changed = state.changed() => if changed.is_err() {
                        return Err(TunnelError::NotConnected(LinkState::Closed));
                    },
                }
            }
        };
        match tokio::time::timeout(shared.connect_wait, waiting).await {
            Ok(result) => result,
            Err(_) => Err(TunnelError::NotConnected(self.state())),
        }
    }

    /// The state now.
    #[must_use]
    pub fn state(&self) -> LinkState {
        self.inner.shared.state.borrow().clone()
    }

    /// The state, as it changes.
    #[must_use]
    pub fn watch(&self) -> watch::Receiver<LinkState> {
        self.inner.shared.state.subscribe()
    }

    /// The transport in use or last used (or the one passed in [`ConnectorOptions::transport`]).
    #[must_use]
    pub fn transport(&self) -> Option<Transport> {
        self.inner.shared.transport.lock().ok().and_then(|t| *t)
    }

    /// The machine, as given to ssh.
    #[must_use]
    pub fn host(&self) -> &str {
        &self.inner.shared.host
    }

    /// Tells the connector the computer woke, or the network changed: it checks its way at once,
    /// and a pending retry happens now.
    pub fn wake(&self) {
        let _ = self.inner.events.send(Event::Wake);
    }

    /// Starts over now, also from [`LinkState::Unreachable`] (the person asked to).
    pub fn retry(&self) {
        let _ = self.inner.events.send(Event::Retry);
    }

    /// Stops: the link, and so every connection through it, ends; the private directory and
    /// its sockets are removed. Idempotent.
    pub async fn close(&self) {
        let _ = self.inner.events.send(Event::Close);
        let task = self.inner.task.lock().ok().and_then(|mut t| t.take());
        if let Some(task) = task {
            let _ = task.await;
        }
    }
}

/// The connector's private directory: `<runtime dir>/t<hex>`, 0700, removed when dropped.
#[derive(Debug)]
struct PrivateDir(PathBuf);

impl PrivateDir {
    fn new(ssh: &Ssh) -> Result<Self, TunnelError> {
        let base = ssh.runtime_dir_for(cfg!(unix))?;
        let tag = crate::askpass::random::<4>().map_err(SshError::Setup)?;
        let dir = base.join(format!("t{}", crate::askpass::to_hex(&tag)));
        crate::private::ensure_private_dir(&dir).map_err(SshError::Setup)?;
        Ok(Self(dir))
    }
}

impl Drop for PrivateDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Notices wall-clock jumps against the monotonic clock.
#[derive(Debug)]
struct Clock {
    wall_clock: WallClock,
    wall: SystemTime,
    mono: Instant,
}

impl Clock {
    fn new(wall_clock: &WallClock) -> Self {
        Self {
            wall_clock: wall_clock.clone(),
            wall: wall_clock.now(),
            mono: Instant::now(),
        }
    }

    /// Whether the clocks drifted apart by more than [`JUMP`] since the last call.
    fn jumped(&mut self) -> bool {
        let (wall, mono) = (self.wall_clock.now(), Instant::now());
        let jumped = jumped(self.wall, self.mono, wall, mono);
        self.wall = wall;
        self.mono = mono;
        jumped
    }
}

fn jumped(wall_then: SystemTime, mono_then: Instant, wall: SystemTime, mono: Instant) -> bool {
    let mono_passed = mono.saturating_duration_since(mono_then);
    let skew = match wall.duration_since(wall_then) {
        Ok(wall_passed) => wall_passed.abs_diff(mono_passed),
        Err(back) => back.duration().saturating_add(mono_passed),
    };
    skew > JUMP
}

/// The wait before attempt `attempt` (from 0): doubling from `min` up to `max`, then a random
/// point in its upper half (`jitter` is uniform over `u32`).
fn backoff(attempt: u32, min: Duration, max: Duration, jitter: u32) -> Duration {
    let factor = 1u32.checked_shl(attempt.min(20)).unwrap_or(u32::MAX);
    let base = min.saturating_mul(factor).min(max.max(min));
    let half = base / 2;
    half + half.mul_f64(f64::from(jitter) / f64::from(u32::MAX))
}

fn jitter() -> u32 {
    crate::askpass::random::<4>().map_or(u32::MAX / 2, u32::from_le_bytes)
}

/// Why an attempt failed, and what to do about it.
#[derive(Debug)]
struct Failure {
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

/// A forward on one of the links.
#[cfg(unix)]
#[derive(Debug)]
struct Forwarding {
    local: PathBuf,
    remote: String,
    master: PathBuf,
}

/// Owns the links and runs the ladder.
struct Supervisor {
    daemon: Daemon,
    options: ConnectorOptions,
    shared: Arc<Shared>,
    /// With the person's prompt handler and the minimal environment.
    ssh: Ssh,
    dir: PrivateDir,
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
    clock: Clock,
}

impl Supervisor {
    async fn run(mut self, mut events: mpsc::UnboundedReceiver<Event>) {
        let mut attempt: u32 = 0;
        let mut failing_since: Option<Instant> = None;
        loop {
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
            match outcome {
                Ok(active) => {
                    let connected_at = Instant::now();
                    if let Ok(mut remembered) = self.shared.transport.lock() {
                        *remembered = Some(active.transport);
                    }
                    let transport = active.transport;
                    self.shared.active.send_replace(Some(active.clone()));
                    self.shared.set(LinkState::Connected { transport });
                    let lost = self.monitor(&active, &mut events).await;
                    self.shared.active.send_replace(None);
                    let Some(reason) = lost else {
                        break;
                    };
                    self.shared.set(LinkState::Unverifiable { reason });
                    self.stop_links().await;
                    if connected_at.elapsed() >= self.options.backoff_max {
                        // It held: start over at once.
                        attempt = 0;
                        failing_since = None;
                        continue;
                    }
                    // It did not hold: back off, as after a failure (no storm of logins and
                    // prompts against a host that drops every connection).
                    let wait = backoff(
                        attempt,
                        self.options.backoff_min,
                        self.options.backoff_max,
                        jitter(),
                    );
                    attempt = attempt.saturating_add(1);
                    match self.pause(Some(wait), &mut events, false).await {
                        Resume::Close => break,
                        Resume::Asked => {
                            attempt = 0;
                            failing_since = None;
                        }
                        Resume::Timer => {}
                    }
                }
                Err(failure) => {
                    let since = *failing_since.get_or_insert_with(Instant::now);
                    match self.after(failure, since, &mut attempt, &mut events).await {
                        Resume::Close => break,
                        // Asked to (a retry, a wake, a clock jump): as from the start.
                        Resume::Asked => {
                            attempt = 0;
                            failing_since = None;
                        }
                        Resume::Timer => {}
                    }
                }
            }
        }
        self.shared.active.send_replace(None);
        self.stop_links().await;
        self.shared.set(LinkState::Closed);
    }

    /// Sets the state for a failed attempt and waits for the next one.
    async fn after(
        &mut self,
        failure: Failure,
        since: Instant,
        attempt: &mut u32,
        events: &mut mpsc::UnboundedReceiver<Event>,
    ) -> Resume {
        let kind = failure.kind;
        let reason = failure.reason;
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
        // The state, how long to wait, and whether the login link stays (to ask again through
        // it, without signing in again).
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
            Kind::Transient if given_up => (
                unreachable(Unreachable::Network),
                Some(self.options.retry_every),
                false,
            ),
            Kind::Transient => (LinkState::Unverifiable { reason }, Some(next), false),
        };
        *attempt = attempt.saturating_add(1);
        self.shared.set(state);
        if !keep_login {
            self.stop_links().await;
        }
        self.pause(wait, events, kind == Kind::SignIn).await
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

    /// One attempt: the login link, the endpoint, the node's link, the transport.
    async fn establish(&mut self) -> Result<Arc<Active>, Failure> {
        let host = self.daemon.host().to_owned();
        // The login link, kept if it still runs.
        if !self.login.as_mut().is_some_and(Link::alive) {
            self.stop_links().await;
            let link = Link::start(
                LinkSpec {
                    ssh: &self.ssh,
                    dir: &self.dir.0,
                    name: "login",
                    host: &host,
                    via: None,
                },
                self.options.link_wait,
            )
            .await
            .map_err(|e| Failure::ssh(&e, &format!("connecting to {host}")))?;
            self.login = Some(link);
        }
        let route = self.resolve().await?;
        // A job's node, reached with ssh: a link of its own, through the login link.
        let node_ssh = route
            .node
            .as_ref()
            .filter(|n| n.last_hop == LastHop::Ssh)
            .map(|n| n.name.clone());
        match &node_ssh {
            Some(name) => self.node_link(name).await?,
            None => {
                if let Some((_, link)) = self.node.take() {
                    link.stop().await;
                }
            }
        }
        let preferred = self.shared.transport.lock().ok().and_then(|t| *t);
        let srun = route
            .node
            .as_ref()
            .is_some_and(|n| n.last_hop == LastHop::SrunOverlap);
        if cfg!(unix) && !srun && !self.forbidden && preferred != Some(Transport::Stdio) {
            match self.try_forward(&route).await {
                Ok(active) => return Ok(active),
                Err(Some(failure)) => return Err(failure),
                // Not here: the bridge.
                Err(None) => {}
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
        Ok(active)
    }

    /// Where the daemon is, asked through the login link.
    async fn resolve(&mut self) -> Result<Route, Failure> {
        let target = self.status_target();
        let asked = tokio::time::timeout(STATUS_WAIT, route::resolve(&self.daemon, &target)).await;
        let host = self.daemon.host();
        match asked {
            Err(_) => Err(Failure::new(
                Kind::Transient,
                format!("asking {host} where the helper is: no answer within {STATUS_WAIT:?}"),
            )),
            Ok(Ok(route)) => Ok(route),
            Ok(Err(NoRoute::Waiting(why))) => Err(Failure::new(Kind::Waiting, why)),
            Ok(Err(NoRoute::NotRunning(why))) => Err(Failure::new(Kind::NotRunning, why)),
            Ok(Err(NoRoute::Invalid(why))) => Err(Failure::new(
                Kind::Refused,
                format!("the helper's record on {host} failed a check: {why}"),
            )),
            Ok(Err(NoRoute::Failed(error))) => Err(match &error {
                HelperError::Ssh(e) => {
                    Failure::ssh(e, &format!("asking {host} where the helper is"))
                }
                HelperError::UnsafeDirectory(_) | HelperError::InvalidArgument(_) => Failure::new(
                    Kind::Refused,
                    format!("asking {host} where the helper is: {error}"),
                ),
                _ => Failure::new(
                    Kind::Transient,
                    format!("asking {host} where the helper is: {error}"),
                ),
            }),
        }
    }

    /// The target, reached through the login link's master (Unix), or as given (Windows).
    fn status_target(&self) -> crate::Target {
        let ssh = match self.login.as_ref().and_then(Link::control) {
            Some(control) => self.ssh.through_master(control),
            None => self.ssh.clone(),
        };
        self.daemon.target.with_ssh(ssh)
    }

    /// Makes sure the link to `node` runs, replacing one to another node.
    async fn node_link(&mut self, name: &str) -> Result<(), Failure> {
        if let Some((current, link)) = &mut self.node
            && current == name
            && link.alive()
        {
            return Ok(());
        }
        if let Some((_, link)) = self.node.take() {
            link.stop().await;
        }
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
                dir: &self.dir.0,
                name: "node",
                host: name,
                via: Some(via),
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

    /// Tries the forwarded socket. `Err(None)`: not here, use the bridge.
    #[cfg(unix)]
    async fn try_forward(&mut self, route: &Route) -> Result<Arc<Active>, Option<Failure>> {
        let Some((master, log_from)) = self
            .daemon_link()
            .and_then(|link| Some((link.control()?.to_path_buf(), link.log_len())))
        else {
            return Err(None);
        };
        // A master keeps its forwards: one it already has for this socket is used again.
        let local = match &self.forward {
            Some(f) if f.master == master && f.remote == route.socket => f.local.clone(),
            _ => {
                let local = self.dir.0.join(format!("f{}", self.forwards));
                self.forwards = self.forwards.wrapping_add(1);
                let Some(link) = self.daemon_link() else {
                    return Err(None);
                };
                if forward::add(&self.ssh, &self.dir.0, link, &local, &route.socket)
                    .await
                    .is_err()
                {
                    return Err(None);
                }
                self.forward = Some(Forwarding {
                    local: local.clone(),
                    remote: route.socket.clone(),
                    master,
                });
                local
            }
        };
        let Some(link) = self.daemon_link() else {
            return Err(None);
        };
        match forward::check(&local, link, log_from, self.options.probe_timeout).await {
            Ok(()) => Ok(Arc::new(Active {
                transport: Transport::Forwarded,
                local: Some(local),
                ssh: self.ssh.clone(),
                dir: self.dir.0.clone(),
                host: link.host().to_owned(),
                argv: Vec::new(),
                extra: Vec::new(),
                wait: self.options.bridge_wait,
            })),
            Err(forward::Broken::Forbidden) => {
                // The site's choice: remember it, and use the bridge from now on.
                self.forbidden = true;
                Err(None)
            }
            // Something else (no daemon behind it, say): the bridge says what.
            Err(_) => Err(None),
        }
    }

    #[cfg(not(unix))]
    async fn try_forward(&mut self, _route: &Route) -> Result<Arc<Active>, Option<Failure>> {
        Err(None)
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
        let (host, argv) = match &route.node {
            Some(node) if node.last_hop == LastHop::SrunOverlap => {
                let mut argv: Vec<String> = vec![
                    "exec".to_owned(),
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
            Some(node) => {
                let mut argv = vec!["exec".to_owned()];
                argv.extend(bridge);
                (node.name.clone(), argv)
            }
            None => {
                let mut argv = vec!["exec".to_owned()];
                argv.extend(bridge);
                (login.clone(), argv)
            }
        };
        let (ssh, extra) = match self.daemon_link().and_then(Link::control) {
            Some(control) => (self.ssh.through_master(control), Vec::new()),
            // Windows: each connection logs in; a node through the login node.
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
            #[cfg(unix)]
            local: None,
            ssh,
            dir: self.dir.0.clone(),
            host,
            argv,
            extra,
            wait: self.options.bridge_wait,
        }
    }

    /// Watches the way while connected. `Some(reason)` when it is lost; `None` to close.
    async fn monitor(
        &mut self,
        active: &Arc<Active>,
        events: &mut mpsc::UnboundedReceiver<Event>,
    ) -> Option<String> {
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut last_check = Instant::now();
        let mut last_probe = Instant::now();
        // A probe is a channel of the master (Unix). Without one (Windows) it would be a login,
        // a prompt each time for some: there, the link's keepalives watch alone, and a probe
        // runs only when asked (a wake, a failed connection, a clock jump).
        let periodic = self.daemon_link().and_then(Link::control).is_some();
        loop {
            let mut probe = false;
            let mut check = false;
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
                tokio::select! {
                    reason = login_exit => {
                        return Some(format!("lost the connection to {login_host}: {reason}"));
                    }
                    (name, reason) = node_exit => {
                        return Some(format!("lost the connection to {name}: {reason}"));
                    }
                    event = events.recv() => match event {
                        None | Some(Event::Close) => return None,
                        Some(Event::Wake | Event::Suspect) => probe = true,
                        Some(Event::Retry) => {}
                    },
                    _ = tick.tick() => {
                        check = last_check.elapsed() >= self.options.check_every;
                        probe = periodic && last_probe.elapsed() >= self.options.probe_every;
                    }
                }
            }
            if self.clock.jumped() {
                probe = true;
            }
            if check {
                last_check = Instant::now();
                if let Err(reason) = self.check_masters().await {
                    return Some(reason);
                }
            }
            if probe {
                last_probe = Instant::now();
                if let Err(reason) = self.probe(active).await {
                    return Some(reason);
                }
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
                    let checked =
                        link::control(&self.ssh, &self.dir.0, control, link.host(), "check", &[])
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

    /// One connection through the transport, within the probe's time (a channel of the
    /// master), or the bridge's (a login of its own, whose prompts do not count).
    async fn probe(&self, active: &Arc<Active>) -> Result<(), String> {
        let host = active.host.clone();
        #[cfg(unix)]
        if active.transport == Transport::Forwarded
            && let (Some(local), Some(link)) = (&active.local, self.daemon_link())
        {
            let log_from = link.log_len();
            return forward::check(local, link, log_from, self.options.probe_timeout)
                .await
                .map_err(|why| format!("the helper on {host} stopped answering: {why}"));
        }
        let opened = if self.daemon_link().and_then(Link::control).is_some() {
            match tokio::time::timeout(self.options.probe_timeout, active.open()).await {
                Ok(opened) => opened,
                Err(_) => {
                    return Err(format!(
                        "the helper on {host} gave no answer within {:?}",
                        self.options.probe_timeout
                    ));
                }
            }
        } else {
            active.open().await
        };
        opened
            .map(drop)
            .map_err(|e| format!("the helper on {host} stopped answering: {e}"))
    }

    /// Stops the links (the node's first), and with them every connection.
    async fn stop_links(&mut self) {
        if let Some((_, link)) = self.node.take() {
            link.stop().await;
        }
        if let Some(link) = self.login.take() {
            link.stop().await;
        }
        #[cfg(unix)]
        if let Some(forward) = self.forward.take() {
            let _ = std::fs::remove_file(forward.local);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_with_jitter_and_is_capped() {
        let (min, max) = (Duration::from_secs(1), Duration::from_secs(30));
        assert_eq!(backoff(0, min, max, 0), Duration::from_millis(500));
        assert_eq!(backoff(0, min, max, u32::MAX), Duration::from_secs(1));
        assert_eq!(backoff(3, min, max, u32::MAX), Duration::from_secs(8));
        assert_eq!(backoff(10, min, max, u32::MAX), max);
        assert_eq!(backoff(u32::MAX, min, max, 0), max / 2);
        for attempt in 0..40 {
            let wait = backoff(attempt, min, max, jitter());
            assert!(wait >= min / 2 && wait <= max, "{attempt}: {wait:?}");
        }
    }

    #[test]
    fn clock_jumps_are_seen_either_way() {
        let wall = SystemTime::UNIX_EPOCH + Duration::from_secs(1_790_850_391);
        let mono = Instant::now();
        let later = mono + Duration::from_secs(1);
        // A second on both clocks.
        assert!(!jumped(wall, mono, wall + Duration::from_secs(1), later));
        // An hour asleep: the wall clock moved, the monotonic one did not.
        assert!(jumped(wall, mono, wall + Duration::from_secs(3600), later));
        // The wall clock set back.
        assert!(jumped(wall, mono, wall - Duration::from_secs(60), later));
        // A slow tick moves both.
        assert!(!jumped(
            wall,
            mono,
            wall + Duration::from_secs(9),
            mono + Duration::from_secs(9)
        ));
    }

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
    }

    /// The desktop keeps a connector in shared state and spawns its connections.
    #[test]
    fn the_api_can_be_shared_and_spawned() {
        fn shared<T: Send + Sync + 'static>() {}
        fn spawnable<T: Send>(_: &T) {}
        shared::<Connector>();
        shared::<LinkState>();
        fn stream<T: Send + Unpin + AsyncRead + AsyncWrite + 'static>() {}
        stream::<TunnelStream>();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _entered = rt.enter();
        // Never driven, so nothing runs; and no ssh to run if it were.
        let dir = tempfile::tempdir().unwrap();
        let target = crate::Target::with_layout(
            Ssh::new("/nonexistent/ssh").with_runtime_dir(dir.path().join("rt")),
            "hpc-login",
            crate::Layout::in_home("/home/sam").unwrap(),
            crate::Platform::LinuxX86_64,
        )
        .unwrap();
        let daemon = Daemon::new(target, Arc::new(crate::DirectLauncher::default()));
        if let Ok(connector) = Connector::start(daemon, ConnectorOptions::default()) {
            spawnable(&connector.connect());
            spawnable(&connector.close());
            drop(connector);
        }
    }

    #[test]
    fn states_read_well() {
        assert_eq!(LinkState::Connecting.to_string(), "connecting");
        assert!(
            LinkState::Connected {
                transport: Transport::Stdio
            }
            .is_connected()
        );
        assert_eq!(
            serde_json::to_string(&Transport::Forwarded).unwrap(),
            "\"forwarded\""
        );
    }
}
