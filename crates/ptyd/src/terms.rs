//! The terminals ptyd owns: one PTY each, with threads that read its output, write its input
//! and wait for its program.
//!
//! - **Output** goes into a bounded replay buffer and, only when the screen is read, a screen
//!   model ([`Screened`], shared with the tmux runtime: the clamp filter, its own lock, the work
//!   budget). The reader thread never emulates, so one terminal's flood holds up no other.
//! - **Input** goes through a bounded queue to a writer thread per terminal, so a program that
//!   stops reading its input stalls only that thread; once 4 MiB waits, more is refused.
//! - **Ended programs** stay readable (`alive: false`) until 16 more have ended.
//! - **Kill** ends the program's whole tree. On Unix the program leads its own session and
//!   process group (portable-pty starts it with `setsid`), and its group gets `SIGTERM`, then
//!   `SIGKILL` after half a second; the program is not reaped meanwhile (the waiter thread
//!   waits for it with `WNOWAIT` and reaps it only under the same lock), so its id cannot be
//!   reused while it is signalled. A process that left the group (`setsid`, a daemonizing
//!   program) survives. On Windows the program is put in a Job Object of its own as soon as it
//!   has started, and kill terminates the job; closing the job (when ptyd ends) does too. A
//!   process it starts in the instant before that escapes the job (portable-pty cannot start a
//!   program suspended).

use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::{Duration, Instant};

use pitcrew_protocol::ids::TerminalId;
use pitcrew_protocol::runner::Key;
use pitcrew_runtime::keys::pty_key_in;
use pitcrew_runtime::pty::proto::{FailureKind, Terminal};
use pitcrew_runtime::screen::{MAX_SIZE, Screened};
use portable_pty::{Child, MasterPty, PtySize, native_pty_system};

use crate::log;
use crate::scan::{self, Event, Scanner};
use crate::spawn;

/// Ended terminals kept readable.
const DEAD_KEPT: usize = 16;
/// Running terminals at once, at most.
const MAX_LIVE: usize = 256;
/// Input messages queued per terminal.
const INPUT_QUEUE: usize = 256;
/// Input bytes waiting per terminal, at most.
const MAX_PENDING: usize = 4 << 20;
/// How long `kill` waits after `SIGTERM` before `SIGKILL`.
#[cfg(unix)]
const KILL_GRACE: Duration = Duration::from_millis(500);
/// How long `kill` waits for the program's end to be seen.
const KILL_WAIT: Duration = Duration::from_secs(3);
/// How long a Windows console stays open after its program ends, so its last output is
/// delivered before the console is closed (which ends the output pipe).
#[cfg(windows)]
const CLOSE_AFTER: Duration = Duration::from_millis(300);
/// Bytes read from a PTY at a time.
const READ_CHUNK: usize = 64 << 10;

/// A refusal: its kind and a message for people.
pub(crate) type Failed = (FailureKind, String);

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What to write to a terminal.
enum Input {
    /// Typed input.
    Bytes(Vec<u8>),
    /// A fixed answer to a query.
    Answer(&'static [u8]),
    /// The cursor's position, from the screen model.
    Cursor { private: bool },
}

/// What the threads of one terminal share.
pub(crate) struct Shared {
    pub(crate) screened: Screened,
    /// Output arrived, or the program ended.
    pub(crate) changed: tokio::sync::Notify,
    alive: AtomicBool,
    exit_code: Mutex<Option<i64>>,
    app_cursor: AtomicBool,
    /// Input bytes queued and not yet written.
    pending: AtomicUsize,
    size: Mutex<(u16, u16)>,
}

impl Shared {
    pub(crate) fn alive(&self) -> bool {
        self.alive.load(Ordering::Acquire)
    }
}

/// One terminal.
pub(crate) struct Term {
    pub(crate) id: TerminalId,
    name: String,
    pid: Option<u32>,
    pub(crate) shared: Arc<Shared>,
    input: Mutex<Option<SyncSender<Input>>>,
    master: Arc<Mutex<Option<Box<dyn MasterPty + Send>>>>,
    /// Held while the program is signalled, and while it is reaped.
    #[cfg(unix)]
    reap: Arc<Mutex<()>>,
    /// The program's job; terminated and closed by a kill.
    #[cfg(windows)]
    job: Mutex<Option<pitcrew_runtime::pty::windows::Job>>,
    #[cfg(windows)]
    killer: Mutex<Box<dyn portable_pty::ChildKiller + Send + Sync>>,
}

impl Term {
    pub(crate) fn describe(&self) -> Terminal {
        let (cols, rows) = *lock(&self.shared.size);
        Terminal {
            id: self.id,
            name: self.name.clone(),
            pid: self.pid,
            alive: self.shared.alive(),
            exit_code: *lock(&self.shared.exit_code),
            cols,
            rows,
        }
    }

    fn send(&self, input: Input) -> Result<(), TrySendError<Input>> {
        match lock(&self.input).as_ref() {
            Some(sender) => sender.try_send(input),
            None => Err(TrySendError::Disconnected(input)),
        }
    }
}

struct Registry {
    terms: HashMap<TerminalId, Arc<Term>>,
    dead: VecDeque<TerminalId>,
}

/// Every terminal of this ptyd.
pub(crate) struct Terms {
    registry: Mutex<Registry>,
    history: usize,
    me: Weak<Terms>,
}

/// A checked `start` request.
#[derive(Debug, Clone)]
pub(crate) struct StartRequest {
    pub(crate) argv: Vec<String>,
    pub(crate) cwd: String,
    pub(crate) env: Vec<(String, String)>,
    pub(crate) name: String,
    pub(crate) cols: u16,
    pub(crate) rows: u16,
}

fn not_found(id: TerminalId) -> Failed {
    (FailureKind::NotFound, format!("no such terminal: {id}"))
}

fn exited(id: TerminalId) -> Failed {
    (FailureKind::Exited, format!("terminal {id} has exited"))
}

pub(crate) fn check_size(cols: u16, rows: u16) -> Result<(), Failed> {
    let ok = 1..=MAX_SIZE;
    if ok.contains(&cols) && ok.contains(&rows) {
        Ok(())
    } else {
        Err((
            FailureKind::Invalid,
            format!("a terminal must be 1 to {MAX_SIZE} columns and rows, not {cols}x{rows}"),
        ))
    }
}

impl Terms {
    pub(crate) fn new(history: usize) -> Arc<Self> {
        Arc::new_cyclic(|me| Self {
            registry: Mutex::new(Registry {
                terms: HashMap::new(),
                dead: VecDeque::new(),
            }),
            history,
            me: me.clone(),
        })
    }

    /// Running terminals.
    pub(crate) fn live(&self) -> usize {
        lock(&self.registry)
            .terms
            .values()
            .filter(|t| t.shared.alive())
            .count()
    }

    pub(crate) fn get(&self, id: TerminalId) -> Result<Arc<Term>, Failed> {
        lock(&self.registry)
            .terms
            .get(&id)
            .cloned()
            .ok_or_else(|| not_found(id))
    }

    pub(crate) fn list(&self) -> Vec<Terminal> {
        let terms: Vec<Arc<Term>> = lock(&self.registry).terms.values().cloned().collect();
        let mut all: Vec<Terminal> = terms.iter().map(|t| t.describe()).collect();
        all.sort_by_key(|t| t.id);
        all
    }

    /// A terminal's program has ended: it stays readable until 16 more have.
    fn ended(&self, id: TerminalId) {
        let evicted = {
            let mut registry = lock(&self.registry);
            registry.dead.push_back(id);
            let mut evicted = Vec::new();
            while registry.dead.len() > DEAD_KEPT {
                if let Some(gone) = registry.dead.pop_front()
                    && let Some(term) = registry.terms.remove(&gone)
                {
                    evicted.push(term);
                }
            }
            evicted
        };
        // Dropped outside the lock: closing a PTY can take a moment.
        drop(evicted);
    }

    /// Starts a program in a new terminal. Blocking: call it from a blocking thread.
    pub(crate) fn start(&self, request: StartRequest) -> Result<Terminal, Failed> {
        check_size(request.cols, request.rows)?;
        if self.live() >= MAX_LIVE {
            return Err((
                FailureKind::Busy,
                format!("{MAX_LIVE} terminals are running already"),
            ));
        }
        let spawn_error = |why: String| (FailureKind::Spawn, why);
        let launch = spawn::prepare(&request.argv, &request.cwd, &request.env, &request.name)
            .map_err(spawn_error)?;
        let size = PtySize {
            rows: request.rows,
            cols: request.cols,
            pixel_width: 0,
            pixel_height: 0,
        };
        let pair = native_pty_system()
            .openpty(size)
            .map_err(|e| spawn_error(format!("cannot open a terminal: {e}")))?;
        let program = launch.program.display().to_string();
        let child = pair
            .slave
            .spawn_command(launch.command)
            .map_err(|e| spawn_error(format!("cannot start {program}: {e}")))?;
        // Only the program holds the terminal's other side now, so its end is seen.
        drop(pair.slave);
        let pid = child.process_id();
        let mut started = Started {
            child: Some(child),
            pid,
            #[cfg(windows)]
            job: None,
        };
        #[cfg(windows)]
        {
            started.job = pid.and_then(|pid| {
                let job = pitcrew_runtime::pty::windows::Job::new()
                    .and_then(|job| job.assign_pid(pid).map(|()| job));
                job.map_err(|e| log!("{program}: not in a job, kill ends only it: {e}"))
                    .ok()
            });
        }
        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| spawn_error(format!("cannot read the terminal: {e}")))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| spawn_error(format!("cannot write to the terminal: {e}")))?;
        let id = TerminalId::new();
        let shared = Arc::new(Shared {
            screened: Screened::new(self.history, request.cols, request.rows),
            changed: tokio::sync::Notify::new(),
            alive: AtomicBool::new(true),
            exit_code: Mutex::new(None),
            app_cursor: AtomicBool::new(false),
            pending: AtomicUsize::new(0),
            size: Mutex::new((request.cols, request.rows)),
        });
        let (input, queued) = mpsc::sync_channel(INPUT_QUEUE);
        let mut child = started
            .child
            .take()
            .ok_or_else(|| spawn_error("lost the child".into()))?;
        #[cfg(windows)]
        let killer = child.clone_killer();
        let term = Arc::new(Term {
            id,
            name: launch.name,
            pid,
            shared: Arc::clone(&shared),
            input: Mutex::new(Some(input.clone())),
            master: Arc::new(Mutex::new(Some(pair.master))),
            #[cfg(unix)]
            reap: Arc::new(Mutex::new(())),
            #[cfg(windows)]
            job: Mutex::new(started.job.take()),
            #[cfg(windows)]
            killer: Mutex::new(killer),
        });
        lock(&self.registry).terms.insert(id, Arc::clone(&term));

        let threads = (|| -> std::io::Result<()> {
            let (reading, answers) = (Arc::clone(&shared), input);
            std::thread::Builder::new()
                .name("ptyd-read".into())
                .spawn(move || read_loop(reader, &reading, &answers))?;
            let writing = Arc::clone(&shared);
            std::thread::Builder::new()
                .name("ptyd-write".into())
                .spawn(move || write_loop(writer, &queued, &writing, id))?;
            let terms = self.me.clone();
            let waited = Arc::clone(&term);
            std::thread::Builder::new()
                .name("ptyd-wait".into())
                .spawn(move || {
                    wait_loop(&mut child, &waited);
                    if let Some(terms) = terms.upgrade() {
                        terms.ended(waited.id);
                    }
                })?;
            Ok(())
        })();
        if let Err(e) = threads {
            // Without its threads the terminal cannot work: end it.
            log!("cannot start a terminal's threads: {e}");
            let _ = self.kill(id);
            lock(&self.registry).terms.remove(&id);
            return Err((FailureKind::Io, format!("cannot start a thread: {e}")));
        }
        log!("started {id} ({program}, pid {pid:?})");
        Ok(term.describe())
    }

    /// Types `bytes` into a terminal (in order with other input from any connection).
    pub(crate) fn write(&self, id: TerminalId, bytes: Vec<u8>) -> Result<(), Failed> {
        let term = self.get(id)?;
        if !term.shared.alive() {
            return Err(exited(id));
        }
        if bytes.is_empty() {
            return Ok(());
        }
        let len = bytes.len();
        let before = term.shared.pending.fetch_add(len, Ordering::AcqRel);
        if before + len > MAX_PENDING {
            term.shared.pending.fetch_sub(len, Ordering::AcqRel);
            return Err((
                FailureKind::Busy,
                "too much input is waiting: the program is not reading it".into(),
            ));
        }
        match term.send(Input::Bytes(bytes)) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => {
                term.shared.pending.fetch_sub(len, Ordering::AcqRel);
                Err((FailureKind::Busy, "too much input is waiting".into()))
            }
            Err(TrySendError::Disconnected(_)) => {
                term.shared.pending.fetch_sub(len, Ordering::AcqRel);
                Err(exited(id))
            }
        }
    }

    /// Sends named keys, with arrows the way the program asked for them.
    pub(crate) fn keys(&self, id: TerminalId, keys: &[Key]) -> Result<(), Failed> {
        let term = self.get(id)?;
        let application = term.shared.app_cursor.load(Ordering::Acquire);
        let bytes: Vec<u8> = keys
            .iter()
            .flat_map(|&key| pty_key_in(key, application).iter().copied())
            .collect();
        self.write(id, bytes)
    }

    pub(crate) fn resize(&self, id: TerminalId, cols: u16, rows: u16) -> Result<(), Failed> {
        check_size(cols, rows)?;
        let term = self.get(id)?;
        if !term.shared.alive() {
            return Err(exited(id));
        }
        let size = PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        };
        match lock(&term.master).as_ref() {
            Some(master) => master
                .resize(size)
                .map_err(|e| (FailureKind::Io, format!("cannot resize: {e}")))?,
            None => return Err(exited(id)),
        }
        term.shared.screened.resized(cols, rows);
        *lock(&term.shared.size) = (cols, rows);
        Ok(())
    }

    /// Ends a terminal's program and everything it started, and waits (a few seconds at most)
    /// for its end to be seen. Ending an ended one does nothing. Blocking: call it from a
    /// blocking thread.
    pub(crate) fn kill(&self, id: TerminalId) -> Result<(), Failed> {
        let term = self.get(id)?;
        if !term.shared.alive() {
            return Ok(());
        }
        #[cfg(unix)]
        {
            // The waiter cannot reap the program while this is held, so its id (and group)
            // stay its own.
            let _held = lock(&term.reap);
            if term.shared.alive()
                && let Some(pid) = term.pid
            {
                stop_group(pid);
            }
        }
        #[cfg(windows)]
        {
            // Closing the job (it kills on close) would do as well; terminating first does not
            // wait for the last handle to go.
            if let Some(job) = lock(&term.job).take() {
                job.terminate();
            }
            let _ = lock(&term.killer).kill();
        }
        let deadline = Instant::now() + KILL_WAIT;
        while term.shared.alive() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        lock(&term.input).take();
        if term.shared.alive() {
            Err((FailureKind::Io, format!("terminal {id} did not end")))
        } else {
            log!("killed {id}");
            Ok(())
        }
    }
}

/// A started program, until its terminal is recorded.
struct Started {
    child: Option<Box<dyn Child + Send + Sync>>,
    #[allow(dead_code)]
    pid: Option<u32>,
    #[cfg(windows)]
    job: Option<pitcrew_runtime::pty::windows::Job>,
}

impl Drop for Started {
    /// A start that failed after the program was started ends it.
    fn drop(&mut self) {
        let Some(mut child) = self.child.take() else {
            return;
        };
        #[cfg(unix)]
        if let Some(group) = self
            .pid
            .and_then(|pid| i32::try_from(pid).ok())
            .and_then(rustix::process::Pid::from_raw)
        {
            let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
        }
        #[cfg(windows)]
        if let Some(job) = &self.job {
            job.terminate();
        }
        let _ = child.kill();
        let _ = child.wait();
    }
}

/// `SIGTERM` to a process group, then `SIGKILL` if any of it is still there after
/// [`KILL_GRACE`]. The leader must not have been reaped (the caller holds the reap lock).
#[cfg(unix)]
fn stop_group(leader: u32) {
    use rustix::process::{Pid, Signal, kill_process_group, test_kill_process_group};
    let Some(group) = i32::try_from(leader).ok().and_then(Pid::from_raw) else {
        return;
    };
    if kill_process_group(group, Signal::TERM).is_err() {
        return;
    }
    let grace = Instant::now() + KILL_GRACE;
    while Instant::now() < grace {
        if test_kill_process_group(group).is_err() {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let _ = kill_process_group(group, Signal::KILL);
}

/// Reads a terminal's output until its end, recording it and acting on what it asks.
fn read_loop(mut reader: Box<dyn Read + Send>, shared: &Shared, answers: &SyncSender<Input>) {
    let mut buffer = vec![0u8; READ_CHUNK];
    let mut scanner = Scanner::default();
    let mut events = Vec::new();
    loop {
        let n = match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        let data = &buffer[..n];
        shared.screened.append(data);
        scanner.feed(data, &mut events);
        for event in events.drain(..) {
            // Answers are dropped when the queue is full: a flood of queries is not answered.
            match event {
                Event::AppCursor(on) => shared.app_cursor.store(on, Ordering::Release),
                Event::Answer(bytes) => {
                    let _ = answers.try_send(Input::Answer(bytes));
                }
                Event::Cursor { private } => {
                    let _ = answers.try_send(Input::Cursor { private });
                }
            }
        }
        shared.changed.notify_waiters();
    }
    shared.changed.notify_waiters();
}

/// Writes a terminal's input, in order, until its queue closes or the terminal does.
fn write_loop(
    mut writer: Box<dyn Write + Send>,
    queued: &Receiver<Input>,
    shared: &Shared,
    id: TerminalId,
) {
    while let Ok(input) = queued.recv() {
        let bytes = match input {
            Input::Bytes(bytes) => {
                shared.pending.fetch_sub(bytes.len(), Ordering::AcqRel);
                bytes
            }
            Input::Answer(bytes) => bytes.to_vec(),
            Input::Cursor { private } => {
                let screen = shared.screened.screen(&id);
                scan::cursor_report(private, screen.cursor_row, screen.cursor_col)
            }
        };
        if writer.write_all(&bytes).is_err() || writer.flush().is_err() {
            break;
        }
    }
}

/// Waits for a terminal's program to end and records how.
fn wait_loop(child: &mut Box<dyn Child + Send + Sync>, term: &Term) {
    #[cfg(unix)]
    {
        use rustix::process::{Pid, WaitId, WaitIdOptions, waitid};
        // Wait for its end without reaping it, so a kill in progress (which holds the reap
        // lock) still signals its own process group; then reap it under that lock.
        let pid = term
            .pid
            .and_then(|p| i32::try_from(p).ok())
            .and_then(Pid::from_raw);
        let ended = pid.is_some_and(|pid| {
            loop {
                match waitid(
                    WaitId::Pid(pid),
                    WaitIdOptions::EXITED | WaitIdOptions::NOWAIT,
                ) {
                    Err(rustix::io::Errno::INTR) => {}
                    result => break result.is_ok(),
                }
            }
        });
        if ended {
            let _held = lock(&term.reap);
            finish(term, child.wait().ok());
        } else {
            // Cannot wait without reaping: never hold the lock across a blocking wait.
            let status = child.wait();
            let _held = lock(&term.reap);
            finish(term, status.ok());
        }
    }
    #[cfg(windows)]
    {
        let status = child.wait();
        finish(term, status.ok());
        // Let the console deliver the last output, then close it, which ends the output pipe
        // (a console stays open after its program ends).
        std::thread::sleep(CLOSE_AFTER);
        let master = lock(&term.master).take();
        drop(master);
    }
    #[cfg(not(any(unix, windows)))]
    finish(term, child.wait().ok());
}

fn finish(term: &Term, status: Option<portable_pty::ExitStatus>) {
    let code = status
        .filter(|s| s.signal().is_none())
        .map(|s| i64::from(s.exit_code()));
    *lock(&term.shared.exit_code) = code;
    term.shared.alive.store(false, Ordering::Release);
    term.shared.changed.notify_waiters();
    log!("{} ended (exit code {code:?})", term.id);
}
