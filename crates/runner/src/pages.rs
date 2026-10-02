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
//! - **Two errors.** A session the runner's index has never had is
//!   [`PageError::UnknownSession`]. One it has, whose transcript is gone, no longer watched, or
//!   cannot be read (an I/O error, an adapter that fails or panics), is
//!   [`PageError::Unavailable`].

use crate::store::Store;
use crate::watch::{AdapterError, guard};
use pitcrew_interfaces::source::{SourceAdapter, SourceError, TranscriptPage, TranscriptRef};
use pitcrew_protocol::ids::SessionId;
use std::collections::HashMap;
use std::fmt;
use std::io;
use std::sync::{Arc, Mutex, PoisonError, RwLock};

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

#[derive(Clone)]
struct Source {
    adapter: Arc<dyn SourceAdapter>,
    transcript: TranscriptRef,
}

impl Watched {
    /// The watcher tracks `session`'s transcript, read by `adapter`.
    pub fn insert(
        &self,
        session: SessionId,
        adapter: Arc<dyn SourceAdapter>,
        transcript: TranscriptRef,
    ) {
        self.write().insert(
            session,
            Source {
                adapter,
                transcript,
            },
        );
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

/// Transcript pages of the sessions the runner watches, for the API's
/// `GET /v1/sessions/{id}/transcript`. Cheap to clone. Get it from
/// [`RunnerHandle::transcripts`](crate::RunnerHandle::transcripts).
///
/// Like the hooks and terminals, it keeps working with the runner's index after the runner
/// stops (with the transcripts the watcher tracked then), and keeps the index open.
#[derive(Clone, Debug)]
pub struct RunnerTranscripts {
    watched: Arc<Watched>,
    store: Arc<Mutex<Store>>,
}

impl RunnerTranscripts {
    pub(crate) fn new(watched: Arc<Watched>, store: Arc<Mutex<Store>>) -> Self {
        Self { watched, store }
    }

    /// A page of `session`'s transcript, as api-v1's "Transcript paging" says: without `before`,
    /// the newest page; with a page's `from` as `before`, the one before it. `limit` counts items
    /// (`None`: [`DEFAULT_PAGE_LIMIT`]; at most [`MAX_PAGE_LIMIT`]; `0` counts as `1`).
    ///
    /// **Blocking**, and as slow as the filesystem: call it off an async runtime, with a timeout
    /// (a network filesystem that stops answering may not return).
    ///
    /// # Errors
    ///
    /// [`PageError::UnknownSession`] if the runner's index has no such session;
    /// [`PageError::Unavailable`] if its transcript is gone, not watched, or cannot be read.
    pub fn transcript_page(
        &self,
        session: SessionId,
        before: Option<u64>,
        limit: Option<usize>,
    ) -> Result<TranscriptPage, PageError> {
        let limit = limit.map_or(DEFAULT_PAGE_LIMIT, |l| l.clamp(1, MAX_PAGE_LIMIT));
        let Some(source) = self.watched.get(session) else {
            return Err(self.not_watched(session));
        };
        match guard(|| source.adapter.read_page(&source.transcript, before, limit)) {
            Ok(page) => Ok(page),
            Err(AdapterError::Source(SourceError::Io(e)))
                if e.kind() == io::ErrorKind::NotFound =>
            {
                Err(PageError::Unavailable {
                    session,
                    reason: "the transcript is gone",
                })
            }
            Err(e) => {
                tracing::warn!(%session, error = %e, "cannot read a page of a transcript");
                Err(PageError::Unavailable {
                    session,
                    reason: "the transcript could not be read",
                })
            }
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
            Err(e) => {
                tracing::warn!(%session, error = %e, "cannot look up a session in the runner's index");
                PageError::Unavailable {
                    session,
                    reason: "the runner's index could not be read",
                }
            }
        }
    }
}
