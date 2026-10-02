//! Where ptyd listens, who may connect, and how requests are served.
//!
//! - **One per user and endpoint.** On Unix a lock file next to the socket (`flock`), taken
//!   before the socket is bound; on Windows the pipe's first instance
//!   (`FILE_FLAG_FIRST_PIPE_INSTANCE`). A second ptyd exits with status 3.
//! - **Who may connect.** On Unix the socket's directory is private (0700, checked before it is
//!   used) and every client's uid (`SO_PEERCRED`) must be ours, before anything is read. On
//!   Windows the pipe's DACL grants the current user alone, its mandatory label refuses
//!   processes below ptyd's integrity level, remote clients are refused
//!   (`PIPE_REJECT_REMOTE_CLIENTS`), the user of the client's process must be ours before
//!   anything is read, and after the hello the client's own token (read by impersonating it at
//!   the identification level it allows) must be our user at our integrity level: an elevated
//!   ptyd serves only elevated clients, an ordinary one only ordinary ones.
//! - **Bounded.**
//!   - At most 32 clients; a client says hello within 10 seconds.
//!   - At most 16 slow requests (start, screen, kill) of a client, and 64 of everyone's, are
//!     in progress until their replies are written.
//!   - Reads that wait for output have their own budget (256 per client, 1024 in all), since
//!     waiting holds no data: a daemon can tail every terminal at once.
//!   - Reply payloads queued for a client are at most 8 MiB (64 MiB for all clients); a reply
//!     waits for room, so a client that stops reading stops being read.
//!   - A reply not written within 30 seconds ends the connection, and so does a client that
//!     stopped sending and has not taken its last replies 15 seconds later.
//! - **Idle exit.** With no terminal running, no client connected, and no ended terminal that
//!   no client has been told about (kept for at most 10 minutes), for a while (30 seconds by
//!   default), ptyd exits, removing its socket.

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
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc};

use crate::terms::{Failed, StartRequest, Term, Terms};
use crate::{ALREADY_RUNNING, Config, log};

/// How long ptyd stays with no terminals and no clients, by default.
pub(crate) const IDLE_EXIT: Duration = Duration::from_secs(30);
/// Clients at once, at most.
const MAX_CLIENTS: usize = 32;
/// A client must say hello within this.
const HELLO_WITHIN: Duration = Duration::from_secs(10);
/// Slow requests (start, screen, kill) of one client in progress at once, at most; one is in
/// progress until its reply is written.
const IN_FLIGHT: usize = 16;
/// Slow requests of all clients in progress at once, at most.
const ALL_IN_FLIGHT: usize = 64;
/// Reads waiting for output, per client and in all.
const WAITING: usize = 256;
const ALL_WAITING: usize = 1024;
/// Reply payload queued for one client, and for all, in KiB: a reply waits for room.
const REPLY_KIB: usize = 8 << 10;
const ALL_REPLY_KIB: usize = 64 << 10;
/// Replies queued for one client.
const REPLIES: usize = 64;
/// A reply must be written within this, or the connection ends.
const WRITE_WITHIN: Duration = Duration::from_secs(30);
/// A client that stopped sending has this long to take its last replies.
const DRAIN_WITHIN: Duration = Duration::from_secs(MAX_WAIT_MS / 1000 + 5);

struct State {
    terms: Arc<Terms>,
    clients: AtomicUsize,
    /// Budgets shared by all clients.
    slow: Arc<Semaphore>,
    waiting: Arc<Semaphore>,
    reply_kib: Arc<Semaphore>,
    /// The uid clients must have (this process's, unless a debug build was told otherwise).
    #[cfg(unix)]
    uid: u32,
    /// Who clients must be.
    #[cfg(windows)]
    me: pitcrew_runtime::pty::windows::Identity,
}

/// One client's budgets.
struct Budgets {
    slow: Arc<Semaphore>,
    waiting: Arc<Semaphore>,
    reply_kib: Arc<Semaphore>,
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
    // The log is ours now: start it afresh (it is appended to, so a ptyd that lost the race
    // above left the running one's lines alone).
    #[cfg(unix)]
    unix::truncate_log();
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
    #[cfg(windows)]
    let me = match pitcrew_runtime::pty::windows::current_identity() {
        Ok(me) => me,
        Err(e) => {
            log!("cannot read our own token: {e}");
            return ExitCode::FAILURE;
        }
    };
    let state = Arc::new(State {
        terms: Terms::new(config.history),
        clients: AtomicUsize::new(0),
        slow: Arc::new(Semaphore::new(ALL_IN_FLIGHT)),
        waiting: Arc::new(Semaphore::new(ALL_WAITING)),
        reply_kib: Arc::new(Semaphore::new(ALL_REPLY_KIB)),
        #[cfg(unix)]
        uid: config
            .expect_uid
            .unwrap_or_else(|| rustix::process::getuid().as_raw()),
        #[cfg(windows)]
        me,
    });
    let served = runtime.block_on(async {
        // A `SIGCHLD` ignored by whoever started us would make programs vanish without a
        // trace (no exit status, and nothing to hold their pid during a kill). A handler,
        // unlike `SIG_IGN`, keeps ended children waitable; tokio installs one safely.
        #[cfg(unix)]
        let _children = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::child())?;
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
                if let Err(why) = check_peer(&stream, state) {
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
                let busy = state.clients.load(Ordering::Acquire) > 0
                    || state.terms.live() > 0
                    || state.terms.unseen_endings() > 0;
                if busy {
                    quiet_since = None;
                } else if quiet_since.get_or_insert_with(Instant::now).elapsed() >= idle {
                    return;
                }
            }
        }
    }
}

/// A reply on its way to a client, with the permits it holds until it is written: a slow
/// request's, and room for its payload.
struct Outgoing {
    frame: Frame,
    _permits: Vec<OwnedSemaphorePermit>,
}

type Replies = mpsc::Sender<Outgoing>;

/// Serves one client until it disconnects.
async fn client<S>(mut stream: S, state: &Arc<State>)
where
    S: AsyncRead + AsyncWrite + Send + Unpin + 'static,
    State: CheckAfterHello<S>,
{
    // The hello is read before the stream is split: on Windows the client's token can be read
    // only once it has sent something, and through the pipe itself.
    let Ok(Ok(Some(hello_frame))) =
        tokio::time::timeout(HELLO_WITHIN, proto::read_frame(&mut stream)).await
    else {
        return;
    };
    let identity = state.check_after_hello(&stream);
    let (mut reader, mut writer) = tokio::io::split(stream);
    let (replies, mut outgoing) = mpsc::channel::<Outgoing>(REPLIES);
    let mut writing = tokio::spawn(async move {
        while let Some(reply) = outgoing.recv().await {
            let written =
                tokio::time::timeout(WRITE_WITHIN, proto::write_frame(&mut writer, &reply.frame))
                    .await;
            if !matches!(written, Ok(Ok(()))) {
                break;
            }
        }
    });
    if hello(&hello_frame, identity, &replies).await {
        let budgets = Budgets {
            slow: Arc::new(Semaphore::new(IN_FLIGHT)),
            waiting: Arc::new(Semaphore::new(WAITING)),
            reply_kib: Arc::new(Semaphore::new(REPLY_KIB)),
        };
        // A read error or a frame over the limits ends the connection.
        while let Ok(Some(frame)) = proto::read_frame(&mut reader).await {
            let request = match serde_json::from_slice::<Request>(&frame.header) {
                Ok(request) => request,
                Err(e) => match serde_json::from_slice::<IdOnly>(&frame.header) {
                    Ok(IdOnly { id }) => {
                        let reply = Reply::err(id, FailureKind::Invalid, format!("{e}"));
                        if !send(&replies, reply, Vec::new(), Vec::new()).await {
                            break;
                        }
                        continue;
                    }
                    Err(_) => break,
                },
            };
            if !handle(request, frame.payload, state, &replies, &budgets).await {
                break;
            }
        }
    }
    drop(replies);
    // The client has stopped sending; it gets a little while for its last replies.
    if tokio::time::timeout(DRAIN_WITHIN, &mut writing)
        .await
        .is_err()
    {
        writing.abort();
    }
}

/// What is checked once a client's hello has been read.
trait CheckAfterHello<S> {
    fn check_after_hello(&self, stream: &S) -> Result<(), String>;
}

#[cfg(unix)]
impl<S> CheckAfterHello<S> for State {
    /// Nothing more: the peer's uid was checked before anything was read.
    fn check_after_hello(&self, _: &S) -> Result<(), String> {
        Ok(())
    }
}

#[cfg(windows)]
impl CheckAfterHello<tokio::net::windows::named_pipe::NamedPipeServer> for State {
    /// The client's own token: our user, at our integrity level.
    fn check_after_hello(
        &self,
        pipe: &tokio::net::windows::named_pipe::NamedPipeServer,
    ) -> Result<(), String> {
        let theirs = pitcrew_runtime::pty::windows::pipe_client_identity(pipe)
            .map_err(|e| format!("cannot read the client's token: {e}"))?;
        same_identity(&theirs, &self.me)
    }
}

/// A client must be our user at our integrity level.
#[cfg(windows)]
fn same_identity(
    theirs: &pitcrew_runtime::pty::windows::Identity,
    mine: &pitcrew_runtime::pty::windows::Identity,
) -> Result<(), String> {
    if theirs.user != mine.user {
        Err(format!("the client runs as {}, not this user", theirs.user))
    } else if theirs.integrity != mine.integrity {
        Err(format!(
            "the client runs at integrity level {:#x}, and this pitcrew-ptyd at {:#x}: an \
             elevated pitcrew-ptyd serves only elevated clients, and the other way round",
            theirs.integrity, mine.integrity
        ))
    } else {
        Ok(())
    }
}

#[derive(serde::Deserialize)]
struct IdOnly {
    id: u64,
}

/// The first request must be `hello` with our protocol, from a client that passed the checks
/// made after it. True if it was.
async fn hello(frame: &Frame, identity: Result<(), String>, replies: &Replies) -> bool {
    let Ok(Request { id, op }) = serde_json::from_slice::<Request>(&frame.header) else {
        return false;
    };
    let Op::Hello { protocol } = op else {
        return false;
    };
    let refusal = if let Err(why) = identity {
        log!("refused a client: {why}");
        Some(why)
    } else if protocol != PROTOCOL {
        Some(format!(
            "this pitcrew-ptyd speaks protocol {PROTOCOL}, not {protocol}"
        ))
    } else {
        None
    };
    if let Some(why) = refusal {
        let reply = Reply::err(id, FailureKind::Unsupported, why);
        let _ = send(replies, reply, Vec::new(), Vec::new()).await;
        return false;
    }
    let hello = Hello {
        protocol: PROTOCOL,
        version: env!("CARGO_PKG_VERSION").into(),
        pid: std::process::id(),
    };
    send(replies, Reply::ok(id, &hello), Vec::new(), Vec::new()).await
}

/// Queues a reply with the permits it holds until written. False if the connection is gone.
async fn send(
    replies: &Replies,
    reply: Reply,
    payload: Vec<u8>,
    permits: Vec<OwnedSemaphorePermit>,
) -> bool {
    let frame = Frame::new(&reply, payload).unwrap_or_else(|e| Frame {
        header: serde_json::to_vec(&Reply::err(reply.id, FailureKind::Io, e.to_string()))
            .unwrap_or_default(),
        payload: Vec::new(),
    });
    replies
        .send(Outgoing {
            frame,
            _permits: permits,
        })
        .await
        .is_ok()
}

/// Room for a payload of `bytes` in the client's and everyone's reply budgets, waited for.
async fn room(
    bytes: usize,
    mine: &Arc<Semaphore>,
    all: &Arc<Semaphore>,
) -> Vec<OwnedSemaphorePermit> {
    let kib = u32::try_from(bytes.div_ceil(1024)).unwrap_or(u32::MAX);
    if kib == 0 {
        return Vec::new();
    }
    let mut permits = Vec::with_capacity(2);
    // The semaphores are never closed, so these only fail if they were.
    if let Ok(permit) = Arc::clone(mine).acquire_many_owned(kib).await {
        permits.push(permit);
    }
    if let Ok(permit) = Arc::clone(all).acquire_many_owned(kib).await {
        permits.push(permit);
    }
    permits
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

/// Both permits of a budget (the client's, then everyone's), or `None` if either is spent.
fn take(mine: &Arc<Semaphore>, all: &Arc<Semaphore>) -> Option<Vec<OwnedSemaphorePermit>> {
    let mine = Arc::clone(mine).try_acquire_owned().ok()?;
    let all = Arc::clone(all).try_acquire_owned().ok()?;
    Some(vec![mine, all])
}

/// Serves one request. Input, resizes, quick questions and reads that do not wait are answered
/// in order, here; slow requests and waiting reads run on their own. False if the connection
/// is gone.
async fn handle(
    request: Request,
    payload: Vec<u8>,
    state: &Arc<State>,
    replies: &Replies,
    budgets: &Budgets,
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
        Op::Info { terminal } => answer(id, terms.get(terminal).map(|t| t.describe_seen())),
        Op::List => Reply::ok(id, &terms.list()),
        Op::Hello { .. } => Reply::err(id, FailureKind::Invalid, "hello was said already"),
        Op::Read {
            terminal,
            from,
            max,
            wait_ms: 0,
        } => {
            let (reply, data) = match terms.get(terminal) {
                Ok(term) => read_now(id, &term, from, max),
                Err((kind, message)) => (Reply::err(id, kind, message), Vec::new()),
            };
            let permits = room(data.len(), &budgets.reply_kib, &state.reply_kib).await;
            return send(replies, reply, data, permits).await;
        }
        Op::Read {
            terminal,
            from,
            max,
            wait_ms,
        } => {
            let Some(waiting) = take(&budgets.waiting, &state.waiting) else {
                let busy = Reply::err(id, FailureKind::Busy, "too many reads are waiting");
                return send(replies, busy, Vec::new(), Vec::new()).await;
            };
            let term = match terms.get(terminal) {
                Ok(term) => term,
                Err((kind, message)) => {
                    let reply = Reply::err(id, kind, message);
                    return send(replies, reply, Vec::new(), Vec::new()).await;
                }
            };
            let (state, replies) = (Arc::clone(state), replies.clone());
            let mine = Arc::clone(&budgets.reply_kib);
            tokio::spawn(async move {
                let (reply, data) = read_waiting(id, &term, from, max, wait_ms).await;
                // Done waiting: the payload takes reply room instead.
                drop(waiting);
                let permits = room(data.len(), &mine, &state.reply_kib).await;
                let _ = send(&replies, reply, data, permits).await;
            });
            return true;
        }
        Op::Start { .. } | Op::Screen { .. } | Op::Kill { .. } => {
            let Some(permits) = take(&budgets.slow, &state.slow) else {
                let busy = Reply::err(id, FailureKind::Busy, "too many requests are in progress");
                return send(replies, busy, Vec::new(), Vec::new()).await;
            };
            let (state, replies) = (Arc::clone(state), replies.clone());
            tokio::spawn(async move {
                let reply = slow(id, op, &state).await;
                let _ = send(&replies, reply, Vec::new(), permits).await;
            });
            return true;
        }
        _ => Reply::err(id, FailureKind::Unsupported, "an unknown request"),
    };
    send(replies, reply, Vec::new(), Vec::new()).await
}

/// A request that may take a while, on a blocking thread.
async fn slow(id: u64, op: Op, state: &Arc<State>) -> Reply {
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
            blocking(Box::new(move || answer(id, terms.start(request)))).await
        }
        Op::Kill { terminal } => blocking(Box::new(move || done(id, terms.kill(terminal)))).await,
        Op::Screen { terminal } => match terms.get(terminal) {
            Ok(term) => {
                blocking(Box::new(move || {
                    Reply::ok(id, &term.shared.screened.screen(&term.id))
                }))
                .await
            }
            Err((kind, message)) => Reply::err(id, kind, message),
        },
        _ => Reply::err(id, FailureKind::Unsupported, "an unknown request"),
    }
}

/// Output from `from`, now.
fn read_now(id: u64, term: &Term, from: u64, max: u64) -> (Reply, Vec<u8>) {
    let max = usize::try_from(max).unwrap_or(usize::MAX).min(MAX_READ);
    let chunk = term.shared.screened.read(from, max);
    let alive = term.shared.alive();
    if !alive {
        term.seen();
    }
    let head = Chunk {
        offset: chunk.offset,
        end: chunk.end,
        truncated: chunk.truncated,
        alive,
    };
    (Reply::ok(id, &head), chunk.data)
}

/// Output from `from`, waiting up to `wait_ms` while there is none past it and the program
/// runs.
async fn read_waiting(id: u64, term: &Term, from: u64, max: u64, wait_ms: u64) -> (Reply, Vec<u8>) {
    let deadline = tokio::time::Instant::now() + Duration::from_millis(wait_ms.min(MAX_WAIT_MS));
    loop {
        let changed = term.shared.changed.notified();
        tokio::pin!(changed);
        // Registered before looking, so output arriving in between is not missed.
        changed.as_mut().enable();
        let past = term.shared.screened.end() > from;
        if past || !term.shared.alive() || tokio::time::Instant::now() >= deadline {
            return read_now(id, term, from, max);
        }
        tokio::select! {
            () = &mut changed => {}
            () = tokio::time::sleep_until(deadline) => {}
        }
    }
}

/// Refuses a client that is not this user, before anything is read from it.
#[cfg(unix)]
fn check_peer(stream: &tokio::net::UnixStream, state: &State) -> Result<(), String> {
    let peer = stream
        .peer_cred()
        .map_err(|e| format!("cannot read its credentials: {e}"))?;
    same_user(peer.uid(), state.uid)
}

#[cfg(unix)]
fn same_user(peer: u32, me: u32) -> Result<(), String> {
    if peer == me {
        Ok(())
    } else {
        Err(format!("uid {peer} is not this user ({me})"))
    }
}

/// Refuses a client whose process is not this user's, before anything is read from it. (The
/// client's own token, which no pid can stand in for, is checked after its hello.)
#[cfg(windows)]
fn check_peer(
    pipe: &tokio::net::windows::named_pipe::NamedPipeServer,
    state: &State,
) -> Result<(), String> {
    use pitcrew_runtime::pty::windows::{pipe_client_pid, process_user_sid};
    let pid = pipe_client_pid(pipe).map_err(|e| format!("cannot tell who it is: {e}"))?;
    let theirs =
        process_user_sid(pid).map_err(|e| format!("cannot read process {pid}'s user: {e}"))?;
    if theirs == state.me.user {
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
    /// Every instance: local clients only, both directions.
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
    use std::os::fd::AsFd;
    use std::path::{Path, PathBuf};
    use std::process::{Command, ExitCode, Stdio};

    use rustix::fs::{FileType, FlockOperation, Mode, OFlags};

    use crate::Config;

    /// `<endpoint>` with `suffix` added to its file name.
    fn beside(endpoint: &Path, suffix: &str) -> PathBuf {
        let mut name = endpoint.as_os_str().to_owned();
        name.push(suffix);
        PathBuf::from(name)
    }

    /// Opens a file of ours in the private directory, never through a link; the log is opened
    /// for appending.
    fn open(path: &Path, append: bool) -> std::io::Result<File> {
        let mut flags = OFlags::CREATE | OFlags::WRONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        if append {
            flags |= OFlags::APPEND;
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

    /// Empties our log, if standard error is one (a regular file), once the lock is ours.
    pub(super) fn truncate_log() {
        let stderr = std::io::stderr();
        let fd = stderr.as_fd();
        if rustix::fs::fstat(fd).is_ok_and(|stat| FileType::from_raw_mode(stat.st_mode).is_file()) {
            let _ = rustix::fs::ftruncate(fd, 0);
        }
    }

    /// Starts the real ptyd as a detached copy of this one (its log, appended to, next to the
    /// socket) and returns, so the process that started this one is not left with a child to
    /// reap.
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
        let why = same_user(me + 1, me).expect_err("another uid");
        assert!(why.contains("not this user"), "{why}");
    }

    /// The listener's own options and descriptor: our user alone, at our integrity level, and
    /// no remote client (opening the pipe through the network redirector is refused).
    #[cfg(windows)]
    #[test]
    fn the_listener_is_ours_local_and_at_our_level() {
        use super::*;
        use pitcrew_runtime::pty::windows::{
            Ace, Dacl, Identity, PipeSecurity, current_identity, dacl, label_integrity, owner_sid,
        };
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        rt.block_on(async {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos());
            let short = format!("pitcrew-ptyd-unit-listener-{}-{nanos}", std::process::id());
            let name = format!(r"\\.\pipe\{short}");
            let listener = Listener::bind(Path::new(&name)).expect("bind");
            let me = current_identity().expect("identity");
            assert_eq!(
                dacl(&listener.next).expect("dacl"),
                Dacl {
                    protected: true,
                    entries: vec![Ace::Allow(me.user.clone())],
                }
            );
            assert_eq!(owner_sid(&listener.next).expect("owner"), me.user);
            assert_eq!(
                label_integrity(&listener.next).expect("label"),
                Some(me.integrity)
            );
            // A second ptyd cannot take the name.
            assert!(Listener::bind(Path::new(&name)).is_err());
            // Remote clients, compared with a control: two pipes of ours labelled low (so the
            // label stops nobody), one made without the flag, one with the listener's options.
            // Through the network redirector (as a remote client comes), the control must be
            // reachable for the check to mean anything, and the listener's must not.
            let open_remotely = |short: &str| {
                tokio::net::windows::named_pipe::ClientOptions::new()
                    .open(format!(r"\\127.0.0.1\pipe\{short}"))
            };
            let low = PipeSecurity::for_identity(&Identity {
                user: me.user.clone(),
                integrity: pitcrew_runtime::pty::windows::LOW_INTEGRITY,
            })
            .expect("descriptor");
            let control = format!("{short}-control");
            let mut plain = tokio::net::windows::named_pipe::ServerOptions::new();
            plain.first_pipe_instance(true);
            let _control = low
                .create(&plain, &format!(r"\\.\pipe\{control}"))
                .expect("control pipe");
            match open_remotely(&control) {
                Err(e) => eprintln!(
                    "skipped the remote check: the control pipe cannot be reached through the \
                     network redirector here: {e}"
                ),
                Ok(_) => {
                    let guarded = format!("{short}-guarded");
                    let _guarded = low
                        .create(&Listener::options(true), &format!(r"\\.\pipe\{guarded}"))
                        .expect("guarded pipe");
                    assert!(
                        open_remotely(&guarded).is_err(),
                        "a remote client was let in"
                    );
                }
            }
            // And a client of another integrity level is refused after its hello.
            let other = Identity {
                user: me.user.clone(),
                integrity: me.integrity + 0x1000,
            };
            let why = same_identity(&other, &me).expect_err("another level");
            assert!(why.contains("integrity"), "{why}");
            let stranger = Identity {
                user: "S-1-5-21-1-2-3-4".into(),
                integrity: me.integrity,
            };
            assert!(same_identity(&stranger, &me).is_err());
            assert!(same_identity(&me, &me).is_ok());
        });
    }
}
