//! `GET /v1/host/info` as it is now: roles `["hub", "runner"]` while the runner runs, else
//! `["hub"]`; while it runs, capabilities `tmux` when its terminals run in tmux or `pty` when they
//! run in pitcrew-ptyd, and `watch` when it watches at least one home (in that order).
//! The hub always advertises `scan`; without a runner, its scan route returns conflict.
//!
//! `pitcrew_api::router` answers this route with the `HostInfo` it was built with, but the runner
//! may start after the router is built (once a fresh workspace is set up). So [`answer`], a layer
//! over the whole app, answers `GET` and `HEAD /v1/host/info` itself, from [`HostInfoNow`]; every
//! other request goes on to the router. A stand-in: `pitcrew-api` could take a source of host info
//! instead of a value (see the README, "Not wired yet").

use crate::runner::Attached;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::header::{CONTENT_LENGTH, CONTENT_TYPE};
use axum::http::{HeaderValue, Method, StatusCode};
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
                info.capabilities = runner
                    .runtime
                    .into_iter()
                    .chain(runner.watches.then_some(Capability::Watch))
                    .chain([Capability::Scan])
                    .collect();
            }
            None => {
                info.roles = vec![HostRole::Hub];
                info.capabilities = vec![Capability::Scan];
            }
        }
        info
    }
}

/// Answers `GET` and `HEAD /v1/host/info` from `info`; passes everything else on, so the
/// router's fixed answer is never served.
pub async fn answer(
    State(info): State<Arc<HostInfoNow>>,
    request: Request,
    next: Next,
) -> Response {
    let method = request.method();
    if request.uri().path() == PATH && (method == Method::GET || method == Method::HEAD) {
        return reply(&info.now(), method == Method::HEAD);
    }
    next.run(request).await
}

/// `info` as JSON; for `HEAD`, the same headers (the length the body would have) and no body.
fn reply(info: &HostInfo, head: bool) -> Response {
    match serde_json::to_vec(info) {
        Ok(json) => {
            let headers = [
                (CONTENT_TYPE, HeaderValue::from_static("application/json")),
                (CONTENT_LENGTH, HeaderValue::from(json.len())),
            ];
            let body = if head {
                Body::empty()
            } else {
                Body::from(json)
            };
            (headers, body).into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "cannot write the host info");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::Parts;
    use crate::terminals::NoRuntime;
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
        let parts = |watches, runtime| Parts {
            publication: Arc::new(std::sync::Mutex::new(())),
            machine: MachineId::new(),
            hooks: runner.hooks(),
            commands: runner.commands(&terminals),
            terminals: terminals.clone(),
            transcripts: runner.transcripts(),
            watches,
            runtime,
        };
        for (watches, runtime, expected) in [
            (false, None, vec![]),
            (true, None, vec![Capability::Watch]),
            (false, Some(Capability::Tmux), vec![Capability::Tmux]),
            (
                true,
                Some(Capability::Tmux),
                vec![Capability::Tmux, Capability::Watch],
            ),
            (false, Some(Capability::Pty), vec![Capability::Pty]),
            (
                true,
                Some(Capability::Pty),
                vec![Capability::Pty, Capability::Watch],
            ),
        ] {
            let attached = Arc::new(Attached::default());
            let info = HostInfoNow::new(Arc::clone(&attached));
            let before = info.now();
            assert_eq!(before.name, "pitcrewd");
            assert_eq!(before.protocol, pitcrew_protocol::PROTOCOL_VERSION);
            assert_eq!(before.roles, [HostRole::Hub]);
            assert_eq!(before.capabilities, [Capability::Scan]);
            attached.set(parts(watches, runtime));
            let after = info.now();
            assert_eq!(after.roles, [HostRole::Hub, HostRole::Runner]);
            assert_eq!(
                after.capabilities,
                [expected, vec![Capability::Scan]].concat()
            );
            assert_eq!(after.machine, before.machine);
        }
        runner.stop();
    }

    /// `GET` has the JSON; `HEAD` the same headers, the length included, and no body.
    #[test]
    fn head_has_gets_headers_and_no_body() {
        let info = HostInfoNow::new(Arc::new(Attached::default())).now();
        let json = serde_json::to_vec(&info).unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        for head in [false, true] {
            let response = reply(&info, head);
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(response.headers()[CONTENT_TYPE], "application/json");
            assert_eq!(
                response.headers()[CONTENT_LENGTH],
                json.len().to_string().as_str()
            );
            let body = runtime
                .block_on(axum::body::to_bytes(response.into_body(), usize::MAX))
                .unwrap();
            let expected: &[u8] = if head { &[] } else { &json };
            assert_eq!(&body[..], expected, "head: {head}");
        }
    }
}
