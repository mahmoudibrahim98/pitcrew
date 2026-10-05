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
pub async fn asker_only(
    State(work): State<Arc<WorkService>>,
    request: Request,
    next: Next,
) -> Response {
    let asker = request
        .uri()
        .path()
        .strip_prefix("/v1/sessions/")
        .and_then(|rest| rest.split('/').next())
        .and_then(|id| id.parse::<SessionId>().ok())
        .and_then(|id| work.orchestrator_asker(&id));
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
mod tests {
    use super::*;

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
