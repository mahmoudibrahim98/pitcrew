//! The hub before the runner link (stream D) is wired in: it knows its sessions but cannot reach
//! a runner, so `GET /v1/sessions/{id}/terminal` is `503 unavailable` for a known session and
//! `404 not_found` for an unknown one, as the contract says ([`NoRunner`]).
//!
//! Dispatching needs no stand-in: the daemon gives hub-work no dispatcher, so
//! `POST /v1/tasks/{id}/dispatch` answers `503 unavailable` before recording anything.

use pitcrew_api::{Attachment, TerminalError, Terminals};
use pitcrew_hub_work::WorkService;
use pitcrew_protocol::api::ErrorCode;
use pitcrew_protocol::ids::SessionId;
use std::sync::Arc;

/// Terminals while no runner is attached.
#[derive(Debug)]
pub struct NoRunner {
    work: Arc<WorkService>,
}

impl NoRunner {
    /// Looks sessions up in `work`.
    #[must_use]
    pub fn new(work: Arc<WorkService>) -> Self {
        Self { work }
    }
}

impl Terminals for NoRunner {
    fn attach(&self, session: SessionId) -> Result<Arc<dyn Attachment>, TerminalError> {
        match self.work.session(&session) {
            Ok(_) => Err(TerminalError::Unavailable(format!(
                "Session {session} has no terminal here: no runner is attached to this hub yet."
            ))),
            Err(e) if e.code() == ErrorCode::NotFound => {
                Err(TerminalError::NotFound(format!("No session {session}.")))
            }
            Err(e) => {
                tracing::error!(error = %e, %session, "cannot look up a session for its terminal");
                Err(TerminalError::Failed(
                    "The session could not be looked up.".to_owned(),
                ))
            }
        }
    }
}
