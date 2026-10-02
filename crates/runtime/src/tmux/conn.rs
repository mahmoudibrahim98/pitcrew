//! One `tmux -C` control client. Commands go to its stdin without forking; its stdout is parsed
//! on a reader thread, replies are matched to commands in the order they were sent (tmux answers
//! one client's commands in order, each line with exactly one `%begin`/`%end` block), and every
//! other notification goes to a [`Sink`].
//!
//! Nothing blocks a caller for longer than its deadline: a writer thread owns stdin, so a tmux
//! that stops reading fills a bounded queue (then `Busy`) instead of blocking the caller.

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command as Process, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use super::TmuxOptions;
use crate::command::Command;
use crate::control::{CommandReply, ControlParser, Notification};

/// Bytes of commands that may wait for tmux to read them.
const MAX_QUEUED: usize = 16 << 20;
/// One read from tmux's stdout.
const READ_CHUNK: usize = 64 << 10;
/// The most stdin bytes written at once.
const WRITE_BATCH: usize = 256 << 10;
/// stderr kept for error messages.
const STDERR_KEPT: usize = 4096;

/// Receives the notifications of one connection, on its reader thread.
pub(crate) trait Sink: Send + Sync {
    /// Any notification but a command reply. `outbox` sends further commands on this connection.
    fn notify(&self, notification: Notification, outbox: &Outbox);
    /// The connection has ended; no more notifications follow.
    fn closed(&self, why: &str);
}

/// Why a call got no reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CallError {
    /// The connection has ended.
    Closed,
    /// Too many commands are waiting for tmux.
    Busy,
    /// No reply before the deadline.
    TimedOut,
}

/// Why a connection could not be opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ConnectError {
    /// No server, or no PitCrew session in it.
    NoSession,
    /// Another client created the session first.
    Duplicate,
    /// tmux did not answer in time.
    TimedOut,
    /// Anything else, with tmux's words.
    Failed(String),
}

/// Commands waiting for their replies, oldest first, and the way to tmux's stdin.
pub(crate) struct Outbox {
    queue: Mutex<Queue>,
    queued: Arc<AtomicUsize>,
}

struct Queue {
    /// `None` for a command whose reply nobody waits for.
    waiters: VecDeque<Option<SyncSender<CommandReply>>>,
    /// `None` once the connection is closed.
    lines: Option<Sender<Vec<u8>>>,
}

impl Outbox {
    /// Sends `commands` in order, returning one receiver per reply.
    pub(crate) fn send(
        &self,
        commands: &[Command],
    ) -> Result<Vec<Receiver<CommandReply>>, CallError> {
        let mut queue = lock(&self.queue);
        let mut replies = Vec::with_capacity(commands.len());
        let mut waiters = Vec::with_capacity(commands.len());
        for _ in commands {
            let (tx, rx) = mpsc::sync_channel(1);
            waiters.push(Some(tx));
            replies.push(rx);
        }
        self.enqueue(&mut queue, commands, waiters)?;
        Ok(replies)
    }

    /// Sends a command whose reply is discarded. False if it could not be queued.
    pub(crate) fn send_and_forget(&self, command: &Command) -> bool {
        let mut queue = lock(&self.queue);
        self.enqueue(&mut queue, std::slice::from_ref(command), vec![None])
            .is_ok()
    }

    fn enqueue(
        &self,
        queue: &mut Queue,
        commands: &[Command],
        waiters: Vec<Option<SyncSender<CommandReply>>>,
    ) -> Result<(), CallError> {
        let Some(lines) = &queue.lines else {
            return Err(CallError::Closed);
        };
        let mut bytes = Vec::new();
        for command in commands {
            bytes.extend_from_slice(command.to_line().as_bytes());
        }
        let size = bytes.len();
        if self.queued.load(Ordering::Acquire).saturating_add(size) > MAX_QUEUED {
            return Err(CallError::Busy);
        }
        self.queued.fetch_add(size, Ordering::AcqRel);
        if lines.send(bytes).is_err() {
            self.queued.fetch_sub(size, Ordering::AcqRel);
            return Err(CallError::Closed);
        }
        // The queue stays locked, so no reply can be delivered before its waiter is in place.
        queue.waiters.extend(waiters);
        Ok(())
    }

    fn deliver(&self, reply: CommandReply) {
        let waiter = lock(&self.queue).waiters.pop_front();
        match waiter {
            Some(Some(tx)) => {
                let _ = tx.try_send(reply);
            }
            Some(None) => {}
            None => tracing::debug!(number = reply.number, "a tmux reply nobody asked for"),
        }
    }

    fn close(&self) {
        let mut queue = lock(&self.queue);
        queue.lines = None;
        queue.waiters.clear();
    }

    pub(crate) fn is_open(&self) -> bool {
        lock(&self.queue).lines.is_some()
    }
}

/// A running control client.
pub(crate) struct Connection {
    outbox: Arc<Outbox>,
    child: Arc<Mutex<Option<Child>>>,
    pid: u32,
    /// Signalled when the reader thread has finished (and reaped the client).
    done: Mutex<Receiver<()>>,
}

impl Connection {
    /// Starts `tmux -S <socket> -f /dev/null -u -C <args>` and waits for the reply to `args`.
    ///
    /// `args` must be fixed text: tmux splits process arguments ending in `;` into separate
    /// commands. Every user value goes over stdin through [`Command`].
    pub(crate) fn open(
        options: &TmuxOptions,
        args: &[&str],
        sink: Arc<dyn Sink>,
        deadline: Instant,
    ) -> Result<(Self, CommandReply), ConnectError> {
        let mut process = Process::new(&options.tmux);
        process
            .arg("-S")
            .arg(&options.socket)
            .args(["-f", "/dev/null", "-u", "-C"])
            .args(args)
            // A server started by this client keeps its working directory.
            .current_dir("/")
            .env_remove("TMUX")
            .env_remove("TMUX_PANE")
            .envs(options.env.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = process
            .spawn()
            .map_err(|e| ConnectError::Failed(format!("cannot run {:?}: {e}", options.tmux)))?;
        let pid = child.id();
        let (stdin, stdout, stderr) =
            match (child.stdin.take(), child.stdout.take(), child.stderr.take()) {
                (Some(i), Some(o), Some(e)) => (i, o, e),
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(ConnectError::Failed("tmux pipes are missing".into()));
                }
            };
        let child = Arc::new(Mutex::new(Some(child)));
        let queued = Arc::new(AtomicUsize::new(0));
        let (lines_tx, lines_rx) = mpsc::channel();
        let (first_tx, first_rx) = mpsc::sync_channel(1);
        let outbox = Arc::new(Outbox {
            queue: Mutex::new(Queue {
                // The reply to the command on the command line comes first.
                waiters: VecDeque::from([Some(first_tx)]),
                lines: Some(lines_tx),
            }),
            queued: Arc::clone(&queued),
        });
        let (done_tx, done_rx) = mpsc::sync_channel(1);
        let stderr_text = Arc::new(Mutex::new(Vec::new()));
        let (stderr_done_tx, stderr_done_rx) = mpsc::sync_channel(1);
        let spawned = spawn_threads(Threads {
            stdin,
            stdout,
            stderr,
            lines: lines_rx,
            queued,
            outbox: Arc::clone(&outbox),
            sink,
            child: Arc::clone(&child),
            done: done_tx,
            stderr_text: Arc::clone(&stderr_text),
            stderr_done: stderr_done_tx,
        });
        let connection = Self {
            outbox,
            child,
            pid,
            done: Mutex::new(done_rx),
        };
        if let Err(e) = spawned {
            connection.kill();
            return Err(ConnectError::Failed(format!("cannot start a thread: {e}")));
        }
        let left = deadline.saturating_duration_since(Instant::now());
        match first_rx.recv_timeout(left) {
            Ok(reply) if !reply.failed => Ok((connection, reply)),
            Ok(reply) => {
                connection.kill();
                Err(classify(&reply_text(&reply)))
            }
            Err(RecvTimeoutError::Timeout) => {
                connection.kill();
                Err(ConnectError::TimedOut)
            }
            Err(RecvTimeoutError::Disconnected) => {
                // tmux ended before answering: the reason is on stderr.
                let _ = stderr_done_rx.recv_timeout(Duration::from_secs(1));
                let text = String::from_utf8_lossy(&lock(&stderr_text))
                    .trim()
                    .to_owned();
                Err(classify(&text))
            }
        }
    }

    pub(crate) fn pid(&self) -> u32 {
        self.pid
    }

    pub(crate) fn outbox(&self) -> &Outbox {
        &self.outbox
    }

    pub(crate) fn is_open(&self) -> bool {
        self.outbox.is_open()
    }

    /// Sends `commands` in order and waits for every reply until `deadline`.
    pub(crate) fn call(
        &self,
        commands: &[Command],
        deadline: Instant,
    ) -> Result<Vec<CommandReply>, CallError> {
        let receivers = self.outbox.send(commands)?;
        receivers
            .into_iter()
            .map(|rx| {
                let left = deadline.saturating_duration_since(Instant::now());
                rx.recv_timeout(left).map_err(|e| match e {
                    RecvTimeoutError::Timeout => CallError::TimedOut,
                    RecvTimeoutError::Disconnected => CallError::Closed,
                })
            })
            .collect()
    }

    /// Detaches: closes the client's stdin, which makes it exit, and kills it if it has not
    /// within `grace`. The server and its windows are untouched.
    pub(crate) fn close(&self, grace: Duration) {
        self.outbox.close();
        if lock(&self.done).recv_timeout(grace).is_err() {
            self.kill();
        }
    }

    /// Kills the client (not the server). Its reader thread reaps it.
    pub(crate) fn kill(&self) {
        self.outbox.close();
        if let Some(child) = lock(&self.child).as_mut() {
            let _ = child.kill();
        }
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.kill();
    }
}

/// tmux's reasons, as it prints them, mapped to what the runtime does next.
fn classify(text: &str) -> ConnectError {
    if text.contains("no sessions")
        || text.contains("can't find session")
        || text.contains("no server running")
        || text.contains("No such file or directory")
        || text.contains("Connection refused")
    {
        ConnectError::NoSession
    } else if text.contains("duplicate session") {
        ConnectError::Duplicate
    } else if text.is_empty() {
        ConnectError::Failed("tmux exited without a reason".into())
    } else {
        ConnectError::Failed(text.to_owned())
    }
}

/// A reply's lines as text, for errors: the first line, at most 200 characters.
pub(crate) fn reply_text(reply: &CommandReply) -> String {
    let first = reply.lines.first().map(Vec::as_slice).unwrap_or_default();
    String::from_utf8_lossy(first).chars().take(200).collect()
}

struct Threads {
    stdin: ChildStdin,
    stdout: ChildStdout,
    stderr: std::process::ChildStderr,
    lines: Receiver<Vec<u8>>,
    queued: Arc<AtomicUsize>,
    outbox: Arc<Outbox>,
    sink: Arc<dyn Sink>,
    child: Arc<Mutex<Option<Child>>>,
    done: SyncSender<()>,
    stderr_text: Arc<Mutex<Vec<u8>>>,
    stderr_done: SyncSender<()>,
}

fn spawn_threads(t: Threads) -> io::Result<()> {
    let Threads {
        stdin,
        stdout,
        stderr,
        lines,
        queued,
        outbox,
        sink,
        child,
        done,
        stderr_text,
        stderr_done,
    } = t;
    let writer_child = Arc::clone(&child);
    thread::Builder::new()
        .name("pitcrew-tmux-write".into())
        .spawn(move || write_loop(stdin, &lines, &queued, &writer_child))?;
    thread::Builder::new()
        .name("pitcrew-tmux-stderr".into())
        .spawn(move || {
            let mut stderr = stderr;
            let mut buf = [0; 1024];
            loop {
                match stderr.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        let mut kept = lock(&stderr_text);
                        let room = STDERR_KEPT.saturating_sub(kept.len());
                        kept.extend_from_slice(&buf[..n.min(room)]);
                    }
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                    Err(_) => break,
                }
            }
            let _ = stderr_done.send(());
        })?;
    thread::Builder::new()
        .name("pitcrew-tmux-read".into())
        .spawn(move || {
            let why = read_loop(stdout, &outbox, &*sink);
            outbox.close();
            let taken = lock(&child).take();
            if let Some(mut child) = taken {
                let _ = child.kill();
                let _ = child.wait();
            }
            sink.closed(&why);
            let _ = done.send(());
        })?;
    Ok(())
}

fn read_loop(mut stdout: ChildStdout, outbox: &Outbox, sink: &dyn Sink) -> String {
    let mut parser = ControlParser::new();
    let mut buf = vec![0; READ_CHUNK];
    loop {
        let n = match stdout.read(&mut buf) {
            Ok(0) => {
                return match parser.finish() {
                    Ok(()) => "tmux closed the control connection".into(),
                    Err(e) => e.to_string(),
                };
            }
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return format!("cannot read from tmux: {e}"),
        };
        match parser.feed(&buf[..n]) {
            Ok(notifications) => {
                for notification in notifications {
                    match notification {
                        Notification::CommandReply(reply) => outbox.deliver(reply),
                        other => sink.notify(other, outbox),
                    }
                }
            }
            Err(e) => return e.to_string(),
        }
    }
}

fn write_loop(
    mut stdin: ChildStdin,
    lines: &Receiver<Vec<u8>>,
    queued: &AtomicUsize,
    child: &Mutex<Option<Child>>,
) {
    while let Ok(mut batch) = lines.recv() {
        while batch.len() < WRITE_BATCH {
            match lines.try_recv() {
                Ok(more) => batch.extend_from_slice(&more),
                Err(_) => break,
            }
        }
        let written = stdin.write_all(&batch).and_then(|()| stdin.flush());
        queued.fetch_sub(batch.len(), Ordering::AcqRel);
        if written.is_err() {
            if let Some(child) = lock(child).as_mut() {
                let _ = child.kill();
            }
            return;
        }
    }
    // Every sender is gone: dropping stdin makes the client detach and exit.
}

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
