//! Terminals: [`RunnerTerminals`] is the API's `Terminals` seam over any `Runtime`.
//!
//! The runner owns the session → terminal mapping. It holds the terminals it started (linked to
//! their session once its transcript is found, see `RunnerCommands`) and those linked by hand,
//! in its own index, with each one's tmux target (for people to attach). A link is kept only
//! while the runtime lists its terminal **by that id**: a runtime keeps a terminal's id across a
//! restart (tmux tags each pane with it), and a target alone is never followed, since another
//! server can reuse it for a terminal that is not this one.
//!
//! The `Attachment` contract:
//! - **every call is bounded in time.** Runtime calls run on a small pool of threads and are
//!   awaited for at most [`TerminalOptions::call_timeout`]; a runtime that does not answer, or a
//!   pool that is full, gives `Unavailable` instead of a hang;
//! - **`exited()` is true only once all output is readable:** the program has ended and the end
//!   of its output stayed where it was for [`TerminalOptions::exit_settle`] (a runtime may still
//!   be draining the last bytes when it notices the exit). Each `attach()` gets its own settle
//!   state: two attachments to one session's terminal each wait out the settle time themselves,
//!   and neither's calls move the other's;
//! - a terminal that disappears counts as ended: `exited()` is true and reads return nothing.
//!
//! The optional `changes()` push hint is not provided: the `Runtime` trait has no change
//! notification to build it from.

use crate::pool::{Pool, PoolError};
use crate::store::{Store, StoreError, TerminalRow};
use pitcrew_api::terminal::{Attachment, TerminalError, Terminals};
use pitcrew_interfaces::runtime::{OutputChunk, Runtime, RuntimeError, StartSpec, TerminalInfo};
use pitcrew_protocol::ids::{SessionId, TerminalId};
use pitcrew_protocol::runner::Key;
use std::fmt;
use std::io;
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

    /// The session that runs in `terminal`, once the runner has linked them (when the session's
    /// transcript is found, for a terminal it started).
    ///
    /// # Errors
    ///
    /// The runner's index cannot be read.
    pub fn session_of(&self, terminal: TerminalId) -> Result<Option<SessionId>, StoreError> {
        Ok(self
            .store()
            .terminals()?
            .into_iter()
            .find(|t| t.terminal == terminal)
            .and_then(|t| t.session))
    }

    /// Forgets the terminals the runtime no longer lists by their id. A terminal listed under
    /// another id is not this one, even with the same tmux target: another server (after a
    /// restart, or another daemon's) may have given that target to a different terminal.
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
            tracing::debug!(terminal = %row.terminal, "a terminal is gone; forgetting it");
            if let Err(e) = store.forget_terminal(row.terminal) {
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

/// Runtime calls, on a small pool of threads (see `pool`), so a caller can stop waiting for one.
struct Calls {
    pool: Pool,
}

impl Calls {
    /// The threads end once the pool is dropped and the calls they are in return.
    fn start(workers: usize, queue: usize) -> io::Result<Self> {
        Ok(Self {
            pool: Pool::start("pitcrew-runtime", workers, queue)?,
        })
    }

    fn run<T: Send + 'static>(
        &self,
        timeout: Duration,
        f: impl FnOnce() -> Result<T, RuntimeError> + Send + 'static,
    ) -> Result<T, TerminalError> {
        match self.pool.run(timeout, f) {
            Ok(result) => result.map_err(TerminalError::from),
            Err(PoolError::Busy) => Err(TerminalError::Unavailable(
                "The terminal runtime is busy.".into(),
            )),
            Err(PoolError::Stopped) => Err(TerminalError::Unavailable(
                "The terminal runtime has stopped.".into(),
            )),
            Err(PoolError::TimedOut) => Err(TerminalError::Unavailable(format!(
                "The terminal runtime did not answer within {timeout:?}."
            ))),
            Err(PoolError::Panicked) => Err(TerminalError::Failed(
                "The terminal runtime call panicked.".into(),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    /// Waits for the pool's thread to reach a point, however loaded the machine is.
    const REACHED: Duration = Duration::from_secs(60);

    /// Unavailable because the call did not answer in time.
    fn timed_out<T>(r: &Result<T, TerminalError>) -> bool {
        matches!(r, Err(TerminalError::Unavailable(m)) if m.contains("did not answer"))
    }

    /// Unavailable because the queue was full: refused without waiting for an answer.
    fn refused<T>(r: &Result<T, TerminalError>) -> bool {
        matches!(r, Err(TerminalError::Unavailable(m)) if m.contains("busy"))
    }

    /// Each step waits for the pool's thread to get where it must be (channels), not for a
    /// margin of time, so a loaded machine makes it slower, never wrong.
    #[test]
    fn a_hung_call_times_out_and_a_full_pool_answers_at_once() {
        let calls = Calls::start(1, 1).expect("pool");
        let (entered, in_stuck) = mpsc::channel::<()>();
        let (release, hold) = mpsc::channel::<()>();
        let stuck = move || {
            let _ = entered.send(());
            let _ = hold.recv();
            Ok(())
        };
        let first = calls.run(Duration::from_millis(50), stuck);
        assert!(timed_out(&first), "{first:?}");
        // The only thread is now in the stuck call: the queue is empty.
        in_stuck
            .recv_timeout(REACHED)
            .expect("the thread took the stuck call");

        // The next call waits in the queue and times out...
        let (ran, queued_ran) = mpsc::channel::<()>();
        let second = calls.run(Duration::from_millis(50), move || {
            let _ = ran.send(());
            Ok(1)
        });
        assert!(timed_out(&second), "{second:?}");
        // ...and with the queue full, another is refused at once, not after its timeout.
        let asked = Instant::now();
        let third = calls.run(Duration::from_secs(10), || Ok(2));
        assert!(refused(&third), "{third:?}");
        assert!(
            asked.elapsed() < Duration::from_secs(9),
            "{:?}",
            asked.elapsed()
        );

        // Unstuck, the thread runs the queued call, and the pool works again.
        release.send(()).expect("the stuck call is waiting");
        queued_ran
            .recv_timeout(REACHED)
            .expect("the queued call ran");
        assert_eq!(calls.run(REACHED, || Ok(7)).ok(), Some(7));
    }

    #[test]
    fn a_panicking_call_fails_and_the_pool_goes_on() {
        let calls = Calls::start(1, 4).expect("pool");
        let r: Result<(), _> = calls.run(Duration::from_secs(5), || panic!("runtime bug"));
        assert!(matches!(r, Err(TerminalError::Failed(_))), "{r:?}");
        assert_eq!(calls.run(Duration::from_secs(5), || Ok(3)).ok(), Some(3));
    }
}
