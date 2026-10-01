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
//! ControlMaster), or with [`crate::Ssh::with_multiplex`]`(false)`, a heartbeat. On Unix the
//! endpoint check and the stdio bridges read no ssh config of their own (`-F none`): the master
//! has the user's. A connector that crashed leaves its master running; the next one to start
//! stops it (`ssh -O exit`) and removes its directory, once that directory's lock is free.
//!
//! **Where the daemon is** comes from the endpoint record, asked through the link with the
//! launcher's `status` before every (re)connection, and checked: the socket must be where its
//! launcher puts it, the helper's version must be plain (it names the bridge), and for a job it
//! must still be ours and running, its endpoint must name it, and the node squeue names must be
//! the one recorded and have a plain name. A record that fails is never used. A job's node is
//! reached as the site recipe says ([`crate::LastHop`]): with ssh through the login link (Unix:
//! a `ProxyCommand` that is a channel of the login link, so the login node is not logged in to
//! again; Windows: `-J`), refusing a node named like one of the user's own `Host`s; or with
//! `srun --jobid <id> --overlap` on the login node, which also reaches a socket on the node's own
//! disk.
//!
//! **Two transports** ([`Transport`]), chosen per machine:
//! - **Forwarded** (Unix): the link's master listens on a socket in the connector's 0700
//!   directory and forwards each connection to the daemon's socket. Connections share it; a
//!   forwarded channel is not a session, so sshd's `MaxSessions` does not limit them. A site that
//!   forbids it (`AllowStreamLocalForwarding no`: ssh logs "administratively prohibited") is
//!   remembered, and the connector falls back.
//! - **Stdio**: each connection runs `pitcrewd connect` ([`crate::bridge`]) on the machine. Each
//!   is a session of the link (Unix), and sshd allows `MaxSessions` of them at once (10 by
//!   default, 1 or 2 on some sites): one more is refused as that connection's error
//!   ([`crate::SshError::SessionRefused`]), and the link stays. Always used on Windows (OpenSSH
//!   there forwards no unix sockets, neither std nor tokio has them, and a TCP port instead would
//!   be open to every local user), and through `srun`, where each connection is also a job step
//!   (the scheduler's load, and `MaxStepCount`), framed against srun's line buffering.
//!
//! [`Connector::transport`] is the choice worth remembering (forwarded once it worked; stdio
//! once the site refused forwarding); pass it back as [`ConnectorOptions::transport`]. What is
//! in use now is in [`LinkState::Connected`].
//!
//! **Watching.** Connected, the connector watches the link's exit (keepalives), and asks the
//! master whether it lives (`ssh -O check`, Unix) every [`ConnectorOptions::check_every`]. With
//! a forwarded socket it also sends a request through it every
//! [`ConnectorOptions::probe_every`]: a request unanswered makes the state
//! [`LinkState::Unverifiable`] (within ten seconds with the defaults), and one goes every second
//! until one is answered (connected again) or 30 s pass (the way is lost); the link, patient,
//! waits as long before it gives up, so a short outage costs no new login. Nothing
//! watches through a session: it would count against `MaxSessions` (or be an srun step), so a
//! stdio link's own keepalives, which give up after 8 s, are its watch. A failed connection
//! makes the connector check at once (the master, or where the daemon is), at most every few
//! seconds however many fail; so do [`Connector::wake`] and a jump of the wall clock against the
//! monotonic one (the laptop slept; on Windows, where the monotonic clock runs during sleep, the
//! desktop should call `wake()` on resume).
//!
//! **The ladder.** A lost way makes the state `Unverifiable` and starts again: at once if the
//! connection had held for [`ConnectorOptions::give_up_after`], else after a back-off with
//! jitter from [`ConnectorOptions::backoff_min`] to `backoff_max`, the endpoint asked afresh
//! each time (a job that moved is followed). Failing (or connecting and dropping again) for
//! `give_up_after` makes it [`LinkState::Unreachable`]: a network that never answered is still
//! tried every [`ConnectorOptions::retry_every`]; a connection that keeps dropping waits for a
//! wake or [`Connector::retry`], so it does not ask for a one-time code every minute. A helper
//! that is not running (or a job that ended) is unreachable at once and asked about again every
//! `retry_every` through the login link; a failed sign-in or a cancelled prompt (also for a
//! connection, on Windows) waits for `retry()`. Prompts while reconnecting go through the
//! askpass bridge as for any call; nothing is stored. Resuming the API stream (`since=`) is the
//! caller's.
//!
//! **Windows** works, with fewer comforts: no ControlMaster, so each connection logs in, and a
//! password or one-time code is asked each time; keys are the way there. `close()` ends its open
//! connections too. Its tests ran on Linux, without connection reuse; not on Windows itself.
//!
//! **Security**, as for every call: agent and X11 forwarding, local commands and the user's
//! configured forwardings are off; every `-o` is ours; whatever comes from the machine passes the
//! checks above before it reaches an ssh command line; local sockets are in a private 0700
//! directory, removed on close; ssh gets only [`crate::MINIMAL_ENV`] (and what
//! [`crate::Ssh::with_env_passthrough`] adds). Reasons never carry paths or secrets.

#[cfg(unix)]
mod forward;
mod link;
mod route;
mod stdio;
mod supervisor;

pub use link::{KEEPALIVE_COUNT, KEEPALIVE_COUNT_PATIENT, KEEPALIVE_INTERVAL};
pub use route::Daemon;

use crate::{Ssh, SshError};
use route::Route;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::io;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant, SystemTime};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::{mpsc, watch};

/// A clock difference this large between the wall clock and the monotonic one, over one tick,
/// means the laptop slept (or its clock was set): check the link at once.
const JUMP: Duration = Duration::from_secs(5);

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
    /// No way through for [`ConnectorOptions::give_up_after`]. A network that never answered is
    /// still tried every [`ConnectorOptions::retry_every`]; connections that kept dropping wait
    /// for [`Connector::wake`] or [`Connector::retry`].
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
    /// The way does not answer now (lost, or not working yet); the connector is checking or
    /// trying again. Connections may still be tried.
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

/// How a [`Connector`] behaves. The defaults suit a desktop; values below a sane floor (100 ms
/// for `backoff_min`, a second for the other intervals) are raised to it.
#[derive(Clone, Debug)]
pub struct ConnectorOptions {
    /// The transport to use, as remembered from an earlier run ([`Connector::transport`]);
    /// `None` chooses (forwarded where it works, else stdio). A forwarded socket the site
    /// refuses still falls back.
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
    /// How often a request goes through a forwarded socket. Default 4 s.
    pub probe_every: Duration,
    /// How long that request may take before the state is unverifiable. Default 4 s.
    pub probe_timeout: Duration,
    /// The first wait between attempts. Default 1 s.
    pub backoff_min: Duration,
    /// The longest wait between attempts. Default 30 s.
    pub backoff_max: Duration,
    /// How long attempts may fail before the machine is unreachable; also how long a connection
    /// must hold to count as a success. Default 2 minutes.
    pub give_up_after: Duration,
    /// Once unreachable (or the helper not running), how often to try again. Default 1 minute.
    pub retry_every: Duration,
    /// The user's ssh config, whose concrete `Host` names a job's node may not be; `None`:
    /// `~/.ssh/config` (with its `Include`s).
    pub ssh_config: Option<PathBuf>,
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
            probe_every: Duration::from_secs(4),
            probe_timeout: Duration::from_secs(4),
            backoff_min: Duration::from_secs(1),
            backoff_max: Duration::from_secs(30),
            give_up_after: Duration::from_secs(120),
            retry_every: Duration::from_secs(60),
            ssh_config: None,
            wall_clock: WallClock::default(),
        }
    }
}

impl ConnectorOptions {
    /// These options with every interval at least its floor.
    fn normalized(mut self) -> Self {
        let second = Duration::from_secs(1);
        let at_least = |d: &mut Duration, min: Duration| *d = (*d).max(min);
        at_least(&mut self.backoff_min, Duration::from_millis(100));
        let min = self.backoff_min;
        at_least(&mut self.backoff_max, min);
        for d in [
            &mut self.link_wait,
            &mut self.bridge_wait,
            &mut self.check_every,
            &mut self.probe_every,
            &mut self.probe_timeout,
            &mut self.give_up_after,
            &mut self.retry_every,
        ] {
            at_least(d, second);
        }
        self
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
    /// An ssh call failed. [`SshError::SessionRefused`]: the server allows no more sessions on
    /// the connection now (its `MaxSessions`); another try may work once one ends.
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
            Stream::Stdio(s) => Pin::new(s.as_mut()).poll_read(cx, buf),
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
            Stream::Stdio(s) => Pin::new(s.as_mut()).poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match &mut self.get_mut().inner {
            #[cfg(unix)]
            Stream::Socket(s) => Pin::new(s).poll_flush(cx),
            Stream::Stdio(s) => Pin::new(s.as_mut()).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match &mut self.get_mut().inner {
            #[cfg(unix)]
            Stream::Socket(s) => Pin::new(s).poll_shutdown(cx),
            Stream::Stdio(s) => Pin::new(s.as_mut()).poll_shutdown(cx),
        }
    }
}

/// What the supervisor is told.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Event {
    /// Check now: the computer woke, or the network changed. A pending retry happens now.
    Wake,
    /// Start over now from [`LinkState::Unreachable`]; ignored while connected.
    Retry,
    /// A connection failed as if the link were gone (ssh failed, or the local socket): ask the
    /// masters.
    LinkSuspect,
    /// A connection failed as if the endpoint were stale (the bridge found no daemon, or
    /// refused): ask where the daemon is.
    RouteSuspect,
    /// A connection needed signing in (Windows) and it failed or was cancelled.
    SignIn(String),
    /// Stop.
    Close,
}

/// The event a failed connection sends, if any. A refused session is that connection's own
/// failure: the link is fine.
fn event_for(error: &TunnelError) -> Option<Event> {
    match error {
        TunnelError::Ssh(SshError::SessionRefused { .. })
        | TunnelError::NotConnected(_)
        | TunnelError::Unsupported(_) => None,
        TunnelError::Ssh(
            e @ (SshError::AuthFailed { .. }
            | SshError::HostKeyRejected { .. }
            | SshError::HostKeyChanged { .. }
            | SshError::Cancelled
            | SshError::Bridge(_)),
        ) => Some(Event::SignIn(format!("signing in for a connection: {e}"))),
        TunnelError::Ssh(_) | TunnelError::Bridge(_) | TunnelError::Io(_) => {
            Some(Event::LinkSuspect)
        }
        TunnelError::NoDaemon(_) | TunnelError::Refused(_) => Some(Event::RouteSuspect),
    }
}

/// How connections are made while connected.
#[derive(Debug)]
struct Active {
    transport: Transport,
    /// Where it leads, as checked.
    route: Route,
    /// Forwarded: the local socket.
    #[cfg(unix)]
    local: Option<PathBuf>,
    /// Stdio: how to run the bridge.
    ssh: Ssh,
    dir: PathBuf,
    host: String,
    argv: Vec<String>,
    framed: bool,
    extra: Vec<String>,
    wait: Duration,
    closing: watch::Receiver<bool>,
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
                    framed: self.framed,
                    extra: self.extra.clone(),
                    wait: self.wait,
                    closing: self.closing.clone(),
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
    /// The transport worth remembering.
    transport: Mutex<Option<Transport>>,
    /// `true` once the connector closes: open stdio connections end.
    closing: watch::Sender<bool>,
    connect_wait: Duration,
}

impl Shared {
    /// Sets the state, with any path taken out of its reason.
    fn set(&self, state: LinkState) {
        let state = match state {
            LinkState::Unverifiable { reason } => LinkState::Unverifiable {
                reason: scrub(&reason),
            },
            LinkState::Unreachable { why, reason } => LinkState::Unreachable {
                why,
                reason: scrub(&reason),
            },
            other => other,
        };
        self.state.send_if_modified(|current| {
            let changed = *current != state;
            *current = state;
            changed
        });
    }

    /// Records the transport worth remembering ([`Connector::transport`]). Only forwarding,
    /// Unix's, decides one.
    #[cfg(unix)]
    fn remember(&self, transport: Transport) {
        if let Ok(mut remembered) = self.transport.lock() {
            *remembered = Some(transport);
        }
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
        let options = options.normalized();
        let ssh = daemon.target.ssh().minimal_env();
        let dir = supervisor::PrivateDir::new(&ssh)?;
        let shared = Arc::new(Shared {
            host: daemon.host().to_owned(),
            state: watch::Sender::new(LinkState::Connecting),
            active: watch::Sender::new(None),
            transport: Mutex::new(options.transport),
            closing: watch::Sender::new(false),
            connect_wait: options.connect_wait,
        });
        let (events, inbox) = mpsc::unbounded_channel();
        let supervisor = supervisor::Supervisor::new(daemon, options, shared.clone(), ssh, dir);
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
    /// [`ConnectorOptions::connect_wait`] for it. A failure that suggests the way is lost makes
    /// the connector check it (coalesced); a refused session ([`SshError::SessionRefused`]) is
    /// only this connection's.
    ///
    /// # Errors
    /// [`TunnelError::NotConnected`] when unreachable, closed, or still not connected after the
    /// wait; else what opening failed with.
    pub async fn connect(&self) -> Result<TunnelStream, TunnelError> {
        let active = self.wait_active().await?;
        let opened = active.open().await;
        if let Err(error) = &opened
            && let Some(event) = event_for(error)
        {
            let _ = self.inner.events.send(event);
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

    /// The transport worth remembering for this machine: forwarded once it worked, stdio once
    /// the site refused forwarding (or as passed in [`ConnectorOptions::transport`]). Not
    /// stdio used because a forward failed for another reason, or because of `srun`.
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
    /// and a pending retry happens now. On Windows the desktop should call this on resume: the
    /// monotonic clock there runs during sleep, so the connector cannot tell by itself.
    pub fn wake(&self) {
        let _ = self.inner.events.send(Event::Wake);
    }

    /// Starts over now from [`LinkState::Unreachable`] (the person asked to). Ignored while
    /// connected.
    pub fn retry(&self) {
        let _ = self.inner.events.send(Event::Retry);
    }

    /// Stops: the links, and every connection (also each stdio connection's own ssh), end; the
    /// private directory and its sockets are removed. Idempotent.
    pub async fn close(&self) {
        self.inner.shared.closing.send_replace(true);
        let _ = self.inner.events.send(Event::Close);
        let task = self.inner.task.lock().ok().and_then(|mut t| t.take());
        if let Some(task) = task {
            let _ = task.await;
        }
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

/// `text` with every path in it (a `/` starting a word, to the word's end) replaced by `…`:
/// reasons and errors end up on screens and in logs, and paths carry user names.
pub(crate) fn scrub(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut prev: Option<char> = None;
    while let Some(c) = chars.next() {
        let starts_word = prev.is_none_or(|p| {
            p.is_whitespace() || matches!(p, '(' | '"' | '\'' | '=' | ':' | '[' | '<' | ',')
        });
        if c == '/' && starts_word {
            while chars
                .next_if(|&n| {
                    !n.is_whitespace()
                        && !matches!(n, '"' | '\'' | ')' | ',' | ']' | '>')
                        && n != ':'
                })
                .is_some()
            {}
            out.push('…');
            prev = Some('…');
            continue;
        }
        out.push(c);
        prev = Some(c);
    }
    out
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
    fn failed_connections_say_what_to_check() {
        let refused = TunnelError::Ssh(SshError::SessionRefused {
            stderr: String::new(),
        });
        assert_eq!(event_for(&refused), None);
        assert!(matches!(
            event_for(&TunnelError::Ssh(SshError::Cancelled)),
            Some(Event::SignIn(_))
        ));
        assert_eq!(
            event_for(&TunnelError::Ssh(SshError::Unreachable {
                stderr: String::new()
            })),
            Some(Event::LinkSuspect)
        );
        assert_eq!(
            event_for(&TunnelError::NoDaemon("x".into())),
            Some(Event::RouteSuspect)
        );
        assert_eq!(
            event_for(&TunnelError::NotConnected(LinkState::Closed)),
            None
        );
    }

    #[test]
    fn options_have_floors() {
        let options = ConnectorOptions {
            backoff_min: Duration::ZERO,
            backoff_max: Duration::ZERO,
            retry_every: Duration::ZERO,
            probe_every: Duration::from_millis(1),
            ..ConnectorOptions::default()
        }
        .normalized();
        assert_eq!(options.backoff_min, Duration::from_millis(100));
        assert_eq!(options.backoff_max, Duration::from_millis(100));
        assert_eq!(options.retry_every, Duration::from_secs(1));
        assert_eq!(options.probe_every, Duration::from_secs(1));
        // The defaults report a silent forward within ten seconds.
        let defaults = ConnectorOptions::default();
        assert!(
            defaults.probe_every + defaults.probe_timeout + Duration::from_secs(1)
                < Duration::from_secs(10)
        );
    }

    #[test]
    fn paths_are_scrubbed() {
        assert_eq!(
            scrub("Control socket connect(/run/user/1000/pitcrew-ssh/t1/login): No such file"),
            "Control socket connect(…): No such file"
        );
        assert_eq!(
            scrub("sh: 1: /home/sam/.pitcrew/bin/1.0/pitcrewd: not found"),
            "sh: 1: …: not found"
        );
        assert_eq!(
            scrub("Timeout, server hpc-login not responding."),
            "Timeout, server hpc-login not responding."
        );
        assert_eq!(scrub("N/A and a/b"), "N/A and a/b");
    }

    /// The desktop keeps a connector in shared state and spawns its connections.
    #[test]
    fn the_api_can_be_shared_and_spawned() {
        fn shared<T: Send + Sync + 'static>() {}
        fn spawnable<T: Send>(_: &T) {}
        fn stream<T: Send + Unpin + AsyncRead + AsyncWrite + 'static>() {}
        shared::<Connector>();
        shared::<LinkState>();
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
