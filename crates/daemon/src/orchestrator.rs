//! The Orchestrator in this process (api-v1.md, "Orchestrator"; hub-work's `orchestrator`): the
//! loop that follows answers from their transcripts ([`follow`]), whether an agent CLI is
//! installed here ([`installed`]), and the rule that an Orchestrator session's transcript and
//! terminal are its asker's alone ([`asker_only`]).
//!
//! - **Following**: about once a second ([`FOLLOW_EVERY`]), on the blocking pool,
//!   `WorkService::follow_orchestrator` reads each answering turn's transcript through the runner
//!   link and ends the turn when its transcript does, or when it passes its bounds. With nothing
//!   answering a look costs a lock and nothing else. The loop holds the work model only while it
//!   looks, so it ends with the daemon.
//! - **Where its sessions run**: each one is a confined run (`crate::confined`): a fresh private
//!   folder under the cache folder, never the state directory, with its prompt in `prompt.md`,
//!   the CLI's confined launch shape, and a reader token minted for that session alone.
//! - **Asker only**: `GET /v1/sessions/{id}/transcript` and `GET /v1/sessions/{id}/terminal` for
//!   an Orchestrator session answer `403` to anyone but the person who asked (`WorkService::
//!   orchestrator_asker`), before and after they clear their conversations.
//! - **Installed** means on the daemon's `PATH`, which the terminals' runtime passes to the CLIs
//!   it starts (with `PATHEXT` on Windows).

use axum::extract::rejection::PathRejection;
use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::{IntoResponse as _, Response};
use pitcrew_auth::ErrorResponse;
use pitcrew_hub_work::WorkService;
use pitcrew_protocol::api::Caller;
use pitcrew_protocol::ids::SessionId;
use pitcrew_protocol::model::Engine;
use std::path::Path;
use std::sync::{Arc, Weak};
use std::time::Duration;

/// How often answering turns are looked at.
pub const FOLLOW_EVERY: Duration = Duration::from_secs(1);

/// Follows the Orchestrator's answers until the daemon stops. See the [module docs](self).
pub async fn follow(work: Weak<WorkService>) {
    loop {
        let Some(looking) = work.upgrade() else {
            return;
        };
        match tokio::task::spawn_blocking(move || looking.follow_orchestrator()).await {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => tracing::warn!(error = %e, "cannot follow the Orchestrator's answers"),
            Err(e) => tracing::warn!(error = %e, "following the Orchestrator's answers failed"),
        }
        tokio::time::sleep(FOLLOW_EVERY).await;
    }
}

/// A route layer for `/v1/sessions/{id}/transcript` and `/v1/sessions/{id}/terminal`: an
/// Orchestrator session's are its asker's alone, so anyone else's request is `403` before the route
/// runs. Other sessions pass.
///
/// The id is the route's own `{id}`, extracted as the handlers extract it (`Path<String>`, which
/// percent-decodes it), never parsed from the raw path: `ses%5F…` names the same session to both.
/// An id the handler could not read either is left to the handler, which finds no session.
pub async fn asker_only(
    State(work): State<Arc<WorkService>>,
    id: Result<axum::extract::Path<String>, PathRejection>,
    request: Request,
    next: Next,
) -> Response {
    let Some(session) = route_session(id) else {
        return next.run(request).await;
    };
    let asker = match tokio::task::spawn_blocking(move || work.orchestrator_asker(&session)).await {
        Ok(Ok(asker)) => asker,
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "cannot tell whether a session is an Orchestrator's");
            return ErrorResponse::new(
                pitcrew_protocol::api::ErrorCode::Internal,
                "The session's asker could not be looked up.",
            )
            .into_response();
        }
        Err(e) => {
            tracing::warn!(error = %e, "cannot tell whether a session is an Orchestrator's");
            return ErrorResponse::new(
                pitcrew_protocol::api::ErrorCode::Internal,
                "The session's asker could not be looked up.",
            )
            .into_response();
        }
    };
    if let Some(asker) = asker {
        let caller = request.extensions().get::<Caller>().map(|c| c.member);
        if caller != Some(asker) {
            return ErrorResponse::forbidden(
                "An Orchestrator session's transcript and terminal are only for the person who \
                 asked.",
            )
            .into_response();
        }
    }
    next.run(request).await
}

/// The session a `/v1/sessions/{id}/…` route names, read as its handler reads it: the decoded
/// `{id}`, then parsed. `None` when the handler would find none either.
#[must_use]
pub fn route_session(id: Result<axum::extract::Path<String>, PathRejection>) -> Option<SessionId> {
    id.ok().and_then(|axum::extract::Path(id)| id.parse().ok())
}

/// The program each engine runs, as the runner starts it.
fn program(engine: Engine) -> Option<&'static str> {
    match engine {
        Engine::Claude => Some("claude"),
        Engine::Codex => Some("codex"),
        Engine::OpenCode => Some("opencode"),
        _ => None,
    }
}

/// Whether `engine`'s CLI is on this daemon's `PATH`.
#[must_use]
pub fn installed(engine: Engine) -> bool {
    let (Some(program), Some(path)) = (program(engine), std::env::var_os("PATH")) else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| on(&dir, program))
}

/// Whether `dir` holds `program`, as something this user may run.
fn on(dir: &Path, program: &str) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::metadata(dir.join(program))
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        let extensions = std::env::var("PATHEXT").unwrap_or_else(|_| ".EXE;.CMD;.BAT".into());
        extensions
            .split(';')
            .filter(|e| !e.is_empty())
            .any(|e| dir.join(format!("{program}{e}")).is_file())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use axum::Router;
    use axum::body::Body;
    use axum::routing::get;
    use pitcrew_protocol::api::TokenScope;
    use pitcrew_protocol::events::{Event, EventBody};
    use pitcrew_protocol::ids::{MachineId, MemberId, WorkspaceId};
    use pitcrew_protocol::model::{Member, MemberKind, Session, SessionState, Workspace};
    use tower::ServiceExt as _;

    /// The forms `session`'s id takes in a request's path: as shown and bare, and percent-encoded
    /// (its `_`, a character of its ULID, either case of hex), which a parser of the raw path
    /// would not read but the route's `{id}` decodes.
    pub(crate) fn path_forms(session: SessionId) -> Vec<String> {
        let ulid = session.0.to_string();
        let (first, last) = (ulid.as_bytes()[0], ulid.as_bytes()[25]);
        vec![
            format!("ses_{ulid}"),
            ulid.clone(),
            format!("ses%5F{ulid}"),
            format!("ses%5f{ulid}"),
            format!("ses_%{first:02X}{}", &ulid[1..]),
            format!("%{first:02X}{}", &ulid[1..]),
            format!("ses_{}%{last:02x}", &ulid[..25]),
        ]
    }

    /// `member`'s device request for `path` through `app`: its status.
    pub(crate) async fn status(app: &Router, path: &str, member: MemberId) -> u16 {
        let request = Request::builder()
            .uri(path)
            .extension(Caller {
                member,
                scope: TokenScope::Device,
                on_behalf_of: None,
            })
            .body(Body::empty())
            .unwrap();
        app.clone()
            .oneshot(request)
            .await
            .unwrap()
            .status()
            .as_u16()
    }

    fn session(parent: Option<SessionId>) -> Session {
        Session {
            id: SessionId::new(),
            engine: Engine::Claude,
            native_id: "synthetic-native".into(),
            machine: MachineId::new(),
            cwd: "/w".into(),
            branch: None,
            title: Some("Synthetic first prompt".into()),
            agent: None,
            workstream: None,
            task: None,
            link_basis: None,
            state: SessionState::Working,
            status_line: None,
            started: 1,
            last_activity: 1,
            terminal: None,
            parent,
        }
    }

    /// An Orchestrator session's transcript is `403` to anyone but its asker, however its id is
    /// written in the path: the layer reads the id the route reads. A sub-agent of it (its own
    /// session, with it as parent) is its asker's too; other sessions pass.
    #[tokio::test]
    async fn the_asker_only_layer_reads_the_id_the_route_reads() {
        let tmp = tempfile::tempdir().unwrap();
        let person = |handle: &str| Member {
            id: MemberId::new(),
            kind: MemberKind::Human,
            handle: handle.into(),
            name: handle.trim_start_matches('@').into(),
            owner: None,
            persona: None,
        };
        let (sam, ana) = (person("@sam"), person("@ana"));
        let asked = SessionId::new();
        let sub_agent = session(Some(asked));
        let other = session(None);
        let file = tmp.path().join("orchestrator.json");
        std::fs::write(
            &file,
            serde_json::json!({
                "version": 1,
                "people": [{"member": sam.id, "sessions": [asked]}],
            })
            .to_string(),
        )
        .unwrap();
        let store = Arc::new(
            pitcrew_store::Store::open_with(
                tmp.path().join("hub.db"),
                pitcrew_store::StoreOptions::default(),
                pitcrew_hub_work::projections(),
            )
            .unwrap(),
        );
        let workspace = Workspace {
            id: WorkspaceId::new(),
            name: "Lab".into(),
        };
        let events: Vec<Event> = [
            EventBody::MemberAdded {
                member: sam.clone(),
            },
            EventBody::MemberAdded {
                member: ana.clone(),
            },
            EventBody::SessionDiscovered {
                session: sub_agent.clone(),
            },
            EventBody::SessionDiscovered {
                session: other.clone(),
            },
        ]
        .into_iter()
        .map(|b| Event::now(workspace.id, sam.id, b))
        .collect();
        store.append(&events).unwrap();
        let work = Arc::new(
            WorkService::new(store, workspace)
                .with_orchestrator_file(file)
                .unwrap(),
        );
        let app = Router::new()
            .route("/v1/sessions/{id}/transcript", get(|| async { "served" }))
            .route_layer(axum::middleware::from_fn_with_state(
                Arc::clone(&work),
                asker_only,
            ));
        for session in [asked, sub_agent.id] {
            for form in path_forms(session) {
                let path = format!("/v1/sessions/{form}/transcript");
                assert_eq!(status(&app, &path, ana.id).await, 403, "{path}");
                assert_eq!(status(&app, &path, sam.id).await, 200, "{path}");
            }
        }
        for form in path_forms(other.id) {
            let path = format!("/v1/sessions/{form}/transcript");
            assert_eq!(status(&app, &path, ana.id).await, 200, "{path}");
        }
        // An id the route cannot read either is left to the route.
        assert_eq!(
            status(&app, "/v1/sessions/ses%5Fnot-an-id/transcript", ana.id).await,
            200
        );
    }

    #[test]
    fn installed_means_on_the_path() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(!on(tmp.path(), "claude"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let file = tmp.path().join("claude");
            std::fs::write(&file, "#!/bin/sh\n").unwrap();
            assert!(!on(tmp.path(), "claude"), "not runnable");
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
            assert!(on(tmp.path(), "claude"));
        }
    }
}
