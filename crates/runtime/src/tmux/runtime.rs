//! [`TmuxRuntime`]: the `Runtime` trait over one private tmux server.

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use pitcrew_interfaces::runtime::{
    OutputChunk, Runtime, RuntimeError, RuntimeKind, Screen, StartSpec, TerminalInfo,
};
use pitcrew_protocol::ids::TerminalId;
use pitcrew_protocol::runner::Key;

use super::conn::{
    CallError, ConnectError, Connection, Outbox, Pending, Sink, Waiter, lock, reply_text,
};
use super::state::{self, Claim, LIST_FORMAT, Listed, MAX_SIZE, RESERVE, Term, Terminals};
use super::{OFFSET_OPTION, SESSION, TERMINAL_OPTION, TmuxOptions};
use crate::command::{Argument, Command, FormatError};
use crate::control::{CommandReply, Notification, PaneId, WindowId};
use crate::detect::TmuxVersion;

/// The shell script every terminal runs, with the program (an absolute path, so never a shell
/// builtin) as `$0` and its arguments as `$@`: they are positional parameters, never parsed as
/// shell text. (tmux would run a lone argument through the shell, so it is never passed alone.)
///
/// - It unsets `TMUX` and `TMUX_PANE`, so the program does not reach PitCrew's server by
///   accident (people attach from their own terminals, which this does not affect).
/// - It does not `exec` the program: when a pane's own process ends, tmux 3.2 destroys the pane
///   at once and control clients can lose its last output. The shell outlives the program by a
///   moment, so that output is delivered first.
/// - It traps `INT` and `QUIT`, so Ctrl-C reaches only the program (which gets the default
///   handlers back), as it would without the shell.
const WRAPPER: &str = r#"unset TMUX TMUX_PANE; trap : INT QUIT; "$0" "$@"; s=$?; sleep 0.2 2>/dev/null || sleep 1; exit "$s""#;

/// The window that holds a new session until its first terminal exists. It ends by itself if
/// PitCrew stops before removing it.
const HOLDER_NAME: &str = "pitcrew-start";
const HOLDER: [&str; 3] = ["/bin/sh", "-c", "sleep 60"];

/// Bytes per `send-keys -H` command.
const INPUT_CHUNK: usize = 1024;

/// How long attaching is not retried after it failed, unless a terminal is started.
const RETRY_AFTER: Duration = Duration::from_millis(500);

/// How long `kill` waits after `SIGTERM` before `SIGKILL`.
const KILL_GRACE: Duration = Duration::from_millis(500);

/// Windows waiting to be killed on the next connection, at most.
const PENDING_KILLS: usize = 64;

/// How long dropping the runtime waits for its keeper thread.
const DROP_WAIT: Duration = Duration::from_secs(2);

/// PitCrew's terminals as windows of a private tmux server (see the [module docs](super)).
///
/// - Every call is bounded by [`TmuxOptions::call_timeout`] (`start` by
///   [`TmuxOptions::start_timeout`]), and answers `Unavailable` past it. Input or a resize that
///   timed out may still reach the terminal: the command was sent.
/// - A control client that dies is replaced in the background; terminals keep their output
///   offsets, and skip one at the gap, so a reader at the old end reads `truncated` (output
///   printed while no client was attached is not in the stream).
/// - Dropping the runtime detaches without stopping any terminal, and stores each terminal's
///   exact end offset in tmux: a new runtime on the same socket finds the terminals with
///   [`Runtime::list`] and numbers their output on from there. After a crash it resumes at most
///   1 MiB further on, so a reader's old offset is never reused (it reads `truncated`).
/// - A program that has ended stays readable (`alive: false`) until 16 more have ended.
/// - `TerminalInfo::pid` is not the program's own process but its parent: the `sh` that runs
///   it (see [`WRAPPER`]), which leads the process group the program and its children are in.
/// - `kill` sends that process group `SIGTERM`, then `SIGKILL` after half a second, then closes
///   the window. A process that left the group (`setsid`, a daemonizing program) survives.
/// - Programs are found as files on the spec's `PATH` (else this process's), absolute entries
///   only; a name that is not a file there is refused.
pub struct TmuxRuntime {
    inner: Arc<Inner>,
    keeper: Option<(JoinHandle<()>, Mutex<Receiver<()>>)>,
}

impl std::fmt::Debug for TmuxRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TmuxRuntime")
            .field("socket", &self.inner.options.socket)
            .finish_non_exhaustive()
    }
}

struct Inner {
    /// With `tmux` resolved to an absolute path when it could be.
    options: TmuxOptions,
    me: Weak<Inner>,
    terminals: Mutex<Terminals>,
    /// Signalled when output arrives, a terminal ends or the connection drops.
    changed: Condvar,
    /// Callers waiting on `changed`: output is not signalled when there are none.
    waiting: AtomicUsize,
    link: Mutex<Option<Arc<Link>>>,
    connecting: Gate,
    starting: Gate,
    input: Gate,
    generation: AtomicU64,
    /// When attaching last failed, and why.
    failed: Mutex<Option<(Instant, Down)>>,
    shutdown: AtomicBool,
    keeper: Mutex<Option<Sender<()>>>,
    /// The server last attached to.
    server: Mutex<Option<Server>>,
    /// Windows of abandoned starts, killed again on the next connection to the same server.
    pending_kills: Mutex<Vec<(u32, WindowId)>>,
}

/// A tmux server instance: its pid, and the id of PitCrew's session in it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Server {
    pid: u32,
    session: String,
}

/// The current control client.
struct Link {
    conn: Connection,
    generation: u64,
    /// The window that created the session, removed once a terminal exists.
    holder: Mutex<Option<WindowId>>,
    /// The server's pid, from reconcile.
    server: AtomicU64,
}

/// Why there is no connection.
#[derive(Debug, Clone)]
enum Down {
    /// No server, or no PitCrew session: there are no live terminals.
    NoServer,
    Unavailable(String),
}

impl Down {
    fn error(self) -> RuntimeError {
        match self {
            Self::NoServer => {
                RuntimeError::Unavailable("PitCrew's tmux server is not running".into())
            }
            Self::Unavailable(why) => RuntimeError::Unavailable(why),
        }
    }
}

impl TmuxRuntime {
    /// A runtime for the server on `options.socket`. Checks (creating it if needed) the
    /// socket's private directory, finds tmux once, and starts attaching in the background; it
    /// does not start a server, which the first [`Runtime::start`] does.
    ///
    /// # Errors
    ///
    /// `Unavailable` if the socket's directory is unsafe or cannot be created.
    pub fn new(mut options: TmuxOptions) -> Result<Self, RuntimeError> {
        super::socket::ensure_private(&options.socket).map_err(RuntimeError::Unavailable)?;
        if let Some(tmux) = options.resolved_tmux() {
            options.tmux = tmux;
        }
        let (wake, woken) = mpsc::channel();
        let inner = Arc::new_cyclic(|me| Inner {
            terminals: Mutex::new(Terminals::new(options.history)),
            options,
            me: me.clone(),
            changed: Condvar::new(),
            waiting: AtomicUsize::new(0),
            link: Mutex::new(None),
            connecting: Gate::default(),
            starting: Gate::default(),
            input: Gate::default(),
            generation: AtomicU64::new(0),
            failed: Mutex::new(None),
            shutdown: AtomicBool::new(false),
            keeper: Mutex::new(Some(wake.clone())),
            server: Mutex::new(None),
            pending_kills: Mutex::new(Vec::new()),
        });
        let weak = Arc::downgrade(&inner);
        let (stopped, keeper_done) = mpsc::sync_channel(1);
        let keeper = std::thread::Builder::new()
            .name("pitcrew-tmux-keeper".into())
            .spawn(move || {
                keep(&weak, &woken);
                let _ = stopped.send(());
            })
            .map_err(RuntimeError::Io)?;
        // Attach now, so output is recorded from the start rather than from the first call.
        let _ = wake.send(());
        Ok(Self {
            inner,
            keeper: Some((keeper, Mutex::new(keeper_done))),
        })
    }

    /// The private server's socket: `tmux -S <socket> attach -t pitcrew` shows the terminals.
    pub fn socket(&self) -> &Path {
        &self.inner.options.socket
    }

    /// The control client's process id, while one is attached.
    pub fn control_pid(&self) -> Option<u32> {
        self.inner.current().map(|link| link.conn.pid())
    }

    /// Waits until a terminal's output goes past `offset` or its program ends, for at most
    /// `timeout`, and returns where its output ends now.
    ///
    /// # Errors
    ///
    /// `NotFound` for a terminal the runtime does not have.
    pub fn wait_for_output(
        &self,
        id: TerminalId,
        offset: u64,
        timeout: Duration,
    ) -> Result<u64, RuntimeError> {
        let deadline = Instant::now() + timeout;
        let inner = &self.inner;
        inner.adopt_if_unknown(id, deadline);
        inner.waiting.fetch_add(1, Ordering::AcqRel);
        let waited = (|| {
            let mut terminals = lock(&inner.terminals);
            loop {
                let term = terminals.get(id).ok_or(RuntimeError::NotFound(id))?;
                let end = term.end();
                if end > offset || !term.alive {
                    return Ok(end);
                }
                let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                    return Ok(end);
                };
                terminals = inner
                    .changed
                    .wait_timeout(terminals, left)
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .0;
            }
        })();
        inner.waiting.fetch_sub(1, Ordering::AcqRel);
        waited
    }
}

impl Drop for TmuxRuntime {
    /// Bounded: a few seconds at most, however tmux behaves.
    fn drop(&mut self) {
        let inner = &self.inner;
        inner.shutdown.store(true, Ordering::Release);
        lock(&inner.keeper).take();
        if let Some((keeper, done)) = self.keeper.take() {
            // The keeper may be in the middle of attaching; it is left to finish on its own
            // rather than waited for past DROP_WAIT.
            if lock(&done).recv_timeout(DROP_WAIT).is_ok() {
                let _ = keeper.join();
            }
        }
        let link = lock(&inner.link).take();
        // Stop recording first, so the offsets stored below are the last a reader saw.
        let ends = {
            let mut terminals = lock(&inner.terminals);
            terminals.closing = true;
            terminals.ends()
        };
        if let Some(link) = link {
            if link.conn.is_open() && !ends.is_empty() {
                let commands: Vec<Command> = ends
                    .iter()
                    .filter_map(|&(pane, end)| offset_command(pane, end).ok())
                    .collect();
                let deadline =
                    Instant::now() + inner.options.call_timeout.min(Duration::from_secs(2));
                if let Err(e) = link.conn.call(&commands, deadline) {
                    tracing::warn!(?e, "could not store the terminals' offsets in tmux");
                }
            }
            link.conn.close(Duration::from_secs(1));
        }
        inner.changed.notify_all();
    }
}

impl Inner {
    fn current(&self) -> Option<Arc<Link>> {
        lock(&self.link)
            .as_ref()
            .filter(|link| link.conn.is_open())
            .cloned()
    }

    /// The control client, attaching (or, with `create`, starting the server) if there is none.
    fn connection(&self, deadline: Instant, create: bool) -> Result<Arc<Link>, Down> {
        if let Some(link) = self.current() {
            return Ok(link);
        }
        let _pass = self
            .connecting
            .enter(deadline)
            .ok_or_else(|| Down::Unavailable("tmux is busy connecting".into()))?;
        if let Some(link) = self.current() {
            return Ok(link);
        }
        if self.shutdown.load(Ordering::Acquire) {
            return Err(Down::Unavailable(
                "the tmux runtime is shutting down".into(),
            ));
        }
        if !create
            && let Some((at, why)) = lock(&self.failed).clone()
            && at.elapsed() < RETRY_AFTER
        {
            return Err(why);
        }
        match self.open(deadline, create) {
            Ok(link) => {
                lock(&self.failed).take();
                Ok(link)
            }
            Err(why) => {
                *lock(&self.failed) = Some((Instant::now(), why.clone()));
                Err(why)
            }
        }
    }

    fn open(&self, deadline: Instant, create: bool) -> Result<Arc<Link>, Down> {
        lock(&self.terminals).detach();
        let exact = format!("={SESSION}");
        let mut attempt = self.connect(&["attach-session", "-t", exact.as_str()], deadline);
        if matches!(attempt, Err(ConnectError::NoSession))
            && let Some(server) = lock(&self.server).clone()
        {
            // Someone may have renamed the session: try it by id. Reconcile checks that the
            // server is still the one it was.
            attempt = self.connect(&["attach-session", "-t", server.session.as_str()], deadline);
        }
        let (conn, generation, holder) = match attempt {
            Ok((conn, generation, _)) => (conn, generation, None),
            Err(ConnectError::NoSession) if create => {
                let mut args = vec!["new-session", "-s", SESSION, "-n", HOLDER_NAME];
                args.extend(["-P", "-F", "#{window_id}", "--"]);
                args.extend(HOLDER);
                match self.connect(&args, deadline) {
                    Ok((conn, generation, reply)) => {
                        let holder = reply.lines.first().and_then(|line| WindowId::parse(line));
                        (conn, generation, holder)
                    }
                    // Someone else made it in the meantime.
                    Err(ConnectError::Duplicate) => {
                        match self.connect(&["attach-session", "-t", exact.as_str()], deadline) {
                            Ok((conn, generation, _)) => (conn, generation, None),
                            Err(e) => return Err(self.connect_failed(e)),
                        }
                    }
                    Err(e) => return Err(self.connect_failed(e)),
                }
            }
            Err(e) => return Err(self.connect_failed(e)),
        };
        let link = Arc::new(Link {
            conn,
            generation,
            holder: Mutex::new(holder),
            server: AtomicU64::new(0),
        });
        self.reconcile(&link, deadline)?;
        if self.shutdown.load(Ordering::Acquire) {
            link.conn.kill();
            return Err(Down::Unavailable(
                "the tmux runtime is shutting down".into(),
            ));
        }
        self.replay_kills(&link, deadline);
        *lock(&self.link) = Some(Arc::clone(&link));
        tracing::debug!(pid = link.conn.pid(), "attached to PitCrew's tmux server");
        Ok(link)
    }

    fn connect(
        &self,
        args: &[&str],
        deadline: Instant,
    ) -> Result<(Connection, u64, CommandReply), ConnectError> {
        let generation = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
        let feed = Arc::new(Feed {
            inner: self.me.clone(),
            generation,
        });
        let (conn, reply) = Connection::open(&self.options, args, feed, deadline)?;
        Ok((conn, generation, reply))
    }

    fn connect_failed(&self, error: ConnectError) -> Down {
        match error {
            ConnectError::NoSession => {
                // No server: whatever ran in it has ended.
                lock(&self.terminals).all_died();
                self.changed.notify_all();
                Down::NoServer
            }
            ConnectError::Duplicate => Down::Unavailable("tmux session already exists".into()),
            ConnectError::TimedOut => Down::Unavailable(format!(
                "tmux did not answer on {}",
                self.options.socket.display()
            )),
            ConnectError::Unsafe(why) => Down::Unavailable(why),
            ConnectError::Failed(why) => Down::Unavailable(format!("tmux: {why}")),
        }
    }

    /// Checks the server and brings the terminals up to date with its panes.
    fn reconcile(&self, link: &Link, deadline: Instant) -> Result<(), Down> {
        let commands = [
            Command::new("display-message")
                .and_then(|c| c.arg(Argument::Flag("-p")))
                .and_then(|c| c.arg(Argument::Format("#{version} #{pid} #{session_id}"))),
            // New windows take the size `start` sets as default-size, not a client's.
            Command::new("set-option")
                .and_then(|c| c.arg(Argument::Flag("-g")))
                .and_then(|c| c.arg(Argument::Text("window-size")))
                .and_then(|c| c.arg(Argument::Text("manual"))),
            Command::new("list-panes")
                .and_then(|c| c.arg(Argument::Flag("-s")))
                .and_then(|c| c.arg(Argument::Flag("-F")))
                .and_then(|c| c.arg(Argument::Format(LIST_FORMAT))),
        ]
        .into_iter()
        .collect::<Result<Vec<_>, FormatError>>()
        .map_err(|e| Down::Unavailable(e.to_string()))?;
        let replies = link
            .conn
            .call(&commands, deadline)
            .map_err(|e| Down::Unavailable(call_error(e)))?;
        let about = replies[0]
            .lines
            .first()
            .map(|v| String::from_utf8_lossy(v).into_owned())
            .unwrap_or_default();
        let server = match server_facts(&about) {
            Ok(server) => server,
            Err(why) => {
                link.conn.kill();
                return Err(Down::Unavailable(why));
            }
        };
        for reply in &replies[1..] {
            if reply.failed {
                return Err(Down::Unavailable(format!("tmux: {}", reply_text(reply))));
            }
        }
        {
            let mut known = lock(&self.server);
            if known.as_ref().is_some_and(|k| k.pid != server.pid) {
                // Another server: whatever ran in the one before has ended with it.
                lock(&self.terminals).all_died();
                lock(&self.pending_kills).clear();
            }
            link.server.store(u64::from(server.pid), Ordering::Release);
            *known = Some(server);
        }
        let listed: Vec<Listed> = replies[2]
            .lines
            .iter()
            .filter_map(|line| state::parse_listed(line))
            .collect();
        let adopted = lock(&self.terminals).reconcile(&listed);
        self.changed.notify_all();
        if adopted.is_empty() {
            return Ok(());
        }
        // Store a resume offset ahead of the output before anyone reads it.
        let commands: Vec<Command> = adopted
            .iter()
            .filter_map(|&(_, pane, offset)| offset_command(pane, offset).ok())
            .collect();
        if let Ok(replies) = link.conn.call(&commands, deadline) {
            let mut terminals = lock(&self.terminals);
            for (&(id, _, offset), reply) in adopted.iter().zip(&replies) {
                if !reply.failed {
                    terminals.reserved(id, offset);
                }
            }
        }
        Ok(())
    }

    /// Kills, on a fresh connection, the windows of starts that were given up on this server.
    fn replay_kills(&self, link: &Link, deadline: Instant) {
        let server = link.server.load(Ordering::Acquire);
        let kills: Vec<Command> = std::mem::take(&mut *lock(&self.pending_kills))
            .into_iter()
            .filter(|&(pid, _)| u64::from(pid) == server)
            .filter_map(|(_, window)| kill_window(window).ok())
            .collect();
        if !kills.is_empty() {
            // A failure means that window is gone already.
            let _ = link.conn.call(&kills, deadline);
        }
    }

    /// A window nobody will use (its start was given up): kill it now, and again on the next
    /// connection in case this one does not get there.
    fn discard_window(&self, outbox: &Outbox, server: u64, window: WindowId) {
        if let Ok(kill) = kill_window(window) {
            outbox.send_and_forget(&kill);
        }
        let Ok(server) = u32::try_from(server) else {
            return;
        };
        let mut pending = lock(&self.pending_kills);
        if pending.len() >= PENDING_KILLS {
            pending.remove(0);
        }
        pending.push((server, window));
    }

    /// A terminal not known yet may be one a restarted runtime has not listed: attach first.
    fn adopt_if_unknown(&self, id: TerminalId, deadline: Instant) {
        if lock(&self.terminals).get(id).is_none() {
            if let Some(link) = self.current() {
                let _ = self.reconcile(&link, deadline);
            } else {
                let _ = self.connection(deadline, false);
            }
        }
    }

    fn with<T>(
        &self,
        id: TerminalId,
        deadline: Instant,
        f: impl FnOnce(&Term) -> T,
    ) -> Result<T, RuntimeError> {
        self.adopt_if_unknown(id, deadline);
        lock(&self.terminals)
            .get(id)
            .map(f)
            .ok_or(RuntimeError::NotFound(id))
    }

    /// A live terminal's window and pane, with the connection to reach them.
    fn live(
        &self,
        id: TerminalId,
        deadline: Instant,
    ) -> Result<(Arc<Link>, WindowId, PaneId), RuntimeError> {
        let (window, pane, alive) = self.with(id, deadline, |t| (t.window, t.pane, t.alive))?;
        if !alive {
            return Err(exited(id));
        }
        let link = self
            .connection(deadline, false)
            .map_err(|down| match down {
                // No server: the terminal has ended with it.
                Down::NoServer => exited(id),
                down => down.error(),
            })?;
        let alive = lock(&self.terminals).get(id).is_some_and(|t| t.alive);
        if !alive {
            return Err(exited(id));
        }
        Ok((link, window, pane))
    }

    fn call(
        &self,
        link: &Link,
        commands: &[Command],
        deadline: Instant,
    ) -> Result<Vec<CommandReply>, RuntimeError> {
        link.conn
            .call(commands, deadline)
            .map_err(|e| RuntimeError::Unavailable(call_error(e)))
    }

    /// Leaves copy mode (and other modes that accept `cancel`) before input, so the input
    /// reaches the program instead of the mode's key bindings. Callers hold the input gate.
    fn leave_mode(&self, link: &Link, pane: PaneId, deadline: Instant) -> Result<(), RuntimeError> {
        for _ in 0..3 {
            let query = Command::pane_in_mode(pane).map_err(invalid)?;
            let reply = self.call(link, &[query], deadline)?;
            let reply = &reply[0];
            if reply.failed {
                return Err(failed(reply));
            }
            if reply.lines.first().is_some_and(|l| l.as_slice() == b"0") {
                return Ok(());
            }
            let cancel = Command::cancel_copy_mode(pane).map_err(invalid)?;
            let reply = self.call(link, &[cancel], deadline)?;
            if reply[0].failed {
                return Err(failed(&reply[0]));
            }
        }
        Err(RuntimeError::Unavailable(
            "the terminal is in a tmux mode that does not take input".into(),
        ))
    }

    /// Sends input commands for a live terminal, after leaving any mode.
    fn input(
        &self,
        id: TerminalId,
        commands: impl FnOnce(PaneId) -> Result<Vec<Command>, FormatError>,
    ) -> Result<(), RuntimeError> {
        let deadline = Instant::now() + self.options.call_timeout;
        let (link, _, pane) = self.live(id, deadline)?;
        let commands = commands(pane).map_err(invalid)?;
        if commands.is_empty() {
            return Ok(());
        }
        let _pass = self
            .input
            .enter(deadline)
            .ok_or_else(|| RuntimeError::Unavailable("terminal input is busy".into()))?;
        self.leave_mode(&link, pane, deadline)?;
        for reply in self.call(&link, &commands, deadline)? {
            if reply.failed {
                return Err(failed(&reply));
            }
        }
        Ok(())
    }

    fn notified(&self, notification: Notification, outbox: &Outbox) {
        match notification {
            Notification::Output { pane, data }
            | Notification::ExtendedOutput { pane, data, .. } => {
                lock(&self.terminals).output(pane, &data, |pane, offset| {
                    offset_command(pane, offset).is_ok_and(|c| outbox.send_and_forget(&c))
                });
                if self.waiting.load(Ordering::Acquire) > 0 {
                    self.changed.notify_all();
                }
            }
            Notification::WindowClose { window } | Notification::UnlinkedWindowClose { window } => {
                lock(&self.terminals).window_closed(window);
                self.changed.notify_all();
            }
            Notification::LayoutChange { layout, .. } => {
                // Recorded only: the screen model applies it when it is next read.
                let mut terminals = lock(&self.terminals);
                for (pane, cols, rows) in state::layout_panes(&layout) {
                    terminals.pane_size(PaneId(pane), cols, rows);
                }
            }
            _ => {}
        }
    }

    fn lost(&self, generation: u64, why: &str) {
        let mut link = lock(&self.link);
        if link.as_ref().is_some_and(|l| l.generation == generation) {
            *link = None;
            drop(link);
            if !self.shutdown.load(Ordering::Acquire) {
                tracing::warn!(%why, "the tmux control client ended; reattaching");
            }
            if let Some(keeper) = lock(&self.keeper).as_ref() {
                let _ = keeper.send(());
            }
        }
        self.changed.notify_all();
    }
}

/// `#{version} #{pid} #{session_id}`: the server's facts, if its version is supported.
fn server_facts(about: &str) -> Result<Server, String> {
    let mut fields = about.split(' ');
    let version = fields
        .next()
        .and_then(|v| format!("tmux {v}").parse::<TmuxVersion>().ok());
    match version {
        Some(v) if v.is_supported() => {}
        other => {
            let shown = other.map_or_else(|| "an unknown version".to_owned(), |v| v.to_string());
            return Err(format!(
                "the running tmux server is {shown}; PitCrew needs 3.2 or newer"
            ));
        }
    }
    let pid = fields.next().and_then(|p| p.parse().ok());
    let session = fields.next().filter(|s| {
        s.strip_prefix('$')
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
    });
    match (pid, session) {
        (Some(pid), Some(session)) => Ok(Server {
            pid,
            session: session.to_owned(),
        }),
        _ => Err(format!("tmux did not say which server it is: {about:?}")),
    }
}

/// The keeper thread: (re)attaches in the background while terminals are alive (and once at
/// the start, to find terminals from before a restart), so their output keeps flowing when
/// nobody calls the runtime.
fn keep(inner: &Weak<Inner>, woken: &Receiver<()>) {
    let mut first = true;
    while woken.recv().is_ok() {
        let mut delay = Duration::from_millis(50);
        loop {
            let Some(strong) = inner.upgrade() else {
                return;
            };
            if strong.shutdown.load(Ordering::Acquire)
                || strong.current().is_some()
                || (!first && !lock(&strong.terminals).any_alive())
            {
                break;
            }
            let deadline = Instant::now() + strong.options.call_timeout;
            match strong.connection(deadline, false) {
                Ok(_) | Err(Down::NoServer) => break,
                Err(Down::Unavailable(why)) => {
                    tracing::debug!(%why, "attaching to tmux failed; retrying");
                }
            }
            drop(strong);
            if let Err(RecvTimeoutError::Disconnected) = woken.recv_timeout(delay) {
                return;
            }
            delay = (delay * 2).min(Duration::from_secs(2));
        }
        first = false;
    }
}

/// The reader thread's view of the runtime.
struct Feed {
    inner: Weak<Inner>,
    generation: u64,
}

impl Sink for Feed {
    fn notify(&self, notification: Notification, outbox: &Outbox) {
        if let Some(inner) = self.inner.upgrade() {
            inner.notified(notification, outbox);
        }
    }

    fn closed(&self, why: &str) {
        if let Some(inner) = self.inner.upgrade() {
            inner.lost(self.generation, why);
        }
    }
}

impl Runtime for TmuxRuntime {
    fn kind(&self) -> RuntimeKind {
        RuntimeKind::Tmux
    }

    fn start(&self, spec: &StartSpec) -> Result<TerminalInfo, RuntimeError> {
        let inner = &self.inner;
        let spawn = |reason: String| RuntimeError::Spawn {
            program: spec.program.clone(),
            reason,
        };
        let (cols, rows) = check_size(spec.cols, spec.rows).map_err(spawn)?;
        let name = state::window_name(&spec.name);
        let new_window = new_window(spec, &name, &inner.options).map_err(spawn)?;
        let deadline = Instant::now() + inner.options.start_timeout;
        let _pass = inner
            .starting
            .enter(deadline)
            .ok_or_else(|| RuntimeError::Unavailable("tmux is busy starting a terminal".into()))?;
        let link = inner.connection(deadline, true).map_err(Down::error)?;
        let server = link.server.load(Ordering::Acquire);
        let default_size = Command::new("set-option")
            .and_then(|c| c.arg(Argument::Text("default-size")))
            .and_then(|c| c.arg(Argument::Text(&format!("{cols}x{rows}"))))
            .map_err(invalid)?;
        let holder = lock(&link.holder).take();
        let weak = inner.me.clone();
        // If this call gives up before tmux answers, the window it gets is removed then.
        let created = Pending::new(move |reply, outbox| {
            if let (Some(inner), Some((window, _, _))) = (weak.upgrade(), parse_ids(reply))
                && !reply.failed
            {
                inner.discard_window(outbox, server, window);
            }
        });
        let sent = link.conn.outbox().send_with(
            &[default_size, new_window],
            vec![Waiter::Discard, Waiter::Pending(Arc::clone(&created))],
        );
        let created = sent
            .and_then(|()| created.wait(deadline))
            .map_err(|e| RuntimeError::Unavailable(call_error(e)))
            .and_then(|reply| match parse_ids(&reply) {
                Some(ids) if !reply.failed => Ok(ids),
                _ => Err(spawn(format!("tmux: {}", reply_text(&reply)))),
            });
        let (window, pane, pid) = match created {
            Ok(ids) => ids,
            Err(e) => {
                // Do not leave a session behind for a terminal that never started.
                if let Some(holder) = holder
                    && let Ok(kill) = kill_window(holder)
                {
                    link.conn.outbox().send_and_forget(&kill);
                }
                return Err(e);
            }
        };
        let id = TerminalId::new();
        lock(&inner.terminals).claim(Claim {
            id,
            name,
            window,
            pane,
            pid,
            cols,
            rows,
            offset: 0,
            alive: true,
        });
        let mut commands = vec![
            set_pane_option(pane, TERMINAL_OPTION, &id.to_string()).map_err(invalid)?,
            offset_command(pane, RESERVE).map_err(invalid)?,
        ];
        if let Some(holder) = holder {
            commands.push(kill_window(holder).map_err(invalid)?);
        }
        match inner.call(&link, &commands, deadline) {
            Ok(replies) if !replies[0].failed => {
                if !replies[1].failed {
                    lock(&inner.terminals).reserved(id, RESERVE);
                }
            }
            // The window is gone already: the program ended at once. Its output is readable.
            Ok(_) => lock(&inner.terminals).died(id),
            Err(e) => {
                // It may not be tagged, so a restarted runtime would never find it: remove it.
                inner.discard_window(link.conn.outbox(), server, window);
                lock(&inner.terminals).died(id);
                return Err(e);
            }
        }
        inner.changed.notify_all();
        inner.with(id, deadline, Term::info)
    }

    fn write(&self, id: TerminalId, bytes: &[u8]) -> Result<(), RuntimeError> {
        self.inner.input(id, |pane| {
            bytes
                .chunks(INPUT_CHUNK)
                .filter_map(|chunk| Command::send_bytes(pane, chunk).transpose())
                .collect()
        })
    }

    fn send_keys(&self, id: TerminalId, keys: &[Key]) -> Result<(), RuntimeError> {
        self.inner.input(id, |pane| {
            Ok(Command::send_keys(pane, keys)?.into_iter().collect())
        })
    }

    fn resize(&self, id: TerminalId, cols: u16, rows: u16) -> Result<(), RuntimeError> {
        let (cols, rows) = check_size(cols, rows).map_err(|why| {
            RuntimeError::Io(std::io::Error::new(std::io::ErrorKind::InvalidInput, why))
        })?;
        let deadline = Instant::now() + self.inner.options.call_timeout;
        let (link, window, _) = self.inner.live(id, deadline)?;
        let command = Command::new("resize-window")
            .and_then(|c| c.arg(Argument::Flag("-t")))
            .and_then(|c| c.arg(Argument::Window(window)))
            .and_then(|c| c.arg(Argument::Flag("-x")))
            .and_then(|c| c.arg(Argument::Number(cols.into())))
            .and_then(|c| c.arg(Argument::Flag("-y")))
            .and_then(|c| c.arg(Argument::Number(rows.into())))
            .map_err(invalid)?;
        let reply = self.inner.call(&link, &[command], deadline)?;
        if reply[0].failed {
            return Err(failed(&reply[0]));
        }
        lock(&self.inner.terminals).set_size(id, cols, rows);
        Ok(())
    }

    fn screen(&self, id: TerminalId) -> Result<Screen, RuntimeError> {
        let deadline = Instant::now() + self.inner.options.call_timeout;
        self.inner.adopt_if_unknown(id, deadline);
        state::screen(&self.inner.terminals, id).ok_or(RuntimeError::NotFound(id))
    }

    fn read_output(
        &self,
        id: TerminalId,
        from: u64,
        max: usize,
    ) -> Result<OutputChunk, RuntimeError> {
        let deadline = Instant::now() + self.inner.options.call_timeout;
        self.inner.with(id, deadline, |t| t.read(from, max))
    }

    fn info(&self, id: TerminalId) -> Result<TerminalInfo, RuntimeError> {
        // For a live terminal, attach if needed so `alive` is current (a missing server means it
        // has ended). An ended one stays ended: no need to reach tmux.
        let deadline = Instant::now() + self.inner.options.call_timeout;
        let ended = lock(&self.inner.terminals)
            .get(id)
            .is_some_and(|t| !t.alive);
        if !ended {
            let _ = self.inner.connection(deadline, false);
        }
        self.inner.with(id, deadline, Term::info)
    }

    fn list(&self) -> Result<Vec<TerminalInfo>, RuntimeError> {
        let inner = &self.inner;
        let deadline = Instant::now() + inner.options.call_timeout;
        match inner.connection(deadline, false) {
            Ok(link) => inner.reconcile(&link, deadline).map_err(Down::error)?,
            Err(Down::NoServer) => {}
            Err(down) => return Err(down.error()),
        }
        let mut all: Vec<TerminalInfo> = lock(&inner.terminals).all().map(Term::info).collect();
        all.sort_by_key(|t| t.id);
        Ok(all)
    }

    fn kill(&self, id: TerminalId) -> Result<(), RuntimeError> {
        let inner = &self.inner;
        let deadline = Instant::now() + inner.options.call_timeout;
        let (link, window, pane) = match inner.live(id, deadline) {
            Ok(found) => found,
            // Already ended: nothing to kill.
            Err(RuntimeError::Io(e)) if e.kind() == std::io::ErrorKind::BrokenPipe => {
                return Ok(());
            }
            Err(e) => return Err(e),
        };
        // The pane's process (the wrapper shell) leads the program's process group: stop the
        // group first, since closing the window only sends SIGHUP, which a program can ignore.
        // Only the process this runtime recorded for the pane is signalled.
        let recorded = lock(&inner.terminals).get(id).and_then(|t| t.pid);
        let query = Command::new("display-message")
            .and_then(|c| c.arg(Argument::Flag("-p")))
            .and_then(|c| c.arg(Argument::Flag("-t")))
            .and_then(|c| c.arg(Argument::Pane(pane)))
            .and_then(|c| c.arg(Argument::Format("#{pane_pid} #{pane_dead}")))
            .map_err(invalid)?;
        let reply = inner.call(&link, &[query], deadline)?;
        let running = reply[0]
            .lines
            .first()
            .filter(|_| !reply[0].failed)
            .and_then(|line| std::str::from_utf8(line).ok())
            .and_then(|line| line.strip_suffix(" 0"))
            .and_then(|pid| pid.parse::<u32>().ok())
            .filter(|&pid| Some(pid) == recorded);
        if let Some(pid) = running {
            stop_group(pid, deadline);
        }
        // A failed reply means the window had gone already.
        match inner.call(&link, &[kill_window(window).map_err(invalid)?], deadline) {
            Ok(_) => {}
            // Its program is stopped, and the server has ended with its last window.
            Err(_) if running.is_some() && !link.conn.is_open() => {}
            Err(e) => return Err(e),
        }
        lock(&inner.terminals).died(id);
        inner.changed.notify_all();
        Ok(())
    }
}

/// `SIGTERM` to a process group, then `SIGKILL` if it is still there after [`KILL_GRACE`].
fn stop_group(leader: u32, deadline: Instant) {
    use rustix::process::{Pid, Signal, kill_process_group, test_kill_process_group};
    let Some(group) = i32::try_from(leader).ok().and_then(Pid::from_raw) else {
        return;
    };
    if kill_process_group(group, Signal::TERM).is_err() {
        return;
    }
    let grace = (Instant::now() + KILL_GRACE).min(deadline);
    while Instant::now() < grace {
        if test_kill_process_group(group).is_err() {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let _ = kill_process_group(group, Signal::KILL);
}

/// A serialising lock with a deadline.
#[derive(Default)]
struct Gate {
    busy: Mutex<bool>,
    free: Condvar,
}

struct Pass<'a>(&'a Gate);

impl Gate {
    fn enter(&self, deadline: Instant) -> Option<Pass<'_>> {
        let mut busy = lock(&self.busy);
        while *busy {
            let left = deadline.checked_duration_since(Instant::now())?;
            busy = self
                .free
                .wait_timeout(busy, left)
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
        *busy = true;
        Some(Pass(self))
    }
}

impl Drop for Pass<'_> {
    fn drop(&mut self) {
        *lock(&self.0.busy) = false;
        self.0.free.notify_one();
    }
}

fn check_size(cols: u16, rows: u16) -> Result<(u16, u16), String> {
    let ok = 1..=MAX_SIZE;
    if ok.contains(&cols) && ok.contains(&rows) {
        Ok((cols, rows))
    } else {
        Err(format!(
            "a terminal must be 1 to {MAX_SIZE} columns and rows, not {cols}x{rows}"
        ))
    }
}

/// `new-window` for a spec: detached, printing its ids, with the name, directory and variables
/// as literal text, and the program, found as a file, run by [`WRAPPER`].
fn new_window(spec: &StartSpec, name: &str, options: &TmuxOptions) -> Result<Command, String> {
    if spec.program.is_empty() {
        return Err("the program is empty".into());
    }
    if spec.program.starts_with('-') {
        return Err("a program name may not start with '-'".into());
    }
    if let Some((key, _)) = spec.env.iter().find(|(key, _)| !state::is_env_name(key)) {
        return Err(format!("{key:?} is not a variable name"));
    }
    let cwd = Path::new(&spec.cwd);
    if !cwd.is_absolute() {
        return Err(format!(
            "the working directory {:?} is not absolute",
            spec.cwd
        ));
    }
    if !cwd.is_dir() {
        return Err(format!(
            "the working directory {:?} does not exist",
            spec.cwd
        ));
    }
    let path = spec
        .env
        .iter()
        .chain(&options.env)
        .find(|(key, _)| key == "PATH")
        .map(|(_, value)| std::ffi::OsString::from(value))
        .or_else(|| std::env::var_os("PATH"));
    let program = super::exe::find(&spec.program, path.as_deref(), cwd)
        .ok_or_else(|| format!("{:?} is not an executable file on PATH", spec.program))?;
    let program = program
        .to_str()
        .ok_or_else(|| format!("{} is not UTF-8", program.display()))?;
    let build = || -> Result<Command, FormatError> {
        let mut command = Command::new("new-window")?
            .arg(Argument::Flag("-d"))?
            .arg(Argument::Flag("-P"))?
            .arg(Argument::Flag("-F"))?
            .arg(Argument::Format("#{window_id} #{pane_id} #{pane_pid}"))?;
        if !name.is_empty() {
            command = command
                .arg(Argument::Flag("-n"))?
                .arg(Argument::Name(name))?;
        }
        command = command
            .arg(Argument::Flag("-c"))?
            .arg(Argument::FormatLiteral(&spec.cwd))?;
        for (key, value) in &spec.env {
            command = command
                .arg(Argument::Flag("-e"))?
                .arg(Argument::Text(&format!("{key}={value}")))?;
        }
        command = command
            .arg(Argument::Flag("--"))?
            .arg(Argument::Text("/bin/sh"))?
            .arg(Argument::Text("-c"))?
            .arg(Argument::Text(WRAPPER))?
            .arg(Argument::Text(program))?;
        for arg in &spec.args {
            command = command.arg(Argument::Text(arg))?;
        }
        Ok(command)
    };
    build().map_err(|e| e.to_string())
}

/// `@1 %1 4242` (the pid is optional) from `new-window -P`.
fn parse_ids(reply: &CommandReply) -> Option<(WindowId, PaneId, Option<u32>)> {
    let line = std::str::from_utf8(reply.lines.first()?).ok()?;
    let mut fields = line.split(' ');
    let window = WindowId::parse(fields.next()?.as_bytes())?;
    let pane = PaneId::parse(fields.next()?.as_bytes())?;
    let pid = fields.next().and_then(|p| p.parse().ok());
    Some((window, pane, pid))
}

fn set_pane_option(pane: PaneId, option: &str, value: &str) -> Result<Command, FormatError> {
    Command::new("set-option")?
        .arg(Argument::Flag("-p"))?
        .arg(Argument::Flag("-t"))?
        .arg(Argument::Pane(pane))?
        .arg(Argument::Text(option))?
        .arg(Argument::Text(value))
}

fn offset_command(pane: PaneId, offset: u64) -> Result<Command, FormatError> {
    set_pane_option(pane, OFFSET_OPTION, &offset.to_string())
}

fn kill_window(window: WindowId) -> Result<Command, FormatError> {
    Command::new("kill-window")?
        .arg(Argument::Flag("-t"))?
        .arg(Argument::Window(window))
}

fn exited(id: TerminalId) -> RuntimeError {
    RuntimeError::Io(std::io::Error::new(
        std::io::ErrorKind::BrokenPipe,
        format!("terminal {id} has exited"),
    ))
}

fn invalid(error: FormatError) -> RuntimeError {
    RuntimeError::Io(std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        error.to_string(),
    ))
}

fn failed(reply: &CommandReply) -> RuntimeError {
    RuntimeError::Io(std::io::Error::other(format!(
        "tmux: {}",
        reply_text(reply)
    )))
}

fn call_error(error: CallError) -> String {
    match error {
        CallError::Closed => "the tmux control connection closed".into(),
        CallError::Busy => "too many commands are waiting for tmux".into(),
        CallError::TimedOut => "tmux did not answer in time".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_server_must_say_a_supported_version_its_pid_and_session() {
        assert_eq!(
            server_facts("3.2a 4242 $0"),
            Ok(Server {
                pid: 4242,
                session: "$0".into()
            })
        );
        for old in ["3.1c 1 $0", "2.7 1 $0", "openbsd-6.8 1 $0"] {
            assert!(
                server_facts(old).expect_err(old).contains("3.2 or newer"),
                "{old}"
            );
        }
        for bad in [
            "",
            "garbage",
            "3.2a",
            "3.2a x $0",
            "3.2a 1 0",
            "3.2a 1 $",
            "3.2a 1 $x",
        ] {
            assert!(server_facts(bad).is_err(), "{bad}");
        }
    }
}
