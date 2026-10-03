//! `GET /v1/sessions/{id}/terminal`: which sessions have a terminal here ([`SessionTerminals`]),
//! and the runtime the runner's terminals use ([`NoRuntime`]).
//!
//! - A session the hub does not know is `404 not_found`.
//! - A session on another machine is `503 unavailable`: this hub reaches no other machine yet.
//! - A session on this machine is the runner's to answer (`RunnerTerminals`): `404` while it has
//!   no terminal (a session PitCrew did not start, or any session without a runtime).
//! - Without a runner (`--no-runner`, or before a fresh workspace is set up), every known session
//!   is `503 unavailable`.
//!
//! **The runtime** is chosen at start (`crate::runtime`): tmux where it is usable, else
//! pitcrew-ptyd where it is installed next to `pitcrewd`, else [`NoRuntime`], which starts
//! nothing and reaches nothing, so no session has a terminal here.

use crate::runner::Attached;
use pitcrew_api::{Attachment, TerminalError, Terminals};
use pitcrew_hub_work::WorkService;
use pitcrew_interfaces::runtime::{
    OutputChunk, Runtime, RuntimeError, RuntimeKind, Screen, StartSpec, TerminalInfo,
};
use pitcrew_protocol::api::ErrorCode;
use pitcrew_protocol::ids::{SessionId, TerminalId};
use pitcrew_protocol::runner::Key;
use std::sync::Arc;

/// Sessions' terminals: the hub decides whose they are, the runner finds them.
#[derive(Debug)]
pub struct SessionTerminals {
    work: Arc<WorkService>,
    /// The runner, once it runs: its machine and its terminals.
    runner: Arc<Attached>,
}

impl SessionTerminals {
    /// Terminals of the sessions `work` knows, found by the runner on its machine once it runs.
    #[must_use]
    pub fn new(work: Arc<WorkService>, runner: Arc<Attached>) -> Self {
        Self { work, runner }
    }
}

impl Terminals for SessionTerminals {
    fn attach(&self, session: SessionId) -> Result<Arc<dyn Attachment>, TerminalError> {
        let found = match self.work.session(&session) {
            Ok(found) => found,
            Err(e) if e.code() == ErrorCode::NotFound => {
                return Err(TerminalError::NotFound(format!("No session {session}.")));
            }
            Err(e) => {
                tracing::error!(error = %e, %session, "cannot look up a session for its terminal");
                return Err(TerminalError::Failed(
                    "The session could not be looked up.".to_owned(),
                ));
            }
        };
        match self.runner.get() {
            None => Err(TerminalError::Unavailable(format!(
                "Session {session} has no terminal here: no runner is attached to this hub."
            ))),
            Some(runner) if found.machine != runner.machine => {
                Err(TerminalError::Unavailable(format!(
                    "Session {session} runs on another machine, which this hub cannot reach yet."
                )))
            }
            Some(runner) => runner.terminals.attach(session),
        }
    }
}

/// The runtime where neither tmux nor pitcrew-ptyd can be used (see the [module docs](self)). It
/// owns no terminal, as the trait means it: it lists none, every terminal is `NotFound`, and
/// starting one is `Unavailable`.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoRuntime;

impl Runtime for NoRuntime {
    /// Nothing reads it but debug output; there is no third kind to name.
    fn kind(&self) -> RuntimeKind {
        RuntimeKind::Pty
    }

    fn start(&self, _spec: &StartSpec) -> Result<TerminalInfo, RuntimeError> {
        Err(RuntimeError::Unavailable(
            "this machine has no terminal runtime for pitcrewd (tmux 3.2 or newer, or \
             pitcrew-ptyd next to pitcrewd, is needed; the daemon's log says why neither is used)"
                .to_owned(),
        ))
    }

    fn write(&self, id: TerminalId, _bytes: &[u8]) -> Result<(), RuntimeError> {
        Err(RuntimeError::NotFound(id))
    }

    fn send_keys(&self, id: TerminalId, _keys: &[Key]) -> Result<(), RuntimeError> {
        Err(RuntimeError::NotFound(id))
    }

    fn resize(&self, id: TerminalId, _cols: u16, _rows: u16) -> Result<(), RuntimeError> {
        Err(RuntimeError::NotFound(id))
    }

    fn screen(&self, id: TerminalId) -> Result<Screen, RuntimeError> {
        Err(RuntimeError::NotFound(id))
    }

    fn read_output(
        &self,
        id: TerminalId,
        _from: u64,
        _max: usize,
    ) -> Result<OutputChunk, RuntimeError> {
        Err(RuntimeError::NotFound(id))
    }

    fn info(&self, id: TerminalId) -> Result<TerminalInfo, RuntimeError> {
        Err(RuntimeError::NotFound(id))
    }

    fn list(&self) -> Result<Vec<TerminalInfo>, RuntimeError> {
        Ok(Vec::new())
    }

    fn kill(&self, id: TerminalId) -> Result<(), RuntimeError> {
        Err(RuntimeError::NotFound(id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::Parts;
    use pitcrew_protocol::events::{Event, EventBody};
    use pitcrew_protocol::ids::{MachineId, MemberId, WorkspaceId};
    use pitcrew_protocol::model::{Engine, Session, SessionState, Workspace};
    use pitcrew_runner::{EventSink, RunnerConfig, SinkError};
    use pitcrew_store::{Store, StoreOptions};

    #[derive(Debug)]
    struct Nowhere;
    impl EventSink for Nowhere {
        fn accept(&self, _events: &[Event]) -> Result<(), SinkError> {
            Ok(())
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
        }
    }

    fn code(r: Result<Arc<dyn Attachment>, TerminalError>) -> &'static str {
        match r {
            Ok(_) => "attached",
            Err(TerminalError::NotFound(_)) => "not_found",
            Err(TerminalError::Unavailable(_)) => "unavailable",
            Err(_) => "other",
        }
    }

    #[test]
    fn whose_terminal_and_where() {
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
        let (here, there) = (MachineId::new(), MachineId::new());
        let (local, remote) = (session(here), session(there));
        for s in [&local, &remote] {
            store
                .append(&[Event::now(
                    workspace.id,
                    MemberId::new(),
                    EventBody::SessionDiscovered { session: s.clone() },
                )])
                .unwrap();
        }
        let runner = pitcrew_runner::start(
            RunnerConfig::new(
                workspace.id,
                here,
                MemberId::new(),
                tmp.path().join("runner"),
            ),
            Vec::new(),
            Arc::new(Nowhere),
        )
        .unwrap();
        let terminals = runner.terminals(Arc::new(NoRuntime)).unwrap();
        let parts = Parts {
            machine: here,
            hooks: runner.hooks(),
            commands: runner.commands(&terminals),
            terminals,
            transcripts: runner.transcripts(),
            watches: false,
            runtime: None,
        };

        let with =
            SessionTerminals::new(Arc::clone(&work), Arc::new(Attached::with(parts.clone())));
        assert_eq!(code(with.attach(SessionId::new())), "not_found");
        assert_eq!(code(with.attach(remote.id)), "unavailable");
        // This machine's session has no terminal.
        assert_eq!(code(with.attach(local.id)), "not_found");

        // Without a runner yet, then with one attached later (a hub set up while it runs).
        let attached = Arc::new(Attached::default());
        let without = SessionTerminals::new(work, Arc::clone(&attached));
        assert_eq!(code(without.attach(SessionId::new())), "not_found");
        assert_eq!(code(without.attach(local.id)), "unavailable");
        assert_eq!(code(without.attach(remote.id)), "unavailable");
        attached.set(parts);
        assert_eq!(code(without.attach(local.id)), "not_found");
        assert_eq!(code(without.attach(remote.id)), "unavailable");
        runner.stop();
    }

    #[test]
    fn no_runtime_owns_nothing() {
        let rt = NoRuntime;
        assert!(rt.list().unwrap().is_empty());
        assert!(matches!(
            rt.info(TerminalId::new()),
            Err(RuntimeError::NotFound(_))
        ));
        let spec = StartSpec {
            program: "claude".into(),
            args: Vec::new(),
            cwd: "/w".into(),
            env: Vec::new(),
            name: "x".into(),
            cols: 80,
            rows: 24,
        };
        assert!(matches!(rt.start(&spec), Err(RuntimeError::Unavailable(_))));
    }
}
