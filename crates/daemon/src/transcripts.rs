//! `GET /v1/sessions/{id}/transcript?before=&limit=` (api-v1, "Transcript paging"), from the
//! runner's own transcript pages (`RunnerTranscripts`).
//!
//! The route checks what only the hub knows, then asks the runner, which finds the session's
//! transcript by its id among those it watches and reads the page with the adapter's own
//! `read_page`, on its own small pool of threads, bounded in time:
//! - the query: `limit` defaults to 200 and counts as 1000 above it; `limit=0`, and a `before` or
//!   `limit` that is not a whole number, are `400`;
//! - an unknown session is `404`; one on another machine, or any without a runner (with
//!   `--no-runner`, or before a fresh workspace is set up), is `503 unavailable`;
//! - a session of this machine that the runner has not indexed (a demo session, or a dispatched
//!   one whose transcript does not exist yet) is an empty page with `at_start: true`, as for a
//!   session without a transcript;
//! - a transcript that is gone or cannot be read, and a read that is busy or does not finish in
//!   time, are `503 unavailable`, saying which (the runner logs the first failure for a session as
//!   a warning, later ones at debug).
//!
//! The runner bounds each read (10 seconds) on its own threads, so a filesystem that does not
//! answer ties up those threads, never tokio's blocking pool for long. The route still gives the
//! call [`READ_TIMEOUT`] as a backstop.

use crate::runner::Attached;
use axum::extract::rejection::{PathRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::routing::get;
use axum::{Json, Router};
use pitcrew_auth::ErrorResponse;
use pitcrew_hub_work::WorkService;
use pitcrew_interfaces::source::TranscriptPage;
use pitcrew_protocol::api::ErrorCode;
use pitcrew_protocol::ids::SessionId;
use pitcrew_runner::{DEFAULT_PAGE_LIMIT, MAX_PAGE_LIMIT, PageError};
use serde::Deserialize;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

/// Longest the route waits for a page: the runner's own bound (10 seconds), and some slack.
const READ_TIMEOUT: Duration = Duration::from_secs(15);

/// The route's state.
#[derive(Debug)]
pub struct Transcripts {
    work: Arc<WorkService>,
    /// The runner, once it runs: its machine and its transcript pages.
    runner: Arc<Attached>,
}

impl Transcripts {
    /// Transcripts of the sessions `work` knows, read by the runner on its machine once it runs.
    #[must_use]
    pub fn new(work: Arc<WorkService>, runner: Arc<Attached>) -> Self {
        Self { work, runner }
    }

    /// The route. Mount it as a **device** route (`RouterParts::device`).
    pub fn routes(self) -> Router {
        Router::new()
            .route("/v1/sessions/{id}/transcript", get(page))
            .with_state(Arc::new(self))
    }

    /// The page, or why not. Blocking, for at most the runner's bound.
    fn page(
        &self,
        session: SessionId,
        before: Option<u64>,
        limit: usize,
    ) -> Result<TranscriptPage, ErrorResponse> {
        if !self
            .work
            .session_included(&session)
            .map_err(|e| ErrorResponse::new(e.code(), "Could not read session inclusion."))?
        {
            return Err(not_found(&session));
        }
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
        let Some(runner) = self.runner.get() else {
            return Err(ErrorResponse::new(
                ErrorCode::Unavailable,
                format!(
                    "Session {session}'s transcript cannot be read: no runner is attached to this \
                     hub."
                ),
            ));
        };
        if found.machine != runner.machine {
            return Err(ErrorResponse::new(
                ErrorCode::Unavailable,
                format!(
                    "Session {session} runs on another machine, which this hub cannot reach yet."
                ),
            ));
        }
        match runner
            .transcripts
            .transcript_page(session, before, Some(limit))
        {
            Ok(page) => Ok(page),
            // A session of this machine the runner never indexed has no transcript here yet.
            Err(PageError::UnknownSession(_)) => Ok(empty()),
            Err(PageError::Unavailable { reason, .. }) => Err(ErrorResponse::new(
                ErrorCode::Unavailable,
                format!("Session {session}'s transcript cannot be read now: {reason}."),
            )),
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
        None => DEFAULT_PAGE_LIMIT,
        Some(v) => match whole(v) {
            Some(0) | None => return Err(invalid("limit must be a whole number of at least 1.")),
            Some(n) => usize::try_from(n)
                .unwrap_or(MAX_PAGE_LIMIT)
                .min(MAX_PAGE_LIMIT),
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
    use crate::runner::Runner;
    use pitcrew_protocol::events::{Event, EventBody};
    use pitcrew_protocol::ids::{MachineId, MemberId, WorkspaceId};
    use pitcrew_protocol::model::{Engine, Session, SessionState, Workspace};
    use pitcrew_store::{Store, StoreOptions};

    #[test]
    fn whole_numbers_only() {
        assert_eq!(whole("0"), Some(0));
        assert_eq!(whole("6514"), Some(6514));
        for bad in ["", "-1", "1.5", "+1", " 1", "abc", "99999999999999999999"] {
            assert_eq!(whole(bad), None, "{bad}");
        }
    }

    fn session(machine: MachineId) -> Session {
        Session {
            id: SessionId::new(),
            engine: Engine::Claude,
            native_id: "n".into(),
            machine,
            cwd: "/w".into(),
            branch: None,
            title: None,
            agent: None,
            workstream: None,
            task: None,
            link_basis: None,
            state: SessionState::Idle,
            status_line: None,
            started: 1,
            last_activity: 1,
            terminal: None,
            parent: None,
            recorded: None,
        }
    }

    fn code(r: &Result<TranscriptPage, ErrorResponse>) -> String {
        match r {
            Ok(page) if page.items.is_empty() && page.at_start => "empty".to_owned(),
            Ok(_) => "page".to_owned(),
            Err(e) => format!("{:?}", e.0.code),
        }
    }

    /// By where the session is: unknown `404`; this machine's, never indexed by the runner, an
    /// empty page at the start; another machine's, or any before a runner is attached, `503`.
    #[test]
    fn whose_transcript_and_where() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(
            Store::open_with(
                tmp.path().join("hub.db"),
                StoreOptions::default(),
                pitcrew_hub_work::projections(),
            )
            .unwrap(),
        );
        let workspace = Workspace {
            id: WorkspaceId::new(),
            name: "Lab".into(),
        };
        let work = Arc::new(WorkService::new(Arc::clone(&store), workspace.clone()));
        let runner = Runner::idle(&tmp.path().join("runner"));
        let parts = runner.parts();
        let (local, remote) = (session(parts.machine), session(MachineId::new()));
        for s in [&local, &remote] {
            store
                .append(&[Event::now(
                    workspace.id,
                    MemberId::new(),
                    EventBody::SessionDiscovered { session: s.clone() },
                )])
                .unwrap();
        }

        let attached = Arc::new(Attached::default());
        let transcripts = Transcripts::new(work, Arc::clone(&attached));
        let page = |s: SessionId| code(&transcripts.page(s, None, 10));
        assert_eq!(page(SessionId::new()), "NotFound");
        assert_eq!(page(local.id), "Unavailable", "no runner yet");
        attached.set(parts);
        assert_eq!(page(SessionId::new()), "NotFound");
        assert_eq!(page(local.id), "empty");
        assert_eq!(page(remote.id), "Unavailable");
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(runner.stop(Duration::from_secs(10)));
    }
}
