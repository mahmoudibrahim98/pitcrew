//! Terminals: [`RunnerTerminals`] is the API's `Terminals` seam over any `Runtime`.
//!
//! The runner owns the session → terminal mapping. It holds the terminals it started (linked to
//! their session once its transcript is found, see `RunnerCommands`) and those linked by hand,
//! in its own index, with each one's tmux target: a runtime that gives a terminal a new id after
//! a restart is followed by that target.
//!
//! The `Attachment` contract:
//! - **every call is bounded in time.** Runtime calls run on a small pool of threads and are
//!   awaited for at most [`TerminalOptions::call_timeout`]; a runtime that does not answer, or a
//!   pool that is full, gives `Unavailable` instead of a hang;
//! - **`exited()` is true only once all output is readable:** the program has ended and the end
//!   of its output stayed where it was for [`TerminalOptions::exit_settle`] (a runtime may still
//!   be draining the last bytes when it notices the exit);
//! - a terminal that disappears counts as ended: `exited()` is true and reads return nothing.
//!
//! The optional `changes()` push hint is not provided: the `Runtime` trait has no change
//! notification to build it from.

use crate::store::{Store, StoreError, TerminalRow};
use pitcrew_api::terminal::{Attachment, TerminalError, Terminals};
use pitcrew_interfaces::runtime::{OutputChunk, Runtime, RuntimeError, StartSpec, TerminalInfo};
use pitcrew_protocol::ids::{SessionId, TerminalId};
use pitcrew_protocol::runner::Key;
use std::fmt;
use std::io;
use std::panic::AssertUnwindSafe;
use std::sync::mpsc::{self, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// Tuning for [`RunnerTerminals`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TerminalOptions {
    /// Longest wait for a runtime call; past it the call answers `Unavailable`.
    pub call_timeout: Duration,
    /// Longest wait for a runtime to start a program.
    pub start_timeout: Duration,
    /// How long the output of an ended program must stay put before `exited()` says so.
    pub exit_settle: Duration,
    /// Threads making runtime calls.
    pub workers: usize,
    /// Calls that may wait for a thread; past it calls answer `Unavailable` at once.
    pub queue: usize,
}

impl Default for TerminalOptions {
    fn default() -> Self {
        Self {
            call_timeout: Duration::from_secs(5),
            start_timeout: Duration::from_secs(30),
            exit_settle: Duration::from_millis(50),
            workers: 4,
            queue: 64,
        }
    }
}

/// Sessions' terminals, over a runtime. Cheap to clone. Get it from
/// [`RunnerHandle::terminals`](crate::RunnerHandle::terminals).
#[derive(Clone)]
pub struct RunnerTerminals {
    inner: Arc<Inner>,
}

struct Inner {
    runtime: Arc<dyn Runtime>,
    store: Arc<Mutex<Store>>,
    calls: Calls,
    options: TerminalOptions,
}

impl fmt::Debug for RunnerTerminals {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RunnerTerminals")
            .field("runtime", &self.inner.runtime.kind())
            .field("options", &self.inner.options)
            .finish_non_exhaustive()
    }
}

impl RunnerTerminals {
    /// Starts the call threads, and brings the stored links up to date with what the runtime has.
    pub(crate) fn new(
        runtime: Arc<dyn Runtime>,
        store: Arc<Mutex<Store>>,
        options: TerminalOptions,
    ) -> io::Result<Self> {
        let terminals = Self {
            inner: Arc::new(Inner {
                runtime,
                store,
                calls: Calls::start(options.workers, options.queue)?,
                options,
            }),
        };
        terminals.refresh();
        Ok(terminals)
    }

    /// Records that `session` runs in `terminal` (e.g. a tmux window found by hand), replacing any
    /// terminal it had.
    ///
    /// # Errors
    ///
    /// The runner's index cannot be written.
    pub fn link(&self, session: SessionId, terminal: TerminalId) -> Result<(), StoreError> {
        let info = self.call(self.inner.options.call_timeout, move |rt| rt.info(terminal));
        let native_target = info.ok().and_then(|i| i.native_target);
        self.store().put_terminal(&TerminalRow {
            terminal,
            native_target,
            session: Some(session),
            engine: None,
            native_id: None,
            cwd: String::new(),
            started_at: crate::now_ms(),
        })
    }

    /// Forgets the session's terminal.
    ///
    /// # Errors
    ///
    /// The runner's index cannot be written.
    pub fn unlink(&self, session: SessionId) -> Result<(), StoreError> {
        self.store().unlink_session(session)
    }

    /// The session's terminal, if it has one.
    ///
    /// # Errors
    ///
    /// The runner's index cannot be read.
    pub fn terminal_of(&self, session: SessionId) -> Result<Option<TerminalId>, StoreError> {
        Ok(self.store().terminal_of(session)?.map(|t| t.terminal))
    }

    /// Follows terminals the runtime now knows by another id (same tmux target), and forgets
    /// those it no longer has.
    pub fn refresh(&self) {
        let listed = match self.call(self.inner.options.call_timeout, |rt| rt.list()) {
            Ok(listed) => listed,
            Err(e) => {
                tracing::warn!(error = %e, "cannot list terminals; keeping the stored links");
                return;
            }
        };
        let store = self.store();
        let rows = match store.terminals() {
            Ok(rows) => rows,
            Err(e) => {
                tracing::warn!(error = %e, "cannot read the stored terminals");
                return;
            }
        };
        for row in rows {
            if listed.iter().any(|t| t.id == row.terminal) {
                continue;
            }
            let moved = row.native_target.as_ref().and_then(|target| {
                listed
                    .iter()
                    .find(|t| t.native_target.as_ref() == Some(target))
            });
            let saved = match moved {
                Some(t) => store.retarget_terminal(row.terminal, t.id),
                None => {
                    tracing::debug!(terminal = %row.terminal, "a terminal is gone; forgetting it");
                    store.forget_terminal(row.terminal)
                }
            };
            if let Err(e) = saved {
                tracing::warn!(error = %e, "cannot update a stored terminal");
            }
        }
    }

    pub(crate) fn start(&self, spec: StartSpec) -> Result<TerminalInfo, TerminalError> {
        self.call(self.inner.options.start_timeout, move |rt| rt.start(&spec))
    }

    pub(crate) fn write(&self, terminal: TerminalId, bytes: Vec<u8>) -> Result<(), TerminalError> {
        self.call(self.inner.options.call_timeout, move |rt| {
            rt.write(terminal, &bytes)
        })
    }

    pub(crate) fn send_keys(
        &self,
        terminal: TerminalId,
        keys: Vec<Key>,
    ) -> Result<(), TerminalError> {
        self.call(self.inner.options.call_timeout, move |rt| {
            rt.send_keys(terminal, &keys)
        })
    }

    pub(crate) fn resize(
        &self,
        terminal: TerminalId,
        cols: u16,
        rows: u16,
    ) -> Result<(), TerminalError> {
        self.call(self.inner.options.call_timeout, move |rt| {
            rt.resize(terminal, cols, rows)
        })
    }

    pub(crate) fn info(&self, terminal: TerminalId) -> Result<TerminalInfo, TerminalError> {
        self.call(self.inner.options.call_timeout, move |rt| rt.info(terminal))
    }

    pub(crate) fn kill(&self, terminal: TerminalId) -> Result<(), TerminalError> {
        self.call(self.inner.options.call_timeout, move |rt| rt.kill(terminal))
    }

    pub(crate) fn store(&self) -> MutexGuard<'_, Store> {
        self.inner
            .store
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn call<T: Send + 'static>(
        &self,
        timeout: Duration,
        f: impl FnOnce(&dyn Runtime) -> Result<T, RuntimeError> + Send + 'static,
    ) -> Result<T, TerminalError> {
        let runtime = Arc::clone(&self.inner.runtime);
        self.inner.calls.run(timeout, move || f(&*runtime))
    }
}

impl Terminals for RunnerTerminals {
    fn attach(&self, session: SessionId) -> Result<Arc<dyn Attachment>, TerminalError> {
        let terminal = self
            .terminal_of(session)
            .map_err(|e| TerminalError::Failed(e.to_string()))?
            .ok_or_else(|| {
                TerminalError::NotFound(format!("Session {session} has no terminal."))
            })?;
        // Not found if the runtime no longer has it.
        self.info(terminal)?;
        Ok(Arc::new(RunnerAttachment {
            terminals: self.clone(),
            terminal,
            exit: Mutex::new(None),
        }))
    }
}

/// One session's terminal.
struct RunnerAttachment {
    terminals: RunnerTerminals,
    terminal: TerminalId,
    /// The output's end when the program was first seen ended, and when.
    exit: Mutex<Option<(u64, Instant)>>,
}

impl fmt::Debug for RunnerAttachment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RunnerAttachment")
            .field("terminal", &self.terminal)
            .finish_non_exhaustive()
    }
}

impl RunnerAttachment {
    fn timeout(&self) -> Duration {
        self.terminals.inner.options.call_timeout
    }
}

impl Attachment for RunnerAttachment {
    fn read(&self, from: u64, max: usize) -> Result<OutputChunk, TerminalError> {
        let id = self.terminal;
        match self
            .terminals
            .call(self.timeout(), move |rt| rt.read_output(id, from, max))
        {
            // Gone: it ended, and there is nothing more to read.
            Err(TerminalError::NotFound(_)) => Ok(OutputChunk {
                offset: from,
                data: Vec::new(),
                end: from,
                truncated: false,
            }),
            other => other,
        }
    }

    fn write(&self, bytes: &[u8]) -> Result<(), TerminalError> {
        self.terminals.write(self.terminal, bytes.to_vec())
    }

    fn resize(&self, cols: u16, rows: u16) -> Result<(), TerminalError> {
        self.terminals.resize(self.terminal, cols, rows)
    }

    fn exited(&self) -> Result<bool, TerminalError> {
        let id = self.terminal;
        let alive = match self.terminals.info(id) {
            Ok(info) => info.alive,
            Err(TerminalError::NotFound(_)) => return Ok(true),
            Err(e) => return Err(e),
        };
        let mut exit = self.exit.lock().unwrap_or_else(PoisonError::into_inner);
        if alive {
            *exit = None;
            return Ok(false);
        }
        drop(exit);
        // Where the output ends now.
        let end = match self
            .terminals
            .call(self.timeout(), move |rt| rt.read_output(id, u64::MAX, 0))
        {
            Ok(chunk) => chunk.end,
            Err(TerminalError::NotFound(_)) => return Ok(true),
            Err(e) => return Err(e),
        };
        let settle = self.terminals.inner.options.exit_settle;
        let mut exit = self.exit.lock().unwrap_or_else(PoisonError::into_inner);
        match *exit {
            Some((at, since)) if at == end => Ok(since.elapsed() >= settle),
            _ => {
                *exit = Some((end, Instant::now()));
                Ok(false)
            }
        }
    }
}

type Job = Box<dyn FnOnce() + Send + 'static>;

/// A small pool of threads for blocking runtime calls, so a caller can stop waiting for one.
struct Calls {
    queue: SyncSender<Job>,
}

impl Calls {
    /// The threads end once the pool is dropped and the calls they are in return.
    fn start(workers: usize, queue: usize) -> io::Result<Self> {
        let (tx, rx) = mpsc::sync_channel::<Job>(queue.max(1));
        let rx = Arc::new(Mutex::new(rx));
        for i in 0..workers.max(1) {
            let rx = Arc::clone(&rx);
            std::thread::Builder::new()
                .name(format!("pitcrew-runtime-{i}"))
                .spawn(move || {
                    loop {
                        let job = rx.lock().unwrap_or_else(PoisonError::into_inner).recv();
                        let Ok(job) = job else {
                            return;
                        };
                        // A panic drops the job's reply channel; its caller sees that.
                        let _ = std::panic::catch_unwind(AssertUnwindSafe(job));
                    }
                })?;
        }
        Ok(Self { queue: tx })
    }

    fn run<T: Send + 'static>(
        &self,
        timeout: Duration,
        f: impl FnOnce() -> Result<T, RuntimeError> + Send + 'static,
    ) -> Result<T, TerminalError> {
        let (reply, answer) = mpsc::sync_channel(1);
        let job: Job = Box::new(move || {
            let _ = reply.send(f());
        });
        match self.queue.try_send(job) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                return Err(TerminalError::Unavailable(
                    "The terminal runtime is busy.".into(),
                ));
            }
            Err(TrySendError::Disconnected(_)) => {
                return Err(TerminalError::Unavailable(
                    "The terminal runtime has stopped.".into(),
                ));
            }
        }
        match answer.recv_timeout(timeout) {
            Ok(result) => result.map_err(TerminalError::from),
            Err(RecvTimeoutError::Timeout) => Err(TerminalError::Unavailable(format!(
                "The terminal runtime did not answer within {timeout:?}."
            ))),
            Err(RecvTimeoutError::Disconnected) => Err(TerminalError::Failed(
                "The terminal runtime call panicked.".into(),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hung_call_times_out_and_a_full_pool_answers_at_once() {
        let calls = Calls::start(1, 1).expect("pool");
        let (release, hold) = mpsc::channel::<()>();
        let hold = Arc::new(Mutex::new(hold));
        let stuck = {
            let hold = Arc::clone(&hold);
            move || {
                let _ = hold.lock().map(|h| h.recv());
                Ok(())
            }
        };
        let started = Instant::now();
        let first = calls.run(Duration::from_millis(50), stuck);
        assert!(
            matches!(first, Err(TerminalError::Unavailable(_))),
            "{first:?}"
        );
        // The only thread is still stuck: the next call waits in the queue and times out...
        let second = calls.run(Duration::from_millis(50), || Ok(1));
        assert!(matches!(second, Err(TerminalError::Unavailable(_))));
        // ...and with the queue full, another is refused without waiting.
        let before = Instant::now();
        let third = calls.run(Duration::from_secs(10), || Ok(2));
        assert!(matches!(third, Err(TerminalError::Unavailable(_))));
        assert!(before.elapsed() < Duration::from_secs(1));
        assert!(started.elapsed() < Duration::from_secs(5));

        // Unstuck, the pool works again.
        let _ = release.send(());
        let mut answered = None;
        for _ in 0..100 {
            if let Ok(v) = calls.run(Duration::from_millis(100), || Ok(7)) {
                answered = Some(v);
                break;
            }
        }
        assert_eq!(answered, Some(7));
    }

    #[test]
    fn a_panicking_call_fails_and_the_pool_goes_on() {
        let calls = Calls::start(1, 4).expect("pool");
        let r: Result<(), _> = calls.run(Duration::from_secs(5), || panic!("runtime bug"));
        assert!(matches!(r, Err(TerminalError::Failed(_))), "{r:?}");
        assert_eq!(calls.run(Duration::from_secs(5), || Ok(3)).ok(), Some(3));
    }
}
