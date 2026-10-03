//! Transcript pages (api-v1, "Transcript paging"): [`RunnerTranscripts::transcript_page`].
//!
//! - **Only transcripts the watcher tracks.** A session is found by its id among them: the
//!   watcher adds each one it tracks to [`Watched`] and removes it when its file is deleted. A
//!   page never reads any other file, nor one the runner stopped watching.
//! - **Through the adapter's own `read_page`**, so whatever the adapter does to open a file safely
//!   applies here too: tail-first, up to `limit` items ending just before `before`, whole records
//!   only (a page may hold more than `limit` items), a partial last line left out, and `from`,
//!   `to` and `at_start` as the adapter gives them. The runner adds the limits: no limit is
//!   [`DEFAULT_PAGE_LIMIT`], more than [`MAX_PAGE_LIMIT`] counts as that, and `0` as `1`, so
//!   paging back always moves.
//! - **Bounded.** Reads run on a small pool of threads and are awaited for a limited time; a
//!   call finding every thread busy and the queue full is refused at once (see
//!   [`RunnerTranscripts`]).
//! - **Two errors.** A session the runner's index has never had is
//!   [`PageError::UnknownSession`]. One it has, whose transcript is gone, no longer watched, or
//!   cannot be read (an I/O error, an adapter that fails or panics, a read that did not end in
//!   time, a pool that is full), is [`PageError::Unavailable`]. A failed page is logged as a
//!   warning once per session, then at debug.

use crate::pool::{Pool, PoolError};
use crate::store::Store;
use crate::watch::{AdapterError, guard};
use pitcrew_interfaces::source::{SourceAdapter, SourceError, TranscriptPage, TranscriptRef};
use pitcrew_protocol::ids::SessionId;
use pitcrew_protocol::model::{Engine, TimestampMs};
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::io;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::time::Duration;

/// Items in a page when no limit is given.
pub const DEFAULT_PAGE_LIMIT: usize = 200;
/// Most items a page may ask for; a larger limit counts as this.
pub const MAX_PAGE_LIMIT: usize = 1000;

/// Why a transcript page could not be read. The API answers `404` for the first, `503` for the
/// second.
#[derive(Debug, thiserror::Error)]
pub enum PageError {
    /// The runner's index has no session by this id.
    #[error("no session {0} on this runner")]
    UnknownSession(SessionId),
    /// The runner knows the session, but its transcript is gone, no longer watched, or cannot be
    /// read now.
    #[error("the transcript of session {session} cannot be read: {reason}")]
    Unavailable {
        /// The session.
        session: SessionId,
        /// Why, without the transcript's path.
        reason: &'static str,
    },
}

/// The transcripts the watcher tracks, by session.
#[derive(Default)]
pub(crate) struct Watched {
    sessions: RwLock<HashMap<SessionId, Source>>,
}

/// A tracked transcript, as pages read it: its path shared with the watcher's own entry.
#[derive(Clone)]
pub(crate) struct Source {
    pub adapter: Arc<dyn SourceAdapter>,
    pub engine: Engine,
    pub path: Arc<Path>,
    pub inner_id: Option<Arc<str>>,
    /// Size and modification time when the watcher began tracking it.
    pub size: u64,
    pub modified: TimestampMs,
}

impl Source {
    fn transcript(&self) -> TranscriptRef {
        TranscriptRef {
            engine: self.engine,
            path: self.path.to_path_buf(),
            inner_id: self.inner_id.as_deref().map(str::to_owned),
            size: self.size,
            modified: self.modified,
        }
    }
}

impl Watched {
    /// The watcher tracks `session`'s transcript.
    pub fn insert(&self, session: SessionId, source: Source) {
        self.write().insert(session, source);
    }

    /// The watcher no longer tracks `session`'s transcript.
    pub fn remove(&self, session: SessionId) {
        self.write().remove(&session);
    }

    fn get(&self, session: SessionId) -> Option<Source> {
        self.sessions
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&session)
            .cloned()
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, HashMap<SessionId, Source>> {
        self.sessions
            .write()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

impl fmt::Debug for Watched {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let n = self
            .sessions
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .len();
        f.debug_struct("Watched")
            .field("sessions", &n)
            .finish_non_exhaustive()
    }
}

/// Tuning for [`RunnerTranscripts`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PageOptions {
    /// Longest wait for a page. Past it the call answers [`PageError::Unavailable`], and the read
    /// goes on on its thread.
    pub timeout: Duration,
    /// Threads reading pages.
    pub workers: usize,
    /// Reads that may wait for a thread; past it a call is refused at once.
    pub queue: usize,
}

impl Default for PageOptions {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(10),
            workers: 2,
            queue: 4,
        }
    }
}

/// Transcript pages of the sessions the runner watches, for the API's
/// `GET /v1/sessions/{id}/transcript`. Cheap to clone; clones share one small pool of threads.
/// Get it from [`RunnerHandle::transcripts`](crate::RunnerHandle::transcripts).
///
/// Every read runs on that pool and is awaited for at most [`PageOptions::timeout`]; with every
/// thread busy and the queue full, a call is refused at once. So a file system that stops
/// answering, and a client that keeps asking, cost these threads only, never the caller's.
///
/// Like the hooks and terminals, it keeps working with the runner's index after the runner
/// stops (with the transcripts the watcher tracked then), and keeps the index open.
#[derive(Clone)]
pub struct RunnerTranscripts {
    inner: Arc<Inner>,
}

struct Inner {
    watched: Arc<Watched>,
    store: Arc<Mutex<Store>>,
    pool: Pool,
    options: PageOptions,
    /// Sessions whose failed pages were logged as a warning; later ones are logged at debug.
    warned: Mutex<HashSet<SessionId>>,
}

impl fmt::Debug for RunnerTranscripts {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RunnerTranscripts")
            .field("watched", &self.inner.watched)
            .field("options", &self.inner.options)
            .finish_non_exhaustive()
    }
}

impl RunnerTranscripts {
    /// Starts the pool's threads.
    pub(crate) fn new(
        watched: Arc<Watched>,
        store: Arc<Mutex<Store>>,
        options: PageOptions,
    ) -> io::Result<Self> {
        Ok(Self {
            inner: Arc::new(Inner {
                watched,
                store,
                pool: Pool::start("pitcrew-pages", options.workers, options.queue)?,
                options,
                warned: Mutex::new(HashSet::new()),
            }),
        })
    }

    /// A page of `session`'s transcript, as api-v1's "Transcript paging" says: without `before`,
    /// the newest page; with a page's `from` as `before`, the one before it. `limit` counts items
    /// (`None`: [`DEFAULT_PAGE_LIMIT`]; at most [`MAX_PAGE_LIMIT`]; `0` counts as `1`).
    ///
    /// **Blocking**, for at most [`PageOptions::timeout`] (see the type's docs): call it off an
    /// async runtime.
    ///
    /// # Errors
    ///
    /// [`PageError::UnknownSession`] if the runner's index has no such session;
    /// [`PageError::Unavailable`] if its transcript is gone, not watched, or cannot be read, or
    /// the read did not end in time, or too many are under way.
    pub fn transcript_page(
        &self,
        session: SessionId,
        before: Option<u64>,
        limit: Option<usize>,
    ) -> Result<TranscriptPage, PageError> {
        let limit = limit.map_or(DEFAULT_PAGE_LIMIT, |l| l.clamp(1, MAX_PAGE_LIMIT));
        let inner = Arc::clone(&self.inner);
        let job = move || inner.read(session, before, limit);
        match self.inner.pool.run(self.inner.options.timeout, job) {
            Ok(page) => page,
            Err(PoolError::Busy) => Err(self.inner.failed(
                session,
                "the runner is busy reading other transcripts",
                &"every page thread is busy and the queue is full",
            )),
            Err(PoolError::TimedOut) => Err(self.inner.failed(
                session,
                "the transcript took too long to read",
                &format_args!("no answer within {:?}", self.inner.options.timeout),
            )),
            Err(PoolError::Panicked) => Err(self.inner.failed(
                session,
                "the transcript could not be read",
                &"the read panicked",
            )),
            Err(PoolError::Stopped) => Err(self.inner.failed(
                session,
                "the runner no longer reads transcripts",
                &"the page threads are gone",
            )),
        }
    }
}

impl Inner {
    /// The page, on a pool thread.
    fn read(
        &self,
        session: SessionId,
        before: Option<u64>,
        limit: usize,
    ) -> Result<TranscriptPage, PageError> {
        let Some(source) = self.watched.get(session) else {
            return Err(self.not_watched(session));
        };
        let transcript = source.transcript();
        match guard(|| source.adapter.read_page(&transcript, before, limit)) {
            Ok(page) => Ok(page),
            Err(AdapterError::Source(SourceError::Io(e)))
                if e.kind() == io::ErrorKind::NotFound =>
            {
                Err(PageError::Unavailable {
                    session,
                    reason: "the transcript is gone",
                })
            }
            Err(e) => Err(self.failed(session, "the transcript could not be read", &e)),
        }
    }

    /// The error for a session whose transcript the watcher does not track.
    fn not_watched(&self, session: SessionId) -> PageError {
        let known = self
            .store
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .has_session(session);
        match known {
            Ok(true) => PageError::Unavailable {
                session,
                reason: "the transcript is gone or no longer watched",
            },
            Ok(false) => PageError::UnknownSession(session),
            Err(e) => self.failed(session, "the runner's index could not be read", &e),
        }
    }

    /// A failed page: logged as a warning the first time for a session, then at debug, so a
    /// client that keeps asking does not fill the log.
    fn failed(
        &self,
        session: SessionId,
        reason: &'static str,
        error: &dyn fmt::Display,
    ) -> PageError {
        let first = self
            .warned
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(session);
        if first {
            tracing::warn!(%session, reason, error = %error, "a transcript page failed; later failures for this session are logged at debug");
        } else {
            tracing::debug!(%session, reason, error = %error, "a transcript page failed");
        }
        PageError::Unavailable { session, reason }
    }
}
