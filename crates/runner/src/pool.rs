//! A small pool of threads for blocking calls a caller may stop waiting for: a call that does not
//! return in time answers [`PoolError::TimedOut`] and is left running on its thread, and when
//! every thread is busy and the queue is full, a call is refused at once ([`PoolError::Busy`]).
//! So a file or a runtime that hangs costs the pool's threads, never the caller's.

use std::io;
use std::panic::AssertUnwindSafe;
use std::sync::mpsc::{self, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

type Job = Box<dyn FnOnce() + Send + 'static>;

/// Why a call through a [`Pool`] gave no answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PoolError {
    /// Every thread is busy and the queue is full: refused without waiting.
    Busy,
    /// The pool's threads are gone.
    Stopped,
    /// No answer within the timeout; the call goes on on its thread.
    TimedOut,
    /// The call panicked.
    Panicked,
}

pub(crate) struct Pool {
    queue: SyncSender<Job>,
}

impl Pool {
    /// `workers` threads named `<name>-<n>`, and room for `queue` calls waiting for one. The
    /// threads end once the pool is dropped and the calls they are in return.
    pub fn start(name: &str, workers: usize, queue: usize) -> io::Result<Self> {
        let (tx, rx) = mpsc::sync_channel::<Job>(queue.max(1));
        let rx = Arc::new(Mutex::new(rx));
        for i in 0..workers.max(1) {
            let rx = Arc::clone(&rx);
            std::thread::Builder::new()
                .name(format!("{name}-{i}"))
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

    /// Runs `f` on a pool thread and waits at most `timeout` for its answer.
    pub fn run<T: Send + 'static>(
        &self,
        timeout: Duration,
        f: impl FnOnce() -> T + Send + 'static,
    ) -> Result<T, PoolError> {
        let (reply, answer) = mpsc::sync_channel(1);
        let job: Job = Box::new(move || {
            let _ = reply.send(f());
        });
        match self.queue.try_send(job) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => return Err(PoolError::Busy),
            Err(TrySendError::Disconnected(_)) => return Err(PoolError::Stopped),
        }
        match answer.recv_timeout(timeout) {
            Ok(result) => Ok(result),
            Err(RecvTimeoutError::Timeout) => Err(PoolError::TimedOut),
            Err(RecvTimeoutError::Disconnected) => Err(PoolError::Panicked),
        }
    }
}
