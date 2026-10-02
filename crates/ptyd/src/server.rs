//! Where ptyd listens, who may connect, and how requests are served.
//!
//! - **One per user and endpoint.** On Unix a lock file next to the socket (`flock`), taken
//!   before the socket is bound; on Windows the pipe's first instance (`FILE_FLAG_FIRST_PIPE_INSTANCE`).
//!   A second ptyd exits with status 3.
//! - **Who may connect.** On Unix the socket's directory is private (0700, checked before it is
//!   used) and every client's uid (`SO_PEERCRED`) must be ours. On Windows the pipe's DACL grants
//!   the current user alone, remote clients are refused (`PIPE_REJECT_REMOTE_CLIENTS`), and the
//!   user of every client's process token must be ours. Anyone else is disconnected at once.
//! - **Bounded.** At most 32 clients; a client says hello within 10 seconds; at most 64 of its
//!   slow requests (start, screen, read, kill) run at once; frames are capped by the protocol.
//! - **Idle exit.** With no terminals running and no client connected for a while (30 seconds
//!   by default), ptyd exits, removing its socket.

use std::ffi::OsString;
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use pitcrew_runtime::pty::check_endpoint;
use pitcrew_runtime::pty::proto::{
    self, Chunk, FailureKind, Frame, Hello, MAX_READ, MAX_WAIT_MS, MAX_WRITE, Op, PROTOCOL, Reply,
    Request,
};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{Semaphore, mpsc};

use crate::terms::{Failed, StartRequest, Term, Terms};
use crate::{ALREADY_RUNNING, Config, log};

/// How long ptyd stays with no terminals and no clients, by default.
pub(crate) const IDLE_EXIT: Duration = Duration::from_secs(30);
/// Clients at once, at most.
const MAX_CLIENTS: usize = 32;
/// A client must say hello within this.
const HELLO_WITHIN: Duration = Duration::from_secs(10);
/// Slow requests of one client running at once, at most.
const IN_FLIGHT: usize = 64;
/// Replies queued for one client.
const REPLIES: usize = 64;

struct State {
    terms: Arc<Terms>,
    clients: AtomicUsize,
}

/// Counts a client while it is connected, whatever ends its task.
struct Connected(Arc<State>);

impl Drop for Connected {
    fn drop(&mut self) {
        self.0.clients.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Runs ptyd: on Unix, unless `--foreground`, starts a detached copy that serves and returns.
pub(crate) fn run(config: &Config, args: &[OsString]) -> ExitCode {
    #[cfg(unix)]
    if !config.foreground {
        return unix::detach(config, args);
    }
    #[cfg(not(unix))]
    let _ = args;
    // Leave the starter's session: its terminal's signals, and its end, no longer reach us.
    #[cfg(unix)]
    let _ = rustix::process::setsid();
    if let Err(why) = check_endpoint(&config.endpoint) {
        log!("{why}");
        return ExitCode::FAILURE;
    }
    #[cfg(unix)]
    let _lock = match unix::Lock::take(&config.endpoint) {
        Ok(lock) => lock,
        Err(unix::LockError::Taken) => {
            log!(
                "another pitcrew-ptyd serves {} already",
                config.endpoint.display()
            );
            return ExitCode::from(ALREADY_RUNNING);
        }
        Err(unix::LockError::Failed(why)) => {
            log!("{why}");
            return ExitCode::FAILURE;
        }
    };
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("ptyd-io")
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            log!("cannot start: {e}");
            return ExitCode::FAILURE;
        }
    };
    let state = Arc::new(State {
        terms: Terms::new(config.history),
        clients: AtomicUsize::new(0),
    });
    let served = runtime.block_on(async {
        let listener = Listener::bind(&config.endpoint)?;
        log!(
            "{} (protocol {PROTOCOL}) serving on {}",
            env!("CARGO_PKG_VERSION"),
            config.endpoint.display()
        );
        serve(listener, &state, config.idle_exit).await;
        Ok::<(), std::io::Error>(())
    });
    runtime.shutdown_timeout(Duration::from_secs(2));
    #[cfg(unix)]
    let _ = std::fs::remove_file(&config.endpoint);
    match served {
        Ok(()) => {
            log!("no terminals and no clients: exiting");
            ExitCode::SUCCESS
        }
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied && cfg!(windows) => {
            log!(
                "another pitcrew-ptyd serves {} already (or someone else holds its name)",
                config.endpoint.display()
            );
            ExitCode::from(ALREADY_RUNNING)
        }
        Err(e) => {
            log!("cannot listen on {}: {e}", config.endpoint.display());
            ExitCode::FAILURE
        }
    }
}

async fn serve(mut listener: Listener, state: &Arc<State>, idle: Duration) {
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    let mut quiet_since: Option<Instant> = None;
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let stream = match accepted {
                    Ok(stream) => stream,
                    Err(e) => {
                        log!("accepting a client failed: {e}");
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        continue;
                    }
                };
                if let Err(why) = check_peer(&stream) {
                    log!("refused a client: {why}");
                    continue;
                }
                if state.clients.fetch_add(1, Ordering::AcqRel) >= MAX_CLIENTS {
                    state.clients.fetch_sub(1, Ordering::AcqRel);
                    log!("refused a client: {MAX_CLIENTS} are connected");
                    continue;
                }
                let connected = Connected(Arc::clone(state));
                tokio::spawn(async move {
                    client(stream, &connected.0).await;
                    drop(connected);
                });
            }
            _ = tick.tick() => {
                if state.clients.load(Ordering::Acquire) > 0 || state.terms.live() > 0 {
                    quiet_since = None;
                } else if quiet_since.get_or_insert_with(Instant::now).elapsed() >= idle {
                    return;
                }
            }
        }
    }
}

/// Serves one client until it disconnects.
async fn client<S: AsyncRead + AsyncWrite + Send + 'static>(stream: S, state: &Arc<State>) {
    let (mut reader, mut writer) = tokio::io::split(stream);
    let (replies, mut outgoing) = mpsc::channel::<Frame>(REPLIES);
    let writing = tokio::spawn(async move {
        while let Some(frame) = outgoing.recv().await {
            if proto::write_frame(&mut writer, &frame).await.is_err() {
                break;
            }
        }
    });
    if hello(&mut reader, &replies).await {
        let in_flight = Arc::new(Semaphore::new(IN_FLIGHT));
        // A read error or a frame over the limits ends the connection.
        while let Ok(Some(frame)) = proto::read_frame(&mut reader).await {
            let request = match serde_json::from_slice::<Request>(&frame.header) {
                Ok(request) => request,
                Err(e) => match serde_json::from_slice::<IdOnly>(&frame.header) {
                    Ok(IdOnly { id }) => {
                        let reply = Reply::err(id, FailureKind::Invalid, format!("{e}"));
                        if !send(&replies, reply, Vec::new()).await {
                            break;
                        }
                        continue;
                    }
                    Err(_) => break,
                },
            };
            if !handle(request, frame.payload, state, &replies, &in_flight).await {
                break;
            }
        }
    }
    drop(replies);
    let _ = writing.await;
}

#[derive(serde::Deserialize)]
struct IdOnly {
    id: u64,
}

/// The first request must be `hello` with our protocol. True if it was.
async fn hello<R: AsyncRead + Unpin>(reader: &mut R, replies: &mpsc::Sender<Frame>) -> bool {
    let Ok(Ok(Some(frame))) = tokio::time::timeout(HELLO_WITHIN, proto::read_frame(reader)).await
    else {
        return false;
    };
    let Ok(Request { id, op }) = serde_json::from_slice::<Request>(&frame.header) else {
        return false;
    };
    match op {
        Op::Hello { protocol } if protocol == PROTOCOL => {
            let hello = Hello {
                protocol: PROTOCOL,
                version: env!("CARGO_PKG_VERSION").into(),
                pid: std::process::id(),
            };
            send(replies, Reply::ok(id, &hello), Vec::new()).await
        }
        Op::Hello { protocol } => {
            let why = format!("this pitcrew-ptyd speaks protocol {PROTOCOL}, not {protocol}");
            let _ = send(
                replies,
                Reply::err(id, FailureKind::Unsupported, why),
                Vec::new(),
            )
            .await;
            false
        }
        _ => false,
    }
}

async fn send(replies: &mpsc::Sender<Frame>, reply: Reply, payload: Vec<u8>) -> bool {
    let frame = Frame::new(&reply, payload).unwrap_or_else(|e| Frame {
        header: serde_json::to_vec(&Reply::err(reply.id, FailureKind::Io, e.to_string()))
            .unwrap_or_default(),
        payload: Vec::new(),
    });
    replies.send(frame).await.is_ok()
}

fn done(id: u64, result: Result<(), Failed>) -> Reply {
    match result {
        Ok(()) => Reply::ok(id, &serde_json::json!({})),
        Err((kind, message)) => Reply::err(id, kind, message),
    }
}

fn answer(id: u64, result: Result<impl serde::Serialize, Failed>) -> Reply {
    match result {
        Ok(value) => Reply::ok(id, &value),
        Err((kind, message)) => Reply::err(id, kind, message),
    }
}

/// Serves one request. Input, resizes and quick questions are answered in order, here; slow
/// requests run on their own. False if the connection is gone.
async fn handle(
    request: Request,
    payload: Vec<u8>,
    state: &Arc<State>,
    replies: &mpsc::Sender<Frame>,
    in_flight: &Arc<Semaphore>,
) -> bool {
    let Request { id, op } = request;
    let terms = &state.terms;
    let reply = match op {
        Op::Write { .. } if payload.len() > MAX_WRITE => Reply::err(
            id,
            FailureKind::Invalid,
            format!("at most {MAX_WRITE} bytes per write"),
        ),
        Op::Write { terminal } => done(id, terms.write(terminal, payload)),
        Op::Keys { terminal, keys } => done(id, terms.keys(terminal, &keys)),
        Op::Resize {
            terminal,
            cols,
            rows,
        } => done(id, terms.resize(terminal, cols, rows)),
        Op::Info { terminal } => answer(id, terms.get(terminal).map(|t| t.describe())),
        Op::List => Reply::ok(id, &terms.list()),
        Op::Hello { .. } => Reply::err(id, FailureKind::Invalid, "hello was said already"),
        Op::Start { .. } | Op::Screen { .. } | Op::Read { .. } | Op::Kill { .. } => {
            let Ok(permit) = Arc::clone(in_flight).try_acquire_owned() else {
                let busy = Reply::err(id, FailureKind::Busy, "too many requests are in progress");
                return send(replies, busy, Vec::new()).await;
            };
            let (state, replies) = (Arc::clone(state), replies.clone());
            tokio::spawn(async move {
                let (reply, payload) = slow(id, op, &state).await;
                let _ = send(&replies, reply, payload).await;
                drop(permit);
            });
            return true;
        }
        _ => Reply::err(id, FailureKind::Unsupported, "an unknown request"),
    };
    send(replies, reply, Vec::new()).await
}

/// A request that may take a while: on a blocking thread, or waiting for output.
async fn slow(id: u64, op: Op, state: &Arc<State>) -> (Reply, Vec<u8>) {
    let terms = Arc::clone(&state.terms);
    let blocking = move |work: Box<dyn FnOnce() -> Reply + Send>| async move {
        tokio::task::spawn_blocking(work)
            .await
            .unwrap_or_else(|_| Reply::err(id, FailureKind::Io, "the request failed inside ptyd"))
    };
    match op {
        Op::Start {
            argv,
            cwd,
            env,
            name,
            cols,
            rows,
        } => {
            let request = StartRequest {
                argv,
                cwd,
                env,
                name,
                cols,
                rows,
            };
            let reply = blocking(Box::new(move || answer(id, terms.start(request)))).await;
            (reply, Vec::new())
        }
        Op::Kill { terminal } => {
            let reply = blocking(Box::new(move || done(id, terms.kill(terminal)))).await;
            (reply, Vec::new())
        }
        Op::Screen { terminal } => match terms.get(terminal) {
            Ok(term) => {
                let reply = blocking(Box::new(move || {
                    Reply::ok(id, &term.shared.screened.screen(&term.id))
                }))
                .await;
                (reply, Vec::new())
            }
            Err((kind, message)) => (Reply::err(id, kind, message), Vec::new()),
        },
        Op::Read {
            terminal,
            from,
            max,
            wait_ms,
        } => match terms.get(terminal) {
            Ok(term) => read(id, &term, from, max, wait_ms).await,
            Err((kind, message)) => (Reply::err(id, kind, message), Vec::new()),
        },
        _ => (
            Reply::err(id, FailureKind::Unsupported, "an unknown request"),
            Vec::new(),
        ),
    }
}

/// Output from `from`; with `wait_ms`, waits while there is none past it and the program runs.
async fn read(id: u64, term: &Term, from: u64, max: u64, wait_ms: u64) -> (Reply, Vec<u8>) {
    let max = usize::try_from(max).unwrap_or(usize::MAX).min(MAX_READ);
    let deadline = tokio::time::Instant::now() + Duration::from_millis(wait_ms.min(MAX_WAIT_MS));
    loop {
        let changed = term.shared.changed.notified();
        tokio::pin!(changed);
        // Registered before looking, so output arriving in between is not missed.
        changed.as_mut().enable();
        let chunk = term.shared.screened.read(from, max);
        let alive = term.shared.alive();
        if chunk.end > from || !alive || tokio::time::Instant::now() >= deadline {
            let head = Chunk {
                offset: chunk.offset,
                end: chunk.end,
                truncated: chunk.truncated,
                alive,
            };
            return (Reply::ok(id, &head), chunk.data);
        }
        tokio::select! {
            () = &mut changed => {}
            () = tokio::time::sleep_until(deadline) => {}
        }
    }
}

/// Refuses a client that is not this user.
#[cfg(unix)]
fn check_peer(stream: &tokio::net::UnixStream) -> Result<(), String> {
    let peer = stream
        .peer_cred()
        .map_err(|e| format!("cannot read its credentials: {e}"))?;
    same_user(peer.uid(), rustix::process::getuid().as_raw())
}

#[cfg(unix)]
fn same_user(peer: u32, me: u32) -> Result<(), String> {
    if peer == me {
        Ok(())
    } else {
        Err(format!("uid {peer} is not this user ({me})"))
    }
}

/// Refuses a client whose process token is not this user's.
#[cfg(windows)]
fn check_peer(pipe: &tokio::net::windows::named_pipe::NamedPipeServer) -> Result<(), String> {
    use pitcrew_runtime::pty::windows::{current_user_sid, pipe_client_pid, process_user_sid};
    let pid = pipe_client_pid(pipe).map_err(|e| format!("cannot tell who it is: {e}"))?;
    let theirs =
        process_user_sid(pid).map_err(|e| format!("cannot read process {pid}'s user: {e}"))?;
    let mine = current_user_sid().map_err(|e| format!("cannot read our own user: {e}"))?;
    if theirs == mine {
        Ok(())
    } else {
        Err(format!("process {pid} runs as {theirs}, not this user"))
    }
}

#[cfg(unix)]
struct Listener(tokio::net::UnixListener);

#[cfg(unix)]
impl Listener {
    /// Binds the socket. The caller holds the lock and has checked the directory, so anything
    /// at the path is a socket of ours left by a ptyd that died.
    fn bind(path: &Path) -> std::io::Result<Self> {
        use std::os::unix::fs::PermissionsExt;
        match std::fs::remove_file(path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
        let listener = tokio::net::UnixListener::bind(path)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        Ok(Self(listener))
    }

    async fn accept(&mut self) -> std::io::Result<tokio::net::UnixStream> {
        self.0.accept().await.map(|(stream, _)| stream)
    }
}

#[cfg(windows)]
struct Listener {
    name: std::ffi::OsString,
    security: pitcrew_runtime::pty::windows::PipeSecurity,
    next: tokio::net::windows::named_pipe::NamedPipeServer,
}

#[cfg(windows)]
impl Listener {
    fn options(first: bool) -> tokio::net::windows::named_pipe::ServerOptions {
        let mut options = tokio::net::windows::named_pipe::ServerOptions::new();
        options
            .first_pipe_instance(first)
            .reject_remote_clients(true)
            .access_inbound(true)
            .access_outbound(true);
        options
    }

    /// Creates the pipe's first instance: `PermissionDenied` if the name is taken.
    fn bind(name: &Path) -> std::io::Result<Self> {
        let security = pitcrew_runtime::pty::windows::PipeSecurity::current_user_only()?;
        let name = name.as_os_str().to_owned();
        let shown = name.to_string_lossy().into_owned();
        let next = security.create(&Self::options(true), &shown)?;
        Ok(Self {
            name,
            security,
            next,
        })
    }

    /// The next connected instance; a fresh one listens in its place.
    async fn accept(
        &mut self,
    ) -> std::io::Result<tokio::net::windows::named_pipe::NamedPipeServer> {
        let shown = self.name.to_string_lossy().into_owned();
        let connected = self.next.connect().await;
        let fresh = self.security.create(&Self::options(false), &shown)?;
        let instance = std::mem::replace(&mut self.next, fresh);
        connected.map(|()| instance)
    }
}

#[cfg(unix)]
mod unix {
    use std::ffi::OsString;
    use std::fs::File;
    use std::path::{Path, PathBuf};
    use std::process::{Command, ExitCode, Stdio};

    use rustix::fs::{FlockOperation, Mode, OFlags};

    use crate::Config;

    /// `<endpoint>` with `suffix` added to its file name.
    fn beside(endpoint: &Path, suffix: &str) -> PathBuf {
        let mut name = endpoint.as_os_str().to_owned();
        name.push(suffix);
        PathBuf::from(name)
    }

    /// Opens a file of ours in the private directory, never through a link.
    fn open(path: &Path, truncate: bool) -> std::io::Result<File> {
        let mut flags = OFlags::CREATE | OFlags::WRONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        if truncate {
            flags |= OFlags::TRUNC;
        }
        let fd = rustix::fs::open(path, flags, Mode::from_raw_mode(0o600))?;
        Ok(File::from(fd))
    }

    pub(super) enum LockError {
        Taken,
        Failed(String),
    }

    /// The one-per-endpoint lock, held while ptyd runs.
    pub(super) struct Lock(#[allow(dead_code)] File);

    impl Lock {
        pub(super) fn take(endpoint: &Path) -> Result<Self, LockError> {
            let path = beside(endpoint, ".lock");
            let file = open(&path, false)
                .map_err(|e| LockError::Failed(format!("cannot open {}: {e}", path.display())))?;
            match rustix::fs::flock(&file, FlockOperation::NonBlockingLockExclusive) {
                Ok(()) => Ok(Self(file)),
                Err(rustix::io::Errno::WOULDBLOCK) => Err(LockError::Taken),
                Err(e) => Err(LockError::Failed(format!(
                    "cannot lock {}: {e}",
                    path.display()
                ))),
            }
        }
    }

    /// Starts the real ptyd as a detached copy of this one (its log next to the socket) and
    /// returns, so the process that started this one is not left with a child to reap.
    pub(super) fn detach(config: &Config, args: &[OsString]) -> ExitCode {
        if let Err(why) = pitcrew_runtime::pty::check_endpoint(&config.endpoint) {
            eprintln!("pitcrew-ptyd: {why}");
            return ExitCode::FAILURE;
        }
        let log = beside(&config.endpoint, ".log");
        let stderr = open(&log, true).map_or_else(|_| Stdio::null(), Stdio::from);
        let exe = match std::env::current_exe() {
            Ok(exe) => exe,
            Err(e) => {
                eprintln!("pitcrew-ptyd: cannot find itself: {e}");
                return ExitCode::FAILURE;
            }
        };
        let spawned = Command::new(exe)
            .args(args)
            .arg("--foreground")
            .current_dir("/")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(stderr)
            .spawn();
        match spawned {
            // Not waited for: it is meant to outlive this process.
            Ok(_child) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("pitcrew-ptyd: cannot start: {e}");
                ExitCode::FAILURE
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    #[test]
    fn only_this_users_peers_are_served() {
        use super::*;
        let me = rustix::process::getuid().as_raw();
        assert!(same_user(me, me).is_ok());
        let other = if me == 0 { 1000 } else { 0 };
        let why = same_user(other, me).expect_err("another uid");
        assert!(why.contains("not this user"), "{why}");
        // A real connection from this process passes.
        let (a, _b) = std::os::unix::net::UnixStream::pair().expect("pair");
        a.set_nonblocking(true).expect("nonblocking");
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        rt.block_on(async {
            let a = tokio::net::UnixStream::from_std(a).expect("tokio stream");
            assert!(check_peer(&a).is_ok());
        });
    }
}
