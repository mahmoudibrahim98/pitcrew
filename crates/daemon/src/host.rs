//! `GET /v1/host/info` as it is now: roles `["hub", "runner"]` while the runner runs, else
//! `["hub"]`; capabilities `["watch"]` while it watches at least one home, else `[]`.
//!
//! `pitcrew_api::router` answers this route with the `HostInfo` it was built with, but the runner
//! may start after the router is built (once a fresh workspace is set up). So [`answer`], a layer
//! over the whole app, answers `GET /v1/host/info` itself, from [`HostInfoNow`]; every other
//! request (`HEAD` included) goes on to the router. A stand-in: `pitcrew-api` could take a source
//! of host info instead of a value (see the README, "Not wired yet").

use crate::runner::Attached;
use axum::Json;
use axum::extract::{Request, State};
use axum::http::Method;
use axum::middleware::Next;
use axum::response::{IntoResponse as _, Response};
use pitcrew_protocol::api::{HostInfo, HostRole};
use pitcrew_protocol::runner::Capability;
use std::sync::Arc;

/// The route.
const PATH: &str = "/v1/host/info";

/// This daemon's host info, with the roles and capabilities of the moment.
#[derive(Debug)]
pub struct HostInfoNow {
    /// The version, protocol range and machine, read once at start.
    base: HostInfo,
    runner: Arc<Attached>,
}

impl HostInfoNow {
    /// This process's host info, with `runner` once it runs.
    pub fn new(runner: Arc<Attached>) -> Self {
        Self {
            base: pitcrew_api::local_host_info(env!("CARGO_PKG_VERSION"), Vec::new(), Vec::new()),
            runner,
        }
    }

    /// The host info now.
    pub fn now(&self) -> HostInfo {
        let mut info = self.base.clone();
        match self.runner.get() {
            Some(runner) => {
                info.roles = vec![HostRole::Hub, HostRole::Runner];
                info.capabilities = if runner.watches {
                    vec![Capability::Watch]
                } else {
                    Vec::new()
                };
            }
            None => {
                info.roles = vec![HostRole::Hub];
                info.capabilities = Vec::new();
            }
        }
        info
    }
}

/// Answers `GET /v1/host/info` from `info`; passes everything else on.
pub async fn answer(
    State(info): State<Arc<HostInfoNow>>,
    request: Request,
    next: Next,
) -> Response {
    if request.method() == Method::GET && request.uri().path() == PATH {
        return Json(info.now()).into_response();
    }
    next.run(request).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::Parts;
    use crate::terminals::NoRuntime;
    use crate::transcripts::Found;
    use pitcrew_protocol::events::Event;
    use pitcrew_protocol::ids::{MachineId, MemberId, WorkspaceId};
    use pitcrew_runner::{EventSink, RunnerConfig, SinkError};

    #[derive(Debug)]
    struct Nowhere;
    impl EventSink for Nowhere {
        fn accept(&self, _events: &[Event]) -> Result<(), SinkError> {
            Ok(())
        }
    }

    /// The roles and capabilities are read at each answer: a runner attached later shows.
    #[test]
    fn the_roles_follow_the_runner() {
        let tmp = tempfile::tempdir().unwrap();
        let runner = pitcrew_runner::start(
            RunnerConfig::new(
                WorkspaceId::new(),
                MachineId::new(),
                MemberId::new(),
                tmp.path().join("runner"),
            ),
            Vec::new(),
            Arc::new(Nowhere),
        )
        .unwrap();
        let terminals = runner.terminals(Arc::new(NoRuntime)).unwrap();
        let parts = |watches| Parts {
            machine: MachineId::new(),
            hooks: runner.hooks(),
            terminals: terminals.clone(),
            found: Arc::new(Found::default()),
            watches,
        };
        for watches in [false, true] {
            let attached = Arc::new(Attached::default());
            let info = HostInfoNow::new(Arc::clone(&attached));
            let before = info.now();
            assert_eq!(before.name, "pitcrewd");
            assert_eq!(before.protocol, pitcrew_protocol::PROTOCOL_VERSION);
            assert_eq!(before.roles, [HostRole::Hub]);
            assert!(before.capabilities.is_empty());
            attached.set(parts(watches));
            let after = info.now();
            assert_eq!(after.roles, [HostRole::Hub, HostRole::Runner]);
            let expected = if watches {
                vec![Capability::Watch]
            } else {
                Vec::new()
            };
            assert_eq!(after.capabilities, expected);
            assert_eq!(after.machine, before.machine);
        }
        runner.stop();
    }
}
