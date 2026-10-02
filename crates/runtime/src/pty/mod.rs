//! The PTY runtime: terminals owned by `pitcrew-ptyd`, a small supervisor that outlives
//! `pitcrewd`, so restarting or upgrading the daemon never ends a session. For machines without
//! tmux, native Windows above all (ConPTY there).
//!
//! - **One ptyd per user**, on [`PtyOptions::endpoint`]: on Unix a socket in a private directory
//!   (0700, the same rules as the tmux socket: a private `$XDG_RUNTIME_DIR`, else
//!   `/tmp/pitcrew-<uid>`, checked before every connection), on Windows a named pipe only the
//!   current user can open. Each side checks the other is the same user.
//! - **[`PtyRuntime`]** is its client. It starts ptyd (found next to the running executable,
//!   never on `PATH`) when a terminal is started and none runs, and reconnects as needed.
//!   Output offsets live in ptyd, so a reconnect or a new runtime loses none: after a daemon
//!   restart, [`Runtime::list`] finds the terminals and output goes on from the last offset.
//! - **The protocol** ([`proto`]) is ptyd's own, versioned apart from API v1.
//!
//! [`windows`] holds the only `unsafe` code of this crate and of pitcrew-ptyd.

mod client;
pub mod launch;
pub mod proto;
#[cfg(windows)]
pub mod windows;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use pitcrew_interfaces::runtime::{
    OutputChunk, Runtime, RuntimeError, RuntimeKind, Screen, StartSpec, TerminalInfo,
};
use pitcrew_protocol::ids::TerminalId;
use pitcrew_protocol::runner::{Capability, Key};

use self::client::{CallError, Conn, ConnectError};
use self::proto::{Chunk, Failure, FailureKind, MAX_READ, MAX_WAIT_MS, Op, Terminal};
use crate::background::Background;
use crate::gate::Gate;
use crate::lock;
use crate::screen::MAX_SIZE;

/// Input sent per `write` request.
const WRITE_CHUNK: usize = 256 << 10;
/// How long a starting ptyd is waited for before it is started again.
const RELAUNCH: Duration = Duration::from_secs(3);

/// Where pitcrew-ptyd is and how to reach it.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct PtyOptions {
    /// The pitcrew-ptyd executable. Default: next to the running executable
    /// ([`launch::beside_current_exe`]); `PATH` is never searched.
    pub ptyd: PathBuf,
    /// On Unix the socket's path, in a directory only the user may open (0700, created if
    /// missing). On Windows the pipe's name, `\\.\pipe\…`.
    pub endpoint: PathBuf,
    /// Variables for a ptyd this runtime starts; it, and every terminal it starts, has them
    /// on top of this process's environment.
    pub env: Vec<(String, String)>,
    /// The longest any call waits, except `start`.
    pub call_timeout: Duration,
    /// The longest `start` waits, including starting ptyd.
    pub start_timeout: Duration,
    /// Output history a ptyd this runtime starts keeps per terminal, in bytes.
    pub history: usize,
    /// How long a ptyd this runtime starts waits, with no terminals and no clients, before it
    /// exits. `None`: its default (30 seconds).
    pub idle_exit: Option<Duration>,
}

impl PtyOptions {
    /// Options for a ptyd on `endpoint`, with defaults for the rest.
    pub fn new(endpoint: impl Into<PathBuf>) -> Self {
        Self {
            ptyd: launch::beside_current_exe().unwrap_or_else(|| PathBuf::from(launch::PTYD)),
            endpoint: endpoint.into(),
            env: Vec::new(),
            call_timeout: Duration::from_secs(5),
            start_timeout: Duration::from_secs(15),
            history: crate::replay::DEFAULT_CAPACITY,
            idle_exit: None,
        }
    }

    /// Where the user's ptyd listens by default.
    ///
    /// - Unix: `$XDG_RUNTIME_DIR/pitcrew/ptyd` if that directory is private (it usually is; the
    ///   system removes it when the user's last login ends, which ends ptyd's reach too), else
    ///   `/tmp/pitcrew-<uid>/ptyd`, next to the tmux runtime's socket.
    /// - Windows: `\\.\pipe\pitcrew-ptyd-<the user's SID>`.
    pub fn default_endpoint() -> PathBuf {
        #[cfg(unix)]
        {
            let uid = rustix::process::getuid().as_raw();
            std::env::var_os("XDG_RUNTIME_DIR")
                .map(PathBuf::from)
                .filter(|dir| dir.is_absolute() && crate::tmux::socket::is_private_dir(dir))
                .map(|dir| dir.join("pitcrew").join("ptyd"))
                .filter(|socket| socket.as_os_str().len() <= 100)
                .unwrap_or_else(|| PathBuf::from(format!("/tmp/pitcrew-{uid}/ptyd")))
        }
        #[cfg(windows)]
        {
            let user = windows::current_user_sid().unwrap_or_else(|_| "unknown".into());
            PathBuf::from(format!(r"\\.\pipe\pitcrew-ptyd-{user}"))
        }
        #[cfg(not(any(unix, windows)))]
        {
            PathBuf::from("pitcrew-ptyd")
        }
    }
}

impl Default for PtyOptions {
    fn default() -> Self {
        Self::new(Self::default_endpoint())
    }
}

/// Checks that `endpoint` is safe to listen on or connect to: on Unix, a socket path in a
/// private directory of the user's (created 0700 if missing; see the tmux runtime's rules),
/// with nothing but a socket of the user's there; on Windows, a local pipe name.
///
/// # Errors
///
/// Why it is not.
pub fn check_endpoint(endpoint: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        crate::tmux::socket::ensure_private(endpoint)
    }
    #[cfg(not(unix))]
    {
        let name = endpoint.to_string_lossy();
        let prefix = r"\\.\pipe\";
        let named = name
            .get(..prefix.len())
            .is_some_and(|p| p.eq_ignore_ascii_case(prefix));
        if !named || name.len() == prefix.len() || name.len() > 256 {
            return Err(format!("{name} is not a local pipe name (\\\\.\\pipe\\…)"));
        }
        Ok(())
    }
}

/// The PTY runtime is usable: the runner can report [`Capability::Pty`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct PtySupport {
    /// The pitcrew-ptyd executable.
    pub ptyd: PathBuf,
    /// Where it listens.
    pub endpoint: PathBuf,
}

impl PtySupport {
    /// The capability this gives a runner.
    pub fn capability(&self) -> Capability {
        Capability::Pty
    }
}

/// Checks that pitcrew-ptyd is where `options` says (a program file at an absolute path) and
/// that its endpoint is safe ([`check_endpoint`], which creates the Unix socket's directory).
/// It starts nothing. Quick, but it touches the file system: from async code use
/// [`detect_async`].
///
/// # Errors
///
/// `Unavailable`, with the reason for people.
pub fn detect(options: &PtyOptions) -> Result<PtySupport, RuntimeError> {
    if !cfg!(any(unix, windows)) {
        return Err(RuntimeError::Unavailable(
            "the PTY runtime runs on Unix and Windows only".into(),
        ));
    }
    if !options.ptyd.is_absolute() || !launch::is_program(&options.ptyd) {
        return Err(RuntimeError::Unavailable(format!(
            "pitcrew-ptyd is not installed at {} (next to PitCrew's own executable)",
            options.ptyd.display()
        )));
    }
    check_endpoint(&options.endpoint).map_err(RuntimeError::Unavailable)?;
    #[cfg(windows)]
    windows::current_user_sid()
        .map_err(|e| RuntimeError::Unavailable(format!("cannot read the current user: {e}")))?;
    Ok(PtySupport {
        ptyd: options.ptyd.clone(),
        endpoint: options.endpoint.clone(),
    })
}

/// [`detect`] on its own thread, as a future that any executor can await without blocking.
pub fn detect_async(options: PtyOptions) -> Background<Result<PtySupport, RuntimeError>> {
    crate::background::spawn(
        "pitcrew-pty-detect",
        move || detect(&options),
        |why| {
            Err(RuntimeError::Unavailable(format!(
                "cannot start the PTY check: {why}"
            )))
        },
    )
}

/// PitCrew's terminals in pitcrew-ptyd (see the [module docs](self)).
///
/// - Every call is bounded by [`PtyOptions::call_timeout`] (`start` by
///   [`PtyOptions::start_timeout`]) and answers `Unavailable` past it. Input that timed out may
///   still reach the terminal.
/// - `start` starts ptyd if none runs. Other calls only connect: with no ptyd there are no
///   terminals (`list` is empty, a terminal is `NotFound`).
/// - A lost connection is replaced on the next call; reads, `screen`, `info`, `list`, `resize`
///   and `kill` try once more on a new connection, input and `start` do not (they may have
///   taken effect).
/// - Dropping the runtime closes its connection; the terminals keep running in ptyd.
/// - `TerminalInfo::pid` is the program's own process. Nothing can attach to a PTY by hand, so
///   `native_target` is `None`.
/// - `screen()` may make ptyd emulate up to its work budget: call it, like every method here,
///   from a blocking thread rather than an async executor's.
pub struct PtyRuntime {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for PtyRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PtyRuntime")
            .field("endpoint", &self.inner.options.endpoint)
            .finish_non_exhaustive()
    }
}

struct Inner {
    options: PtyOptions,
    conn: Mutex<Option<Arc<Conn>>>,
    connecting: Gate,
}

/// Why there is no connection.
enum Down {
    /// No ptyd runs: there are no terminals.
    NotRunning,
    Unavailable(String),
}

/// Why a request has no answer.
enum Asked {
    Down(Down),
    Call(CallError),
}

impl From<Down> for Asked {
    fn from(down: Down) -> Self {
        Self::Down(down)
    }
}

impl PtyRuntime {
    /// A runtime for the ptyd on `options.endpoint`. It connects (and starts ptyd) only when
    /// first needed.
    ///
    /// # Errors
    ///
    /// `Unavailable` if the endpoint is not safe ([`check_endpoint`]).
    pub fn new(options: PtyOptions) -> Result<Self, RuntimeError> {
        check_endpoint(&options.endpoint).map_err(RuntimeError::Unavailable)?;
        Ok(Self {
            inner: Arc::new(Inner {
                options,
                conn: Mutex::new(None),
                connecting: Gate::default(),
            }),
        })
    }

    /// Where ptyd listens.
    pub fn endpoint(&self) -> &Path {
        &self.inner.options.endpoint
    }

    /// The process id of the ptyd this runtime is connected to, if it is.
    pub fn ptyd_pid(&self) -> Option<u32> {
        self.inner.current().map(|conn| conn.hello.pid)
    }

    /// Closes the connection to ptyd; the next call opens a new one. Terminals are not affected.
    pub fn disconnect(&self) {
        if let Some(conn) = lock(&self.inner.conn).take() {
            conn.close(Duration::from_secs(1));
        }
    }

    /// Waits until a terminal's output goes past `offset` or its program ends, for at most
    /// `timeout`, and returns where its output ends now.
    ///
    /// # Errors
    ///
    /// `NotFound` for a terminal ptyd does not have; `Unavailable` if ptyd cannot be reached.
    pub fn wait_for_output(
        &self,
        id: TerminalId,
        offset: u64,
        timeout: Duration,
    ) -> Result<u64, RuntimeError> {
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let wait = u64::try_from(left.as_millis())
                .unwrap_or(u64::MAX)
                .min(MAX_WAIT_MS);
            let (chunk, _) = self.read(id, offset, 0, wait)?;
            if chunk.end > offset || !chunk.alive || Instant::now() >= deadline {
                return Ok(chunk.end);
            }
        }
    }

    fn read(
        &self,
        id: TerminalId,
        from: u64,
        max: usize,
        wait_ms: u64,
    ) -> Result<(Chunk, Vec<u8>), RuntimeError> {
        let deadline = Instant::now()
            + self.inner.options.call_timeout
            + Duration::from_millis(wait_ms.min(MAX_WAIT_MS));
        let op = Op::Read {
            terminal: id,
            from,
            max: max.min(MAX_READ) as u64,
            wait_ms,
        };
        let (value, data) = self
            .inner
            .ask(op, Vec::new(), deadline, false, true)
            .map_err(|e| terminal_error(id, e))?;
        Ok((decode(value)?, data))
    }

    fn terminal(&self, op: Op, id: TerminalId, retry: bool) -> Result<Terminal, RuntimeError> {
        let deadline = Instant::now() + self.inner.options.call_timeout;
        let (value, _) = self
            .inner
            .ask(op, Vec::new(), deadline, false, retry)
            .map_err(|e| terminal_error(id, e))?;
        decode(value)
    }

    fn act(
        &self,
        op: Op,
        id: TerminalId,
        payload: Vec<u8>,
        retry: bool,
    ) -> Result<(), RuntimeError> {
        let deadline = Instant::now() + self.inner.options.call_timeout;
        self.inner
            .ask(op, payload, deadline, false, retry)
            .map(drop)
            .map_err(|e| terminal_error(id, e))
    }
}

impl Drop for PtyRuntime {
    fn drop(&mut self) {
        self.disconnect();
    }
}

impl Inner {
    fn current(&self) -> Option<Arc<Conn>> {
        lock(&self.conn)
            .as_ref()
            .filter(|conn| conn.is_open())
            .cloned()
    }

    /// The connection, opening one if there is none; with `spawn`, starting ptyd if none runs.
    fn connection(&self, deadline: Instant, spawn: bool) -> Result<Arc<Conn>, Down> {
        if let Some(conn) = self.current() {
            return Ok(conn);
        }
        let _pass = self
            .connecting
            .enter(deadline)
            .ok_or_else(|| Down::Unavailable("busy connecting to pitcrew-ptyd".into()))?;
        if let Some(conn) = self.current() {
            return Ok(conn);
        }
        let endpoint = &self.options.endpoint;
        let mut patience = None;
        loop {
            match Conn::open(endpoint, deadline, patience) {
                Ok(conn) => {
                    let conn = Arc::new(conn);
                    *lock(&self.conn) = Some(Arc::clone(&conn));
                    tracing::debug!(pid = conn.hello.pid, version = %conn.hello.version, "connected to pitcrew-ptyd");
                    return Ok(conn);
                }
                Err(ConnectError::Absent) if spawn && Instant::now() < deadline => {
                    launch::launch(&self.options, deadline).map_err(Down::Unavailable)?;
                    // Wait for it to listen; if it never does (it lost a race with one that
                    // was exiting, say), start it again.
                    patience = Some(Instant::now() + RELAUNCH);
                }
                Err(ConnectError::Absent) if spawn => {
                    return Err(Down::Unavailable(format!(
                        "pitcrew-ptyd did not start on {} in time",
                        endpoint.display()
                    )));
                }
                Err(ConnectError::Absent) => return Err(Down::NotRunning),
                Err(ConnectError::Unsafe(why) | ConnectError::Failed(why)) => {
                    return Err(Down::Unavailable(why));
                }
                Err(ConnectError::TimedOut) => {
                    return Err(Down::Unavailable(format!(
                        "pitcrew-ptyd on {} did not answer in time",
                        endpoint.display()
                    )));
                }
                Err(ConnectError::Protocol(hello)) => {
                    return Err(Down::Unavailable(format!(
                        "pitcrew-ptyd {} (pid {}) speaks protocol {}, and this PitCrew speaks {}. \
                         Its terminals keep running: end them, or stop that ptyd, to use this \
                         version",
                        hello.version,
                        hello.pid,
                        hello.protocol,
                        proto::PROTOCOL
                    )));
                }
            }
        }
    }

    fn forget(&self, conn: &Arc<Conn>) {
        let mut current = lock(&self.conn);
        if current.as_ref().is_some_and(|c| Arc::ptr_eq(c, conn)) {
            *current = None;
        }
        drop(current);
        conn.close(Duration::ZERO);
    }

    /// Sends a request; with `retry`, once more on a new connection if the first one closed.
    fn ask(
        &self,
        op: Op,
        payload: Vec<u8>,
        deadline: Instant,
        spawn: bool,
        retry: bool,
    ) -> Result<(serde_json::Value, Vec<u8>), Asked> {
        let conn = self.connection(deadline, spawn)?;
        let again = retry.then(|| (op.clone(), payload.clone()));
        match conn.call(op, payload, deadline) {
            Err(CallError::Closed) => {
                self.forget(&conn);
                let Some((op, payload)) = again else {
                    return Err(Asked::Down(Down::Unavailable(
                        "the connection to pitcrew-ptyd closed".into(),
                    )));
                };
                let conn = self.connection(deadline, spawn)?;
                conn.call(op, payload, deadline).map_err(Asked::Call)
            }
            answered => answered.map_err(Asked::Call),
        }
    }
}

fn decode<T: serde::de::DeserializeOwned>(value: serde_json::Value) -> Result<T, RuntimeError> {
    serde_json::from_value(value).map_err(|e| {
        RuntimeError::Io(std::io::Error::other(format!(
            "an answer from pitcrew-ptyd that does not parse: {e}"
        )))
    })
}

fn exited(id: TerminalId) -> RuntimeError {
    RuntimeError::Io(std::io::Error::new(
        std::io::ErrorKind::BrokenPipe,
        format!("terminal {id} has exited"),
    ))
}

/// A failure for a call about terminal `id`.
fn terminal_error(id: TerminalId, asked: Asked) -> RuntimeError {
    match asked {
        // No ptyd: no terminals.
        Asked::Down(Down::NotRunning) => RuntimeError::NotFound(id),
        Asked::Call(CallError::Failed(Failure {
            kind: FailureKind::NotFound,
            ..
        })) => RuntimeError::NotFound(id),
        Asked::Call(CallError::Failed(Failure {
            kind: FailureKind::Exited,
            ..
        })) => exited(id),
        other => error(other, ""),
    }
}

/// A failure; `program` is the one being started, if any.
fn error(asked: Asked, program: &str) -> RuntimeError {
    match asked {
        Asked::Down(Down::NotRunning) => {
            RuntimeError::Unavailable("pitcrew-ptyd is not running".into())
        }
        Asked::Down(Down::Unavailable(why)) => RuntimeError::Unavailable(why),
        Asked::Call(CallError::Closed) => {
            RuntimeError::Unavailable("the connection to pitcrew-ptyd closed".into())
        }
        Asked::Call(CallError::Busy) => {
            RuntimeError::Unavailable("too many calls are waiting for pitcrew-ptyd".into())
        }
        Asked::Call(CallError::TimedOut) => {
            RuntimeError::Unavailable("pitcrew-ptyd did not answer in time".into())
        }
        Asked::Call(CallError::Invalid(why)) => invalid(why),
        Asked::Call(CallError::Failed(Failure { kind, message })) => match kind {
            FailureKind::Spawn => RuntimeError::Spawn {
                program: program.to_owned(),
                reason: message,
            },
            FailureKind::Invalid => invalid(message),
            FailureKind::Busy | FailureKind::Unsupported => RuntimeError::Unavailable(message),
            FailureKind::NotFound | FailureKind::Exited | FailureKind::Io => {
                RuntimeError::Io(std::io::Error::other(message))
            }
        },
    }
}

fn invalid(why: String) -> RuntimeError {
    RuntimeError::Io(std::io::Error::new(std::io::ErrorKind::InvalidInput, why))
}

fn check_size(cols: u16, rows: u16) -> Result<(), String> {
    let ok = 1..=MAX_SIZE;
    if ok.contains(&cols) && ok.contains(&rows) {
        Ok(())
    } else {
        Err(format!(
            "a terminal must be 1 to {MAX_SIZE} columns and rows, not {cols}x{rows}"
        ))
    }
}

impl Runtime for PtyRuntime {
    fn kind(&self) -> RuntimeKind {
        RuntimeKind::Pty
    }

    fn start(&self, spec: &StartSpec) -> Result<TerminalInfo, RuntimeError> {
        let spawn = |reason: String| RuntimeError::Spawn {
            program: spec.program.clone(),
            reason,
        };
        check_size(spec.cols, spec.rows).map_err(spawn)?;
        if spec.program.is_empty() {
            return Err(spawn("the program is empty".into()));
        }
        let mut argv = Vec::with_capacity(spec.args.len() + 1);
        argv.push(spec.program.clone());
        argv.extend(spec.args.iter().cloned());
        let op = Op::Start {
            argv,
            cwd: spec.cwd.clone(),
            env: spec.env.clone(),
            name: spec.name.clone(),
            cols: spec.cols,
            rows: spec.rows,
        };
        let deadline = Instant::now() + self.inner.options.start_timeout;
        let (value, _) = self
            .inner
            .ask(op, Vec::new(), deadline, true, false)
            .map_err(|e| error(e, &spec.program))?;
        decode::<Terminal>(value).map(|t| t.info())
    }

    fn write(&self, id: TerminalId, bytes: &[u8]) -> Result<(), RuntimeError> {
        if bytes.is_empty() {
            return self.act(Op::Write { terminal: id }, id, Vec::new(), false);
        }
        for chunk in bytes.chunks(WRITE_CHUNK) {
            self.act(Op::Write { terminal: id }, id, chunk.to_vec(), false)?;
        }
        Ok(())
    }

    fn send_keys(&self, id: TerminalId, keys: &[Key]) -> Result<(), RuntimeError> {
        let op = Op::Keys {
            terminal: id,
            keys: keys.to_vec(),
        };
        self.act(op, id, Vec::new(), false)
    }

    fn resize(&self, id: TerminalId, cols: u16, rows: u16) -> Result<(), RuntimeError> {
        check_size(cols, rows).map_err(invalid)?;
        let op = Op::Resize {
            terminal: id,
            cols,
            rows,
        };
        self.act(op, id, Vec::new(), true)
    }

    fn screen(&self, id: TerminalId) -> Result<Screen, RuntimeError> {
        let deadline = Instant::now() + self.inner.options.call_timeout;
        let (value, _) = self
            .inner
            .ask(
                Op::Screen { terminal: id },
                Vec::new(),
                deadline,
                false,
                true,
            )
            .map_err(|e| terminal_error(id, e))?;
        decode(value)
    }

    fn read_output(
        &self,
        id: TerminalId,
        from: u64,
        max: usize,
    ) -> Result<OutputChunk, RuntimeError> {
        let (chunk, data) = self.read(id, from, max, 0)?;
        Ok(OutputChunk {
            offset: chunk.offset,
            data,
            end: chunk.end,
            truncated: chunk.truncated,
        })
    }

    fn info(&self, id: TerminalId) -> Result<TerminalInfo, RuntimeError> {
        self.terminal(Op::Info { terminal: id }, id, true)
            .map(|t| t.info())
    }

    fn list(&self) -> Result<Vec<TerminalInfo>, RuntimeError> {
        let deadline = Instant::now() + self.inner.options.call_timeout;
        match self.inner.ask(Op::List, Vec::new(), deadline, false, true) {
            Ok((value, _)) => {
                let mut all: Vec<TerminalInfo> = decode::<Vec<Terminal>>(value)?
                    .iter()
                    .map(Terminal::info)
                    .collect();
                all.sort_by_key(|t| t.id);
                Ok(all)
            }
            Err(Asked::Down(Down::NotRunning)) => Ok(Vec::new()),
            Err(e) => Err(error(e, "")),
        }
    }

    fn kill(&self, id: TerminalId) -> Result<(), RuntimeError> {
        self.act(Op::Kill { terminal: id }, id, Vec::new(), true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_endpoint_is_private_and_short() {
        let endpoint = PtyOptions::default_endpoint();
        #[cfg(unix)]
        {
            assert!(endpoint.is_absolute());
            assert!(endpoint.ends_with("ptyd"), "{}", endpoint.display());
            assert!(endpoint.as_os_str().len() <= 103);
        }
        #[cfg(windows)]
        {
            let name = endpoint.to_string_lossy();
            assert!(name.starts_with(r"\\.\pipe\pitcrew-ptyd-S-1-"), "{name}");
            assert!(check_endpoint(&endpoint).is_ok());
            assert!(check_endpoint(Path::new(r"C:\pipe\x")).is_err());
            assert!(check_endpoint(Path::new(r"\\server\pipe\x")).is_err());
        }
        let options = PtyOptions::new("/tmp/x/ptyd");
        assert_eq!(options.call_timeout, Duration::from_secs(5));
        assert_eq!(options.idle_exit, None);
    }

    #[test]
    fn sizes_are_checked_before_ptyd_is_asked() {
        assert!(check_size(1, 1000).is_ok());
        assert!(check_size(0, 10).is_err());
        assert!(check_size(10, 1001).is_err());
    }

    #[test]
    fn failures_map_to_runtime_errors() {
        let id = TerminalId::new();
        let failed = |kind| {
            Asked::Call(CallError::Failed(Failure {
                kind,
                message: "m".into(),
            }))
        };
        assert!(matches!(
            terminal_error(id, Asked::Down(Down::NotRunning)),
            RuntimeError::NotFound(x) if x == id
        ));
        assert!(matches!(
            terminal_error(id, failed(FailureKind::NotFound)),
            RuntimeError::NotFound(_)
        ));
        assert!(matches!(
            terminal_error(id, failed(FailureKind::Exited)),
            RuntimeError::Io(e) if e.kind() == std::io::ErrorKind::BrokenPipe
        ));
        assert!(matches!(
            error(failed(FailureKind::Spawn), "claude"),
            RuntimeError::Spawn { program, .. } if program == "claude"
        ));
        assert!(matches!(
            error(failed(FailureKind::Invalid), ""),
            RuntimeError::Io(e) if e.kind() == std::io::ErrorKind::InvalidInput
        ));
        assert!(matches!(
            error(Asked::Call(CallError::TimedOut), ""),
            RuntimeError::Unavailable(_)
        ));
    }
}
