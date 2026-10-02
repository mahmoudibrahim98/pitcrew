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
use crate::detect::{TmuxVersion, VersionKind};
use crate::gate::Gate;

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
const WRAPPER: &str = r#": pitcrew-wrapper; unset TMUX TMUX_PANE; trap : INT QUIT; "$0" "$@"; s=$?; sleep 0.2 2>/dev/null || sleep 1; exit "$s""#;

/// How tmux shows a pane started with [`WRAPPER`] in `#{pane_start_command}`.
const WRAPPER_STARTED: &str = r#"/bin/sh -c ": pitcrew-wrapper; "#;

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

/// Further tries of `new-session` when the server it reached went away under it.
const NEW_SESSION_TRIES: usize = 3;

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
/// - `screen()` emulates the output since the last read, up to a work budget: bounded, but call
///   it (like every method here) from a blocking thread, not an async executor's.
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
    /// Windows of abandoned starts whose kill has not been answered yet, killed again on the
    /// next connection to the same server.
    pending_kills: Mutex<Vec<(ServerKey, WindowId)>>,
}

/// A tmux server instance: its pid and start time (a pid alone can be reused).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ServerKey {
    pid: u32,
    started: u64,
}

/// A tmux server, its version, and the id of PitCrew's session in it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Server {
    key: ServerKey,
    version: TmuxVersion,
    session: String,
}

/// The current control client.
struct Link {
    conn: Connection,
    generation: u64,
    /// The window that created the session, removed once a terminal exists.
    holder: Mutex<Option<WindowId>>,
    /// Which server this is, from reconcile.
    server: Mutex<Option<ServerKey>>,
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
                // The failed attach may have started a server that exits at once (it has no
                // session); a new session made just then dies with it ("server exited
                // unexpectedly", seen as no server). The next try gets a fresh server.
                let mut made = self.connect(&args, deadline);
                for _ in 0..NEW_SESSION_TRIES {
                    if !matches!(made, Err(ConnectError::NoSession)) || Instant::now() >= deadline {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(50));
                    made = self.connect(&args, deadline);
                }
                match made {
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
            server: Mutex::new(None),
        });
        self.reconcile(&link, deadline)?;
        if self.shutdown.load(Ordering::Acquire) {
            link.conn.kill();
            return Err(Down::Unavailable(
                "the tmux runtime is shutting down".into(),
            ));
        }
        self.replay_kills(&link, deadline);
        self.sweep_orphans(&link, deadline);
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
                // No server: whatever ran in it has ended, its windows too.
                lock(&self.terminals).all_died();
                lock(&self.pending_kills).clear();
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
        let unavailable = |e: FormatError| Down::Unavailable(e.to_string());
        let about = Command::new("display-message")
            .and_then(|c| c.arg(Argument::Flag("-p")))
            .and_then(|c| {
                c.arg(Argument::Format(
                    "#{version} #{pid} #{start_time} #{session_id}",
                ))
            })
            .map_err(unavailable)?;
        let reply = link
            .conn
            .call(&[about], deadline)
            .map_err(|e| Down::Unavailable(call_error(e)))?;
        let about = reply[0]
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
        // The global `window-size` depends on the server's version, so it follows the check.
        let commands = [
            Command::new("set-option")
                .and_then(|c| c.arg(Argument::Flag("-g")))
                .and_then(|c| c.arg(Argument::Text("window-size")))
                .and_then(|c| c.arg(Argument::Text(global_window_size(&server.version)))),
            Command::new("list-panes")
                .and_then(|c| c.arg(Argument::Flag("-s")))
                .and_then(|c| c.arg(Argument::Flag("-F")))
                .and_then(|c| c.arg(Argument::Format(LIST_FORMAT))),
        ]
        .into_iter()
        .collect::<Result<Vec<_>, FormatError>>()
        .map_err(unavailable)?;
        let replies = link
            .conn
            .call(&commands, deadline)
            .map_err(|e| Down::Unavailable(call_error(e)))?;
        for reply in &replies {
            if reply.failed {
                return Err(Down::Unavailable(format!("tmux: {}", reply_text(reply))));
            }
        }
        {
            let mut known = lock(&self.server);
            if known.as_ref().is_some_and(|k| k.key != server.key) {
                // Another server: whatever ran in the one before has ended with it.
                lock(&self.terminals).all_died();
                lock(&self.pending_kills).clear();
            }
            *lock(&link.server) = Some(server.key);
            *known = Some(server);
        }
        let listed: Vec<Listed> = replies[1]
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
        let server = *lock(&link.server);
        let kills: Vec<Command> = std::mem::take(&mut *lock(&self.pending_kills))
            .into_iter()
            .filter(|&(key, _)| Some(key) == server)
            .filter_map(|(_, window)| kill_window(window).ok())
            .collect();
        if !kills.is_empty() {
            // A failure means that window is gone already.
            let _ = link.conn.call(&kills, deadline);
        }
    }

    /// Kills, on a fresh connection and before any start uses it, the panes a start that never
    /// finished left behind: untagged, but started by [`WRAPPER`]. A start whose connection was
    /// lost before tmux answered leaves one. Only one runtime may use a socket.
    fn sweep_orphans(&self, link: &Link, deadline: Instant) {
        let Ok(list) = Command::new("list-panes")
            .and_then(|c| c.arg(Argument::Flag("-s")))
            .and_then(|c| c.arg(Argument::Flag("-F")))
            .and_then(|c| {
                c.arg(Argument::Format(
                    "#{window_id} #{@pitcrew-terminal}|#{pane_start_command}",
                ))
            })
        else {
            return;
        };
        let Ok(replies) = link.conn.call(&[list], deadline) else {
            return;
        };
        if replies[0].failed {
            return;
        }
        let known: Vec<WindowId> = lock(&self.terminals)
            .all()
            .filter(|t| t.alive)
            .map(|t| t.window)
            .collect();
        let kills: Vec<Command> = orphans(&replies[0].lines, &known)
            .into_iter()
            .filter_map(|window| kill_window(window).ok())
            .collect();
        if !kills.is_empty() {
            tracing::debug!(count = kills.len(), "removing windows of unfinished starts");
            let _ = link.conn.call(&kills, deadline);
        }
    }

    /// A window nobody will use (its start was given up): kill it now, and again on the next
    /// connection to the same server unless tmux answers this kill first.
    fn discard_window(&self, outbox: &Outbox, server: Option<ServerKey>, window: WindowId) {
        let Some(server) = server else {
            return;
        };
        {
            let mut pending = lock(&self.pending_kills);
            if pending.len() >= PENDING_KILLS {
                pending.remove(0);
            }
            pending.push((server, window));
        }
        let Ok(kill) = kill_window(window) else {
            return;
        };
        let weak = self.me.clone();
        // Any answer means the kill reached tmux (a failure: the window had gone already).
        let landed = Waiter::Then(Box::new(move |_| {
            if let Some(inner) = weak.upgrade() {
                lock(&inner.pending_kills).retain(|&entry| entry != (server, window));
            }
        }));
        let _ = outbox.send_with(std::slice::from_ref(&kill), vec![landed]);
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

/// The windows to remove among `list-panes` lines of `#{window_id} #{@pitcrew-terminal}|
/// #{pane_start_command}`: untagged, started by [`WRAPPER`], and not a known terminal's. A
/// window listed more than once (a forged row) is left alone.
fn orphans(lines: &[Vec<u8>], known: &[WindowId]) -> Vec<WindowId> {
    let rows: Vec<(WindowId, String)> = lines
        .iter()
        .filter_map(|line| {
            let line = String::from_utf8_lossy(line);
            let (window, rest) = line.split_once(' ')?;
            Some((WindowId::parse(window.as_bytes())?, rest.to_owned()))
        })
        .collect();
    let mut found = Vec::new();
    for (window, rest) in &rows {
        let Some((tag, started)) = rest.split_once('|') else {
            continue;
        };
        if tag.is_empty()
            && started.starts_with(WRAPPER_STARTED)
            && !known.contains(window)
            && rows.iter().filter(|(w, _)| w == window).count() == 1
        {
            found.push(*window);
        }
    }
    found
}

/// `#{version} #{pid} #{start_time} #{session_id}`: the server's facts, if its version is
/// supported.
fn server_facts(about: &str) -> Result<Server, String> {
    let mut fields = about.split(' ');
    let version = fields
        .next()
        .and_then(|v| format!("tmux {v}").parse::<TmuxVersion>().ok());
    let version = match version {
        Some(v) if v.is_supported() => v,
        other => {
            let shown = other.map_or_else(|| "an unknown version".to_owned(), |v| v.to_string());
            return Err(format!(
                "the running tmux server is {shown}; PitCrew needs 3.2 or newer"
            ));
        }
    };
    let pid = fields.next().and_then(|p| p.parse().ok());
    let started = fields.next().and_then(|s| s.parse().ok());
    let session = fields.next().filter(|s| {
        s.strip_prefix('$')
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
    });
    match (pid, started, session) {
        (Some(pid), Some(started), Some(session)) => Ok(Server {
            key: ServerKey { pid, started },
            version,
            session: session.to_owned(),
        }),
        _ => Err(format!("tmux did not say which server it is: {about:?}")),
    }
}

/// The server's global `window-size`. Every window `start` makes is sized `default-size` (set
/// just before) and then made `manual` itself, so attached clients never resize it; what the
/// global option adds depends on the version.
///
/// - **tmux 3.2:** `manual`, so a new window takes `default-size` whoever is attached.
/// - **tmux 3.3 and later:** `latest`. There a global `manual` crashes the server at its next
///   new window: `clients_calculate_size` reads the manual size of the window being made,
///   which does not exist yet (a NULL dereference, seen on 3.3a to 3.6a). With `latest`
///   a new window takes `default-size` unless someone is attached with a size of their own;
///   then it takes theirs, and `start` resizes it at once.
/// - **OpenBSD's own numbering** (which does not say which tmux it is): `latest`, which works
///   on every version.
fn global_window_size(version: &TmuxVersion) -> &'static str {
    match version.kind {
        VersionKind::Release | VersionKind::Next if (version.major, version.minor) < (3, 3) => {
            "manual"
        }
        _ => "latest",
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
        let server = *lock(&link.server);
        let default_size = Command::new("set-option")
            .and_then(|c| c.arg(Argument::Text("default-size")))
            .and_then(|c| c.arg(Argument::Text(&format!("{cols}x{rows}"))))
            .map_err(invalid)?;
        let holder = lock(&link.holder).take();
        let weak = inner.me.clone();
        // If this call gives up before tmux answers, the window it gets is removed then.
        let created = Pending::new(move |reply, outbox| {
            if let (Some(inner), Some(created)) = (weak.upgrade(), parse_created(reply))
                && !reply.failed
            {
                inner.discard_window(outbox, server, created.window);
            }
        });
        let sent = link.conn.outbox().send_with(
            &[default_size, new_window],
            vec![Waiter::Discard, Waiter::Pending(Arc::clone(&created))],
        );
        let created = sent
            .and_then(|()| created.wait(deadline))
            .map_err(|e| RuntimeError::Unavailable(call_error(e)))
            .and_then(|reply| match parse_created(&reply) {
                Some(created) if !reply.failed => Ok(created),
                _ => Err(spawn(format!("tmux: {}", reply_text(&reply)))),
            });
        let Created {
            window,
            pane,
            pid,
            size,
        } = match created {
            Ok(created) => created,
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
        // The window was made at `default-size` or, on tmux 3.3 and later, at the size of a
        // client attached with a size of its own. Then it is resized below; its first output
        // was drawn at the size it was made with.
        let (made_cols, made_rows) = size.unwrap_or((cols, rows));
        lock(&inner.terminals).claim(Claim {
            id,
            name,
            window,
            pane,
            pid,
            cols: made_cols,
            rows: made_rows,
            offset: 0,
            alive: true,
        });
        let resize = size != Some((cols, rows));
        let mut commands = vec![
            set_pane_option(pane, TERMINAL_OPTION, &id.to_string()).map_err(invalid)?,
            offset_command(pane, RESERVE).map_err(invalid)?,
            // Its own `window-size manual`: no client resizes it (see `global_window_size`).
            manual_window_size(window).map_err(invalid)?,
        ];
        if resize {
            commands.push(resize_window(window, cols, rows).map_err(invalid)?);
        }
        if let Some(holder) = holder {
            commands.push(kill_window(holder).map_err(invalid)?);
        }
        match inner.call(&link, &commands, deadline) {
            Ok(replies) if !replies[0].failed => {
                let mut terminals = lock(&inner.terminals);
                if !replies[1].failed {
                    terminals.reserved(id, RESERVE);
                }
                if resize && !replies[3].failed {
                    terminals.set_size(id, cols, rows);
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
        let command = resize_window(window, cols, rows).map_err(invalid)?;
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
            .filter(|&pid| Some(pid) == recorded && leads_session(pid));
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

/// True if `pid` leads its own session, as a pane's process does: then its process group is
/// the pane's, not one a reused pid happens to belong to.
fn leads_session(pid: u32) -> bool {
    use rustix::process::{Pid, getsid};
    i32::try_from(pid)
        .ok()
        .and_then(Pid::from_raw)
        .is_some_and(|pid| getsid(Some(pid)).is_ok_and(|sid| sid == pid))
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
            .arg(Argument::Format(
                "#{window_id} #{pane_id} #{pane_pid} #{pane_width} #{pane_height}",
            ))?;
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

/// What `new-window -P` says about the window it made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Created {
    window: WindowId,
    pane: PaneId,
    pid: Option<u32>,
    /// The pane's size: (cols, rows).
    size: Option<(u16, u16)>,
}

/// `@1 %1 4242 80 24` from `new-window -P` (the pid and the size are optional).
fn parse_created(reply: &CommandReply) -> Option<Created> {
    let line = std::str::from_utf8(reply.lines.first()?).ok()?;
    let mut fields = line.split(' ');
    let window = WindowId::parse(fields.next()?.as_bytes())?;
    let pane = PaneId::parse(fields.next()?.as_bytes())?;
    let pid = fields.next().and_then(|p| p.parse().ok());
    let mut number = || fields.next().and_then(|n| n.parse().ok());
    let size = number().zip(number());
    Some(Created {
        window,
        pane,
        pid,
        size,
    })
}

/// The window keeps the size the runtime gives it, whoever attaches.
fn manual_window_size(window: WindowId) -> Result<Command, FormatError> {
    Command::new("set-option")?
        .arg(Argument::Flag("-w"))?
        .arg(Argument::Flag("-t"))?
        .arg(Argument::Window(window))?
        .arg(Argument::Text("window-size"))?
        .arg(Argument::Text("manual"))
}

/// `resize-window` also makes the window's own `window-size` manual.
fn resize_window(window: WindowId, cols: u16, rows: u16) -> Result<Command, FormatError> {
    Command::new("resize-window")?
        .arg(Argument::Flag("-t"))?
        .arg(Argument::Window(window))?
        .arg(Argument::Flag("-x"))?
        .arg(Argument::Number(cols.into()))?
        .arg(Argument::Flag("-y"))?
        .arg(Argument::Number(rows.into()))
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
    fn the_server_must_say_a_supported_version_its_pid_start_and_session() {
        assert_eq!(
            server_facts("3.2a 4242 1790906720 $0"),
            Ok(Server {
                key: ServerKey {
                    pid: 4242,
                    started: 1_790_906_720
                },
                version: version("3.2a"),
                session: "$0".into()
            })
        );
        for newer in ["3.3a", "3.4", "3.5a", "next-3.6", "openbsd-7.4"] {
            let facts = server_facts(&format!("{newer} 1 1 $3")).expect(newer);
            assert_eq!(facts.version, version(newer));
        }
        for old in ["3.1c 1 1 $0", "2.7 1 1 $0", "openbsd-6.8 1 1 $0"] {
            assert!(
                server_facts(old).expect_err(old).contains("3.2 or newer"),
                "{old}"
            );
        }
        for bad in [
            "",
            "garbage",
            "3.2a",
            "3.2a x 1 $0",
            "3.2a 1 x $0",
            "3.2a 1 $0",
            "3.2a 1 1 0",
            "3.2a 1 1 $",
            "3.2a 1 1 $x",
        ] {
            assert!(server_facts(bad).is_err(), "{bad}");
        }
    }

    fn version(text: &str) -> TmuxVersion {
        format!("tmux {text}").parse().expect(text)
    }

    #[test]
    fn the_global_window_size_is_manual_only_where_new_windows_survive_it() {
        for old in ["3.2", "3.2a", "next-3.2"] {
            assert_eq!(global_window_size(&version(old)), "manual", "{old}");
        }
        // A global `manual` crashes 3.3 and later at the next new window; OpenBSD's numbering
        // does not say which tmux it is.
        for newer in [
            "next-3.3",
            "3.3",
            "3.3a",
            "3.4",
            "3.5",
            "3.5a",
            "3.6",
            "4.0",
            "openbsd-6.9",
            "openbsd-7.4",
        ] {
            assert_eq!(global_window_size(&version(newer)), "latest", "{newer}");
        }
    }

    #[test]
    fn new_window_replies_give_ids_and_the_size_made() {
        let reply = |line: &str| CommandReply {
            time: 1,
            number: 1,
            flags: 1,
            failed: false,
            lines: vec![line.as_bytes().to_vec()],
        };
        assert_eq!(
            parse_created(&reply("@3 %4 4242 80 24")),
            Some(Created {
                window: WindowId(3),
                pane: PaneId(4),
                pid: Some(4242),
                size: Some((80, 24)),
            })
        );
        let partial = parse_created(&reply("@3 %4  50")).expect("ids");
        assert_eq!((partial.pid, partial.size), (None, None));
        assert_eq!(parse_created(&reply("@3 %4")).map(|c| c.size), Some(None));
        for bad in ["", "%4 @3 1 80 24", "@3"] {
            assert_eq!(parse_created(&reply(bad)), None, "{bad}");
        }
    }

    #[test]
    fn only_untagged_panes_of_our_wrapper_are_orphans() {
        let known = [WindowId(1)];
        let line = |s: &str| s.as_bytes().to_vec();
        let ours = format!("{WRAPPER_STARTED}unset TMUX\" /usr/bin/sh");
        let lines = vec![
            line(&format!("@1 |{ours}")),
            line(&format!("@2 |{ours}")),
            line(&format!("@3 term_x|{ours}")),
            line("@4 |/bin/sh -c \"sleep 60\""),
            line("@5 |bash"),
            // A forged row for @6, which is also listed for real.
            line(&format!("@6 |{ours}")),
            line("@6 term_y|bash"),
            line("garbage"),
        ];
        assert_eq!(orphans(&lines, &known), vec![WindowId(2)]);
        // The marker is what tmux shows for the wrapper this runtime runs.
        assert!(format!("/bin/sh -c \"{WRAPPER}\"").starts_with(WRAPPER_STARTED));
    }

    #[test]
    fn only_a_session_leader_is_signalled_as_a_group() {
        assert!(!leads_session(std::process::id()));
        assert!(!leads_session(0));
        assert!(!leads_session(u32::MAX));
    }
}
