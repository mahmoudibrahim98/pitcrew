//! `GET /v1/sessions/{id}/transcript?before=&limit=` (api-v1, "Transcript paging"), over the
//! transcripts the runner watches.
//!
//! **A stand-in, here until the runner serves pages itself.** The runner knows which transcript
//! each of its sessions is, but does not say (its index is its own); and `pitcrew-api` has no
//! transcript seam yet. So the daemon:
//! - hands the runner its adapters wrapped in [`Recorded`], which keeps what each home's last
//!   discovery found ([`Found`]); the route reads only those transcripts, so only the homes the
//!   runner watches;
//! - finds a session's transcript by its engine and native id, by the names each CLI gives its
//!   files: Claude's `<id>.jsonl` and its sub-agents' `agent-<id>.jsonl`, Codex's
//!   `rollout-<time>-<id>.jsonl`, OpenCode's inner id in its store ([`names`]);
//! - pages it with the adapter's own `read_page`. A read that does not return within 10 seconds
//!   (a file on a filesystem that does not answer) answers `503` and is left running; a stop
//!   waits for it only so long (`serve`).
//!
//! Answers: an unknown session is `404`; one on another machine, or any without a runner, is
//! `503 unavailable`; one on this machine whose transcript is not found (a demo session, a
//! deleted file) is an empty page at the start, as the mock hub answers for a session it has no
//! transcript for. `limit` defaults to 200 and counts as 1000 above it; `limit=0`, and a `before`
//! or `limit` that is not a whole number, are `400`.

use axum::extract::rejection::{PathRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::routing::get;
use axum::{Json, Router};
use pitcrew_auth::ErrorResponse;
use pitcrew_hub_work::WorkService;
use pitcrew_interfaces::source::{
    Cursor, ParseChunk, SourceAdapter, SourceError, TranscriptPage, TranscriptRef,
};
use pitcrew_protocol::api::ErrorCode;
use pitcrew_protocol::ids::{MachineId, SessionId};
use pitcrew_protocol::model::Engine;
use serde::Deserialize;
use std::collections::HashMap;
use std::fmt;
use std::path::{Path as FsPath, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

/// Items in a page when `limit` is not given.
const DEFAULT_LIMIT: usize = 200;
/// Most items in a page; a larger `limit` counts as this.
const MAX_LIMIT: usize = 1000;
/// Longest a page may take to read.
const READ_TIMEOUT: Duration = Duration::from_secs(10);

/// What the last discovery of each watched home found.
#[derive(Default)]
pub struct Found {
    homes: Mutex<HashMap<(Engine, PathBuf), Discovered>>,
}

struct Discovered {
    adapter: Arc<dyn SourceAdapter>,
    transcripts: Vec<TranscriptRef>,
}

impl fmt::Debug for Found {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let homes = self.homes.lock().unwrap_or_else(PoisonError::into_inner);
        f.debug_struct("Found")
            .field("homes", &homes.len())
            .finish_non_exhaustive()
    }
}

impl Found {
    fn record(&self, adapter: &Arc<dyn SourceAdapter>, home: &FsPath, found: &[TranscriptRef]) {
        self.homes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(
                (adapter.engine(), home.to_path_buf()),
                Discovered {
                    adapter: Arc::clone(adapter),
                    transcripts: found.to_vec(),
                },
            );
    }

    /// The transcript the CLI `engine` names `native` (see [`names`]), with the adapter that
    /// reads it; the newest if several are (one session in two homes).
    fn find(
        &self,
        engine: Engine,
        native: &str,
    ) -> Option<(Arc<dyn SourceAdapter>, TranscriptRef)> {
        if native.is_empty() {
            return None;
        }
        let homes = self.homes.lock().unwrap_or_else(PoisonError::into_inner);
        homes
            .iter()
            .filter(|((e, _), _)| *e == engine)
            .flat_map(|(_, d)| d.transcripts.iter().map(move |t| (&d.adapter, t)))
            .filter(|(_, t)| names(engine, t, native))
            .max_by_key(|(_, t)| t.modified)
            .map(|(adapter, t)| (Arc::clone(adapter), t.clone()))
    }
}

/// Whether `t` is the transcript `engine` writes for the session it calls `native`, by the names
/// that CLI gives its files, so an id never matches another kind of file's name:
/// - Claude: `<native>.jsonl`, or `agent-<native>.jsonl` for a sub-agent;
/// - Codex: `rollout-<time>-<native>.jsonl`, or the whole name when its records name no id;
/// - OpenCode: the store's inner id.
fn names(engine: Engine, t: &TranscriptRef, native: &str) -> bool {
    let stem = || t.path.file_stem().and_then(|s| s.to_str());
    match engine {
        Engine::OpenCode => t.inner_id.as_deref() == Some(native),
        Engine::Claude => {
            stem().is_some_and(|stem| stem == native || stem.strip_prefix("agent-") == Some(native))
        }
        Engine::Codex => stem().is_some_and(|stem| {
            stem.strip_prefix("rollout-").is_some_and(|rest| {
                stem == native
                    || rest
                        .strip_suffix(native)
                        .is_some_and(|time| time.ends_with('-'))
            })
        }),
        _ => false,
    }
}

/// An adapter that records what each discovery finds in [`Found`], and otherwise is `inner`.
pub struct Recorded {
    inner: Arc<dyn SourceAdapter>,
    found: Arc<Found>,
}

impl Recorded {
    /// `inner`, recording into `found`.
    #[must_use]
    pub fn new(inner: Arc<dyn SourceAdapter>, found: Arc<Found>) -> Self {
        Self { inner, found }
    }
}

impl fmt::Debug for Recorded {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Recorded")
            .field("engine", &self.inner.engine())
            .finish_non_exhaustive()
    }
}

impl SourceAdapter for Recorded {
    fn engine(&self) -> Engine {
        self.inner.engine()
    }

    fn discover(&self, home: &FsPath) -> Result<Vec<TranscriptRef>, SourceError> {
        let found = self.inner.discover(home)?;
        self.found.record(&self.inner, home, &found);
        Ok(found)
    }

    fn read_from(
        &self,
        transcript: &TranscriptRef,
        cursor: &Cursor,
    ) -> Result<ParseChunk, SourceError> {
        self.inner.read_from(transcript, cursor)
    }

    fn read_page(
        &self,
        transcript: &TranscriptRef,
        before: Option<u64>,
        limit: usize,
    ) -> Result<TranscriptPage, SourceError> {
        self.inner.read_page(transcript, before, limit)
    }
}

/// The route's state.
#[derive(Debug)]
pub struct Transcripts {
    work: Arc<WorkService>,
    /// The runner's machine and what it found; `None` without a runner.
    runner: Option<(MachineId, Arc<Found>)>,
}

impl Transcripts {
    /// Transcripts of the sessions `work` knows, among those the runner on `machine` found.
    #[must_use]
    pub fn new(work: Arc<WorkService>, runner: Option<(MachineId, Arc<Found>)>) -> Self {
        Self { work, runner }
    }

    /// The route. Mount it as a **device** route (`RouterParts::device`).
    pub fn routes(self) -> Router {
        Router::new()
            .route("/v1/sessions/{id}/transcript", get(page))
            .with_state(Arc::new(self))
    }

    /// The page, or why not. Blocking.
    fn page(
        &self,
        session: SessionId,
        before: Option<u64>,
        limit: usize,
    ) -> Result<TranscriptPage, ErrorResponse> {
        let found = match self.work.session(&session) {
            Ok(found) => found,
            Err(e) if e.code() == ErrorCode::NotFound => {
                return Err(not_found(&session));
            }
            Err(e) => {
                tracing::error!(error = %e, %session, "cannot look up a session for its transcript");
                return Err(ErrorResponse::new(
                    ErrorCode::Internal,
                    "The session could not be looked up.",
                ));
            }
        };
        let Some((machine, transcripts)) = &self.runner else {
            return Err(ErrorResponse::new(
                ErrorCode::Unavailable,
                format!(
                    "Session {session}'s transcript cannot be read: no runner is attached to this \
                     hub."
                ),
            ));
        };
        if found.machine != *machine {
            return Err(ErrorResponse::new(
                ErrorCode::Unavailable,
                format!(
                    "Session {session} runs on another machine, which this hub cannot reach yet."
                ),
            ));
        }
        let Some((adapter, transcript)) = transcripts.find(found.engine, &found.native_id) else {
            return Ok(empty());
        };
        match adapter.read_page(&transcript, before, limit) {
            Ok(page) => Ok(page),
            Err(SourceError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => Ok(empty()),
            Err(e) => {
                tracing::warn!(error = %e, %session, "cannot read a session's transcript");
                Err(ErrorResponse::new(
                    ErrorCode::Internal,
                    "The transcript could not be read.",
                ))
            }
        }
    }
}

/// A transcript with nothing in it.
fn empty() -> TranscriptPage {
    TranscriptPage {
        items: Vec::new(),
        from: 0,
        to: 0,
        at_start: true,
    }
}

fn not_found(session: &dyn fmt::Display) -> ErrorResponse {
    ErrorResponse::new(ErrorCode::NotFound, format!("No session {session}."))
}

fn invalid(message: impl Into<String>) -> ErrorResponse {
    ErrorResponse::new(ErrorCode::Invalid, message)
}

/// The query, as text: each value is checked by hand, so a bad one is a `400` in the API's shape.
#[derive(Debug, Deserialize)]
struct PageQuery {
    before: Option<String>,
    limit: Option<String>,
}

/// A whole number written in ASCII digits.
fn whole(value: &str) -> Option<u64> {
    if value.is_empty() || value.len() > 19 || !value.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    value.parse().ok()
}

async fn page(
    State(transcripts): State<Arc<Transcripts>>,
    id: Result<Path<String>, PathRejection>,
    query: Result<Query<PageQuery>, QueryRejection>,
) -> Result<Json<TranscriptPage>, ErrorResponse> {
    let Path(id) = id.map_err(|_| invalid("The session id must be plain text."))?;
    // A malformed id names no session, as on the other session routes.
    let session: SessionId = id.parse().map_err(|_| not_found(&id))?;
    let Query(query) = query.map_err(|_| invalid("The query could not be read."))?;
    let before = query
        .before
        .as_deref()
        .map(|v| whole(v).ok_or_else(|| invalid("before must be a whole number.")))
        .transpose()?;
    let limit = match query.limit.as_deref() {
        None => DEFAULT_LIMIT,
        Some(v) => match whole(v) {
            Some(0) | None => return Err(invalid("limit must be a whole number of at least 1.")),
            Some(n) => usize::try_from(n).unwrap_or(MAX_LIMIT).min(MAX_LIMIT),
        },
    };
    let read = tokio::task::spawn_blocking(move || transcripts.page(session, before, limit));
    match tokio::time::timeout(READ_TIMEOUT, read).await {
        Ok(Ok(page)) => page.map(Json),
        Ok(Err(e)) => {
            tracing::error!(error = %e, "reading a transcript failed");
            Err(ErrorResponse::new(
                ErrorCode::Internal,
                "The transcript could not be read.",
            ))
        }
        Err(_) => {
            tracing::warn!(
                %session,
                seconds = READ_TIMEOUT.as_secs(),
                "a transcript read has not returned; it is left running"
            );
            Err(ErrorResponse::new(
                ErrorCode::Unavailable,
                "The transcript took too long to read.",
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(path: &str, inner: Option<&str>, modified: i64) -> TranscriptRef {
        TranscriptRef {
            engine: Engine::Claude,
            path: PathBuf::from(path),
            inner_id: inner.map(Into::into),
            size: 0,
            modified,
        }
    }

    #[test]
    fn transcripts_are_found_by_the_name_their_cli_gives_them() {
        use Engine::{Claude, Codex, OpenCode};
        let uuid = "2b6f1a8e-4c1d-4f5e-9a37-0c8d1e2f3a4b";
        let main = at(&format!("/h/p/{uuid}.jsonl"), None, 0);
        let sub = at("/h/p/s/subagents/agent-a1b2c3.jsonl", None, 0);
        let rollout = at(
            &format!("/h/sessions/2026/09/30/rollout-2026-09-30T08-00-00-{uuid}.jsonl"),
            None,
            0,
        );
        let store = at("/h/opencode.db", Some("ses_1"), 0);
        // Claude: `<id>.jsonl`, sub-agents `agent-<id>.jsonl`.
        assert!(names(Claude, &main, uuid));
        assert!(names(Claude, &sub, "a1b2c3"));
        // Codex: `rollout-<time>-<id>.jsonl`, or the whole name.
        assert!(names(Codex, &rollout, uuid));
        let stem = format!("rollout-2026-09-30T08-00-00-{uuid}");
        assert!(names(Codex, &rollout, &stem));
        // OpenCode: the inner id, whatever the store's file is called.
        assert!(names(OpenCode, &store, "ses_1"));
        assert!(!names(
            OpenCode,
            &at("/h/ses_1.db", Some("ses_2"), 0),
            "ses_1"
        ));

        // Each engine's rule only: a Claude id never names a rollout, nor a Codex id a Claude
        // file or a sub-agent, nor a file name an OpenCode session.
        assert!(!names(Claude, &rollout, uuid));
        assert!(!names(Codex, &main, uuid));
        assert!(!names(Codex, &sub, "a1b2c3"));
        assert!(!names(OpenCode, &main, uuid));
        // Not a part of another name.
        for other in [
            "/h/p/xa1b2c3.jsonl",
            "/h/p/a1b2c3-x.jsonl",
            "/h/p/old-a1b2c3.jsonl",
            "/h/p/agent-xa1b2c3.jsonl",
        ] {
            assert!(!names(Claude, &at(other, None, 0), "a1b2c3"), "{other}");
        }
        let other_rollout = at(&format!("/h/x-2026-{uuid}.jsonl"), None, 0);
        assert!(!names(Codex, &other_rollout, uuid));
    }

    #[derive(Debug)]
    struct Lists(Vec<TranscriptRef>);
    impl SourceAdapter for Lists {
        fn engine(&self) -> Engine {
            Engine::Claude
        }
        fn discover(&self, _home: &FsPath) -> Result<Vec<TranscriptRef>, SourceError> {
            Ok(self.0.clone())
        }
        fn read_from(&self, _: &TranscriptRef, _: &Cursor) -> Result<ParseChunk, SourceError> {
            Ok(ParseChunk::default())
        }
        fn read_page(
            &self,
            _: &TranscriptRef,
            _: Option<u64>,
            _: usize,
        ) -> Result<TranscriptPage, SourceError> {
            Ok(empty())
        }
    }

    #[test]
    fn a_discovery_replaces_what_the_home_had_and_the_newest_wins() {
        let found = Arc::new(Found::default());
        let old = at("/h/p/s1.jsonl", None, 9);
        let other = at("/h/p/old-s1.jsonl", None, 99);
        let first = Recorded::new(
            Arc::new(Lists(vec![old.clone(), other])),
            Arc::clone(&found),
        );
        assert_eq!(first.discover(FsPath::new("/h")).unwrap().len(), 2);
        assert_eq!(
            found.find(Engine::Claude, "s1").map(|f| f.1),
            Some(old.clone())
        );
        assert!(found.find(Engine::Codex, "s1").is_none());
        assert!(found.find(Engine::Claude, "").is_none());

        // A home's next discovery replaces what it had.
        let none = Recorded::new(Arc::new(Lists(Vec::new())), Arc::clone(&found));
        none.discover(FsPath::new("/h")).unwrap();
        assert!(found.find(Engine::Claude, "s1").is_none());

        // The same session in two homes: the newest.
        let newer = at("/h2/p/s1.jsonl", None, 10);
        Recorded::new(Arc::new(Lists(vec![old.clone()])), Arc::clone(&found))
            .discover(FsPath::new("/h"))
            .unwrap();
        Recorded::new(Arc::new(Lists(vec![newer.clone()])), Arc::clone(&found))
            .discover(FsPath::new("/h2"))
            .unwrap();
        assert_eq!(found.find(Engine::Claude, "s1").map(|f| f.1), Some(newer));
    }

    #[test]
    fn whole_numbers_only() {
        assert_eq!(whole("0"), Some(0));
        assert_eq!(whole("6514"), Some(6514));
        for bad in ["", "-1", "1.5", "+1", " 1", "abc", "99999999999999999999"] {
            assert_eq!(whole(bad), None, "{bad}");
        }
    }
}
