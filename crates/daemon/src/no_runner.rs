//! The hub before the runner link (stream D) is wired in: it knows its sessions but cannot reach
//! a runner, so
//! - `GET /v1/sessions/{id}/terminal` is `503 unavailable` for a known session and `404 not_found`
//!   for an unknown one, as the contract says ([`NoRunner`]);
//! - `POST /v1/tasks/{id}/dispatch` records the dispatch, then fails to start it: hub-work finishes
//!   the dispatch as `failed`, ends its session, and answers `503 unavailable` ([`NoDispatcher`]).

use pitcrew_api::{Attachment, TerminalError, Terminals};
use pitcrew_hub_work::{DispatchError, DispatchRequest, Dispatcher, WorkService};
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

/// Dispatches while no runner is attached: every start is unavailable, so hub-work records the
/// dispatch as failed and answers `503`.
#[derive(Debug, Default)]
pub struct NoDispatcher;

impl Dispatcher for NoDispatcher {
    fn start(&self, _request: &DispatchRequest) -> Result<(), DispatchError> {
        Err(DispatchError::Unavailable(
            "no runner is attached to this hub yet".to_owned(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pitcrew_hub_work::{NewDispatch, TaskRef};
    use pitcrew_protocol::api::{Caller, TokenScope};
    use pitcrew_protocol::events::EventBody;
    use pitcrew_protocol::model::{DispatchOutcome, MemberKind};
    use pitcrew_store::{Store, StoreOptions};

    #[test]
    fn a_dispatch_is_recorded_then_fails_as_unavailable() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open_with(
            tmp.path().join("hub.db"),
            StoreOptions::default(),
            pitcrew_hub_work::projections(),
        )
        .unwrap();
        let demo = pitcrew_fixtures::demo_workspace().unwrap();
        let laptop = demo.machines[0].id;
        let work = WorkService::new(Arc::new(store), demo.workspace.clone())
            .with_dispatcher(Arc::new(NoDispatcher))
            .with_hub_machine(laptop);
        work.seed(&demo).unwrap();
        let person = demo
            .members
            .iter()
            .find(|m| m.kind == MemberKind::Human)
            .unwrap();
        let agent = demo
            .members
            .iter()
            .find(|m| m.kind == MemberKind::Agent && m.owner == Some(person.id))
            .unwrap();
        let caller = Caller {
            member: person.id,
            scope: TokenScope::Device,
            on_behalf_of: None,
        };
        let before = work.store().latest_rev().unwrap();

        let refused = work
            .dispatch_task(
                &caller,
                &TaskRef::parse("PAP-5").unwrap(),
                NewDispatch {
                    agent: agent.id,
                    brief: None,
                    machine: Some(laptop),
                },
            )
            .unwrap_err();
        assert_eq!(refused.code(), ErrorCode::Unavailable, "{refused}");

        let appended = work.store().since(before, 10).unwrap();
        let finished = appended
            .iter()
            .find_map(|e| match &e.event.body {
                EventBody::DispatchFinished { outcome, .. } => Some(*outcome),
                _ => None,
            })
            .expect("dispatch_finished");
        assert_eq!(finished, DispatchOutcome::Failed);
        assert!(
            appended
                .iter()
                .any(|e| matches!(e.event.body, EventBody::SessionEnded { .. }))
        );
    }
}
