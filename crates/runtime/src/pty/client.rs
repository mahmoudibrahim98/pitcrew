//! One connection to pitcrew-ptyd.
//!
//! A thread of its own runs a small tokio runtime that owns the socket (or pipe): one task reads
//! replies and hands each to the caller waiting for its id, another writes requests. Callers on
//! any thread send a request and wait for its reply with a deadline, so no call waits longer
//! than it may, and a ptyd that stops answering costs a bounded queue, never a stuck caller.
//! (On Windows the pipe needs overlapped I/O for reads and writes to proceed at once, which is
//! what tokio's pipes do.)

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::sync::Notify;

use super::proto::{self, Failure, Frame, Hello, Op, Reply, Request};
use crate::lock;

/// Requests queued for the writer, at most.
const QUEUE: usize = 256;

/// Why a call has no answer.
#[derive(Debug)]
pub(crate) enum CallError {
    /// The connection is closed (or closed while waiting).
    Closed,
    /// Too many requests are queued.
    Busy,
    /// No answer before the deadline. The request may still take effect.
    TimedOut,
    /// ptyd refused it.
    Failed(Failure),
    /// The request could not be encoded (too large).
    Invalid(String),
}

/// Why no connection was made.
#[derive(Debug)]
pub(crate) enum ConnectError {
    /// Nothing listens there: no ptyd runs.
    Absent,
    /// The endpoint is not safe to use, or the other end is not ours.
    Unsafe(String),
    /// No answer before the deadline.
    TimedOut,
    /// ptyd speaks another protocol.
    Protocol(Hello),
    /// Anything else.
    Failed(String),
}

type Answer = Result<(serde_json::Value, Vec<u8>), CallError>;
type Waiters = Arc<Mutex<HashMap<u64, SyncSender<Answer>>>>;

/// A connection to ptyd, past its `hello`.
pub(crate) struct Conn {
    outbox: tokio::sync::mpsc::Sender<Vec<u8>>,
    waiters: Waiters,
    next: AtomicU64,
    open: Arc<AtomicBool>,
    stop: Arc<Notify>,
    thread: Mutex<Option<JoinHandle<()>>>,
    pub(crate) hello: Hello,
}

impl std::fmt::Debug for Conn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Conn")
            .field("hello", &self.hello)
            .field("open", &self.is_open())
            .finish_non_exhaustive()
    }
}

impl Conn {
    /// Connects to the ptyd at `endpoint` and says hello, by `deadline`. If nothing listens
    /// there, tries again until `patience` (a ptyd that was just started), else answers
    /// [`ConnectError::Absent`] at once.
    pub(crate) fn open(
        endpoint: &Path,
        deadline: Instant,
        patience: Option<Instant>,
        expect_uid: Option<u32>,
    ) -> Result<Self, ConnectError> {
        let (outbox, inbox) = tokio::sync::mpsc::channel(QUEUE);
        let waiters: Waiters = Arc::default();
        let open = Arc::new(AtomicBool::new(true));
        let stop = Arc::new(Notify::new());
        let (ready, readied) = mpsc::sync_channel(1);
        let endpoint = endpoint.to_owned();
        let thread = {
            let (waiters, open, stop) =
                (Arc::clone(&waiters), Arc::clone(&open), Arc::clone(&stop));
            std::thread::Builder::new()
                .name("pitcrew-pty-io".into())
                .spawn(move || {
                    let until = Until {
                        deadline,
                        patience,
                        expect_uid,
                    };
                    io_thread(&endpoint, until, inbox, &waiters, &open, &stop, &ready);
                })
                .map_err(|e| ConnectError::Failed(format!("cannot start a thread: {e}")))?
        };
        let left = deadline.saturating_duration_since(Instant::now());
        let hello = match readied.recv_timeout(left) {
            Ok(Ok(hello)) => hello,
            Ok(Err(e)) => {
                let _ = thread.join();
                return Err(e);
            }
            Err(_) => {
                stop.notify_one();
                return Err(ConnectError::TimedOut);
            }
        };
        Ok(Self {
            outbox,
            waiters,
            next: AtomicU64::new(2),
            open,
            stop,
            thread: Mutex::new(Some(thread)),
            hello,
        })
    }

    pub(crate) fn is_open(&self) -> bool {
        self.open.load(Ordering::Acquire)
    }

    /// Sends a request and waits for its answer until `deadline`.
    pub(crate) fn call(&self, op: Op, payload: Vec<u8>, deadline: Instant) -> Answer {
        if !self.is_open() {
            return Err(CallError::Closed);
        }
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let frame = Frame::new(&Request { id, op }, payload)
            .map_err(|e| CallError::Invalid(e.to_string()))?;
        let (answer, answered) = mpsc::sync_channel(1);
        lock(&self.waiters).insert(id, answer);
        let forget = || lock(&self.waiters).remove(&id);
        // The I/O thread marks the connection closed, then drops every waiter: one added
        // after that would wait for nothing.
        if !self.is_open() {
            forget();
            return Err(CallError::Closed);
        }
        match self.outbox.try_send(frame.encode()) {
            Ok(()) => {}
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                forget();
                return Err(CallError::Busy);
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                forget();
                return Err(CallError::Closed);
            }
        }
        let left = deadline.saturating_duration_since(Instant::now());
        match answered.recv_timeout(left) {
            Ok(answer) => answer,
            Err(RecvTimeoutError::Timeout) => {
                forget();
                Err(CallError::TimedOut)
            }
            Err(RecvTimeoutError::Disconnected) => Err(CallError::Closed),
        }
    }

    /// Closes the connection and waits (briefly) for its thread.
    pub(crate) fn close(&self, wait: Duration) {
        self.stop.notify_one();
        let Some(thread) = lock(&self.thread).take() else {
            return;
        };
        let deadline = Instant::now() + wait;
        while !thread.is_finished() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        if thread.is_finished() {
            let _ = thread.join();
        }
    }
}

impl Drop for Conn {
    fn drop(&mut self) {
        self.close(Duration::from_secs(1));
    }
}

/// How long connecting may take.
#[derive(Debug, Clone, Copy)]
struct Until {
    deadline: Instant,
    /// Until when an endpoint nobody listens on is tried again.
    patience: Option<Instant>,
    /// The uid the server must have, if not ours (Unix; a hook for tests).
    expect_uid: Option<u32>,
}

fn io_thread(
    endpoint: &Path,
    until: Until,
    inbox: tokio::sync::mpsc::Receiver<Vec<u8>>,
    waiters: &Waiters,
    open: &AtomicBool,
    stop: &Notify,
    ready: &SyncSender<Result<Hello, ConnectError>>,
) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            open.store(false, Ordering::Release);
            let _ = ready.send(Err(ConnectError::Failed(format!(
                "cannot start an I/O runtime: {e}"
            ))));
            return;
        }
    };
    let deadline = until.deadline;
    runtime.block_on(async {
        let left = deadline.saturating_duration_since(Instant::now());
        // Nobody listening, or a ptyd that closes the connection before answering hello (one
        // that is exiting, or lost the race to serve), is tried again while patience lasts.
        let patient = async {
            loop {
                let attempt = async {
                    let stream = connect(endpoint, until.expect_uid).await?;
                    handshake(stream).await
                };
                match attempt.await {
                    Err(ConnectError::Absent)
                        if until.patience.is_some_and(|p| Instant::now() < p) =>
                    {
                        tokio::time::sleep(Duration::from_millis(20)).await;
                    }
                    done => return done,
                }
            }
        };
        let connected = tokio::select! {
            connected = tokio::time::timeout(left, patient) => connected,
            () = stop.notified() => return,
        };
        let (reader, writer, hello) = match connected {
            Ok(Ok(connected)) => connected,
            Ok(Err(e)) => {
                open.store(false, Ordering::Release);
                let _ = ready.send(Err(e));
                return;
            }
            Err(_) => {
                open.store(false, Ordering::Release);
                let _ = ready.send(Err(ConnectError::TimedOut));
                return;
            }
        };
        if ready.send(Ok(hello)).is_err() {
            // The opener has given up.
            return;
        }
        run(reader, writer, inbox, waiters, stop).await;
    });
    open.store(false, Ordering::Release);
    // Whoever still waits learns the connection is gone.
    lock(waiters).clear();
}

type Halves<S> = (tokio::io::ReadHalf<S>, tokio::io::WriteHalf<S>, Hello);

/// The end of the stream, or a connection reset or broken: the other side went away.
fn gone(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::UnexpectedEof
            | std::io::ErrorKind::ConnectionReset
            | std::io::ErrorKind::ConnectionAborted
            | std::io::ErrorKind::BrokenPipe
            // macOS can report ENOTCONN when a connected peer closes before hello is written.
            | std::io::ErrorKind::NotConnected
    )
}

/// Says hello. A ptyd that goes away before answering counts as [`ConnectError::Absent`].
async fn handshake<S: AsyncRead + AsyncWrite>(stream: S) -> Result<Halves<S>, ConnectError> {
    let (mut reader, mut writer) = tokio::io::split(stream);
    let io = |what: &str, e: std::io::Error| {
        if gone(&e) {
            ConnectError::Absent
        } else {
            ConnectError::Failed(format!("cannot {what} pitcrew-ptyd: {e}"))
        }
    };
    let frame = Frame::new(
        &Request {
            id: 1,
            op: Op::Hello {
                protocol: proto::PROTOCOL,
            },
        },
        Vec::new(),
    )
    .map_err(|e| ConnectError::Failed(e.to_string()))?;
    proto::write_frame(&mut writer, &frame)
        .await
        .map_err(|e| io("write to", e))?;
    let reply = proto::read_frame(&mut reader)
        .await
        .map_err(|e| io("read from", e))?
        .ok_or(ConnectError::Absent)?;
    let reply: Reply = serde_json::from_slice(&reply.header)
        .map_err(|e| ConnectError::Failed(format!("pitcrew-ptyd's hello: {e}")))?;
    let hello = match (reply.ok, reply.err) {
        (Some(ok), None) => {
            let hello: Hello = serde_json::from_value(ok)
                .map_err(|e| ConnectError::Failed(format!("pitcrew-ptyd's hello: {e}")))?;
            if hello.protocol != proto::PROTOCOL {
                return Err(ConnectError::Protocol(hello));
            }
            hello
        }
        (_, Some(failure)) => {
            return Err(ConnectError::Failed(format!(
                "pitcrew-ptyd refused: {}",
                failure.message
            )));
        }
        _ => return Err(ConnectError::Failed("an empty hello".into())),
    };
    Ok((reader, writer, hello))
}

async fn run<S: AsyncRead + AsyncWrite>(
    mut reader: tokio::io::ReadHalf<S>,
    mut writer: tokio::io::WriteHalf<S>,
    mut inbox: tokio::sync::mpsc::Receiver<Vec<u8>>,
    waiters: &Waiters,
    stop: &Notify,
) {
    let read = async {
        // Read errors, a bad frame included, end the connection.
        while let Ok(Some(frame)) = proto::read_frame(&mut reader).await {
            let Ok(reply) = serde_json::from_slice::<Reply>(&frame.header) else {
                break;
            };
            let Some(waiter) = lock(waiters).remove(&reply.id) else {
                // Its caller gave up.
                continue;
            };
            let answer = match (reply.ok, reply.err) {
                (_, Some(failure)) => Err(CallError::Failed(failure)),
                (ok, None) => Ok((ok.unwrap_or(serde_json::Value::Null), frame.payload)),
            };
            let _ = waiter.try_send(answer);
        }
    };
    let write = async {
        while let Some(bytes) = inbox.recv().await {
            if writer.write_all(&bytes).await.is_err() || writer.flush().await.is_err() {
                break;
            }
        }
    };
    tokio::select! {
        () = read => {}
        () = write => {}
        () = stop.notified() => {}
    }
}

/// Connects and checks the server is this user (`expect_uid`, when given, in its place: a hook
/// for tests).
#[cfg(unix)]
async fn connect(
    endpoint: &Path,
    expect_uid: Option<u32>,
) -> Result<tokio::net::UnixStream, ConnectError> {
    crate::tmux::socket::ensure_private(endpoint).map_err(ConnectError::Unsafe)?;
    let mut stream = tokio::net::UnixStream::connect(endpoint)
        .await
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused => {
                ConnectError::Absent
            }
            _ => ConnectError::Failed(format!("cannot connect to {}: {e}", endpoint.display())),
        })?;
    check_peer(&mut stream, endpoint, expect_uid).await?;
    Ok(stream)
}

#[cfg(unix)]
async fn peer_error(stream: &mut tokio::net::UnixStream, error: std::io::Error) -> ConnectError {
    use tokio::io::AsyncReadExt as _;
    // Only the platform's ENOTCONN with confirmed EOF is an exiting peer. A credential
    // failure on a live peer must never turn into permission to start another ptyd.
    if error.raw_os_error() == Some(rustix::io::Errno::NOTCONN.raw_os_error()) {
        let mut byte = [0];
        if matches!(
            tokio::time::timeout(Duration::from_millis(100), stream.read(&mut byte)).await,
            Ok(Ok(0))
        ) {
            return ConnectError::Absent;
        }
    }
    ConnectError::Failed(format!("cannot check pitcrew-ptyd's user: {error}"))
}

#[cfg(unix)]
async fn check_peer(
    stream: &mut tokio::net::UnixStream,
    endpoint: &Path,
    expect_uid: Option<u32>,
) -> Result<(), ConnectError> {
    let peer = match stream.peer_cred() {
        Ok(peer) => peer,
        Err(error) => return Err(peer_error(stream, error).await),
    };
    let me = expect_uid.unwrap_or_else(|| rustix::process::getuid().as_raw());
    if peer.uid() != me {
        return Err(ConnectError::Unsafe(format!(
            "{} is served by uid {}, not this user ({me})",
            endpoint.display(),
            peer.uid()
        )));
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod peer_tests {
    use super::*;

    #[test]
    fn closed_peer_and_wrong_uid_remain_distinct() -> Result<(), Box<dyn std::error::Error>> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        runtime.block_on(async {
            let (mut client, server) = tokio::net::UnixStream::pair()?;
            let wrong = rustix::process::getuid().as_raw() + 1;
            assert!(
                matches!(check_peer(&mut client, Path::new("/synthetic/ptyd"), Some(wrong)).await,
                Err(ConnectError::Unsafe(reason)) if reason.contains("served by uid"))
            );
            let not_connected =
                || std::io::Error::from_raw_os_error(rustix::io::Errno::NOTCONN.raw_os_error());
            assert!(
                matches!(
                    peer_error(&mut client, not_connected()).await,
                    ConnectError::Failed(_)
                ),
                "a live peer with a credential error must be refused"
            );
            drop(server);
            assert!(matches!(
                peer_error(&mut client, not_connected()).await,
                ConnectError::Absent
            ));
            assert!(matches!(
                peer_error(
                    &mut client,
                    std::io::Error::from_raw_os_error(rustix::io::Errno::ACCESS.raw_os_error())
                )
                .await,
                ConnectError::Failed(_)
            ));
            Ok::<_, Box<dyn std::error::Error>>(())
        })
    }
}

/// The pipe must be the current user's, at our integrity level: an elevated ptyd's pipe is
/// labelled high (and refuses medium clients itself), and a ptyd started from an ordinary
/// process must not serve an elevated one, nor the other way round. A pipe without a label
/// counts as medium, as Windows treats it.
#[cfg(windows)]
pub(crate) fn check_pipe(
    pipe: &impl std::os::windows::io::AsHandle,
    endpoint: &Path,
) -> Result<(), ConnectError> {
    use super::windows::{MEDIUM_INTEGRITY, current_identity, label_integrity, owner_sid};
    let failed = |what: &str, e: std::io::Error| ConnectError::Failed(format!("{what}: {e}"));
    let owner = owner_sid(pipe).map_err(|e| failed("cannot check pitcrew-ptyd's pipe", e))?;
    let me = current_identity().map_err(|e| failed("cannot read our own token", e))?;
    if owner != me.user {
        return Err(ConnectError::Unsafe(format!(
            "{} belongs to another user ({owner})",
            endpoint.display()
        )));
    }
    let level = label_integrity(pipe)
        .map_err(|e| failed("cannot read pitcrew-ptyd's pipe's label", e))?
        .unwrap_or(MEDIUM_INTEGRITY);
    if level != me.integrity {
        return Err(ConnectError::Unsafe(format!(
            "{} is served at integrity level {level:#x}, and this process runs at {:#x} (an \
             elevated pitcrew-ptyd serves only elevated clients, and the other way round)",
            endpoint.display(),
            me.integrity
        )));
    }
    Ok(())
}

#[cfg(windows)]
async fn connect(
    endpoint: &Path,
    _expect_uid: Option<u32>,
) -> Result<tokio::net::windows::named_pipe::NamedPipeClient, ConnectError> {
    use tokio::net::windows::named_pipe::ClientOptions;
    use windows_sys::Win32::Foundation::ERROR_PIPE_BUSY;
    use windows_sys::Win32::Storage::FileSystem::{SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT};

    super::check_endpoint(endpoint).map_err(ConnectError::Unsafe)?;
    loop {
        // The server may only identify us, never act as us, in case the name was taken by
        // someone else.
        let opened = ClientOptions::new()
            .security_qos_flags(SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION)
            .open(endpoint.as_os_str());
        match opened {
            Ok(client) => {
                check_pipe(&client, endpoint)?;
                return Ok(client);
            }
            // Every instance is busy: one is made for each connection, so wait a moment.
            Err(e) if e.raw_os_error() == i32::try_from(ERROR_PIPE_BUSY).ok() => {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(ConnectError::Absent),
            Err(e) => {
                return Err(ConnectError::Failed(format!(
                    "cannot open {}: {e}",
                    endpoint.display()
                )));
            }
        }
    }
}

#[cfg(test)]
mod handshake_tests {
    use super::*;
    use std::io::{Error, ErrorKind};
    use std::pin::Pin;
    use std::task::{Context, Poll};
    use tokio::io::ReadBuf;

    struct FailedHello {
        kind: ErrorKind,
        on_read: bool,
    }

    impl AsyncRead for FailedHello {
        fn poll_read(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
            _: &mut ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            Poll::Ready(Err(Error::from(self.kind)))
        }
    }

    impl AsyncWrite for FailedHello {
        fn poll_write(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
            bytes: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            Poll::Ready(if self.on_read {
                Ok(bytes.len())
            } else {
                Err(Error::from(self.kind))
            })
        }

        fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }

        fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    #[test]
    fn hello_disconnects_count_as_absent_on_read_and_write() -> Result<(), std::io::Error> {
        let runtime = tokio::runtime::Builder::new_current_thread().build()?;
        for kind in [
            ErrorKind::UnexpectedEof,
            ErrorKind::ConnectionReset,
            ErrorKind::ConnectionAborted,
            ErrorKind::BrokenPipe,
            ErrorKind::NotConnected,
            ErrorKind::PermissionDenied,
            ErrorKind::InvalidData,
        ] {
            for on_read in [false, true] {
                let result = runtime.block_on(handshake(FailedHello { kind, on_read }));
                if matches!(kind, ErrorKind::PermissionDenied | ErrorKind::InvalidData) {
                    assert!(
                        matches!(result, Err(ConnectError::Failed(_))),
                        "{kind:?}, read={on_read}"
                    );
                } else {
                    assert!(
                        matches!(result, Err(ConnectError::Absent)),
                        "{kind:?}, read={on_read}"
                    );
                }
            }
        }
        Ok(())
    }
}
