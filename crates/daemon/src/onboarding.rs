//! Device-only exact hook previews. Configurations stay on the hub's own machine.
use axum::{
    Extension, Json, Router,
    extract::{Path, State},
    routing::post,
};
use pitcrew_cli::install::Installation;
use pitcrew_hub_work::{WorkError, WorkService};
use pitcrew_protocol::{
    api::{Caller, ErrorCode, TokenScope},
    ids::{EventId, MachineId, MemberId},
    onboarding::{HooksDiff, InstallHooks},
};
use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{Arc, Mutex, PoisonError},
    time::{Duration, Instant},
};

const TTL: Duration = Duration::from_secs(600);
const MAX_BYTES: usize = 16 * 1024 * 1024;
struct Preview {
    revision: String,
    owner: MemberId,
    machine: MachineId,
    created: Instant,
    plan: Installation,
}
struct Hooks {
    work: Arc<WorkService>,
    previews: Mutex<VecDeque<Preview>>,
}
pub fn routes(work: Arc<WorkService>) -> Router {
    Router::new()
        .route("/v1/machines/{id}/hooks/diff", post(diff))
        .route("/v1/machines/{id}/hooks/install", post(install))
        .with_state(Arc::new(Hooks {
            work,
            previews: Mutex::default(),
        }))
}
fn machine(work: &WorkService, caller: &Caller, id: &str) -> Result<MachineId, WorkError> {
    if caller.scope != TokenScope::Device {
        return Err(WorkError::forbidden(
            "Hook installation needs a device token.",
        ));
    }
    let id: MachineId = id
        .parse()
        .map_err(|_| WorkError::not_found("Unknown machine."))?;
    if !work.machines()?.iter().any(|m| m.id == id) {
        return Err(WorkError::not_found("Unknown machine."));
    }
    if work.hub_machine() != Some(id) {
        return Err(WorkError::new(
            ErrorCode::Unsupported,
            "Hooks on another machine are not supported yet.",
        ));
    }
    Ok(id)
}
fn executable() -> Result<PathBuf, WorkError> {
    let name = if cfg!(windows) {
        "pitcrew.exe"
    } else {
        "pitcrew"
    };
    let sibling = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|p| p.join(name)));
    sibling
        .filter(|p| p.is_file())
        .or_else(|| {
            std::env::var_os("PATH").and_then(|path| {
                std::env::split_paths(&path)
                    .filter(|p| p.is_absolute())
                    .map(|p| p.join(name))
                    .find(|p| p.is_file())
            })
        })
        .ok_or_else(|| {
            WorkError::unavailable(
                "Install pitcrew beside pitcrewd or on its PATH before installing hooks.",
            )
        })
}
// Never pass installer errors to tracing: malformed JSON can contain private settings.
fn installer_error(e: pitcrew_cli::error::Error) -> WorkError {
    match e.kind {
        pitcrew_cli::error::Kind::Conflict => {
            WorkError::conflict("The hook preview is stale or conflicting. Preview again.")
        }
        pitcrew_cli::error::Kind::Invalid => {
            WorkError::invalid("Hook configuration cannot be previewed safely.")
        }
        _ => {
            WorkError::unavailable("Hook installation could not complete. Preview again or retry.")
        }
    }
}
async fn diff(
    State(hooks): State<Arc<Hooks>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
) -> Result<Json<HooksDiff>, WorkError> {
    tokio::task::spawn_blocking(move || {
        let id = machine(&hooks.work, &caller, &id)?;
        let plan = Installation::preview(&|name| std::env::var_os(name), &executable()?)
            .map_err(installer_error)?;
        let bytes = plan.bytes();
        if bytes > MAX_BYTES {
            return Err(WorkError::new(
                ErrorCode::TooLarge,
                "The hook preview is too large.",
            ));
        }
        let revision = EventId::new().to_string();
        let result = HooksDiff {
            revision: revision.clone(),
            files: plan.files().map_err(installer_error)?,
            engines: plan.engines(),
        };
        let mut previews = hooks
            .previews
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        previews.retain(|p| p.created.elapsed() < TTL);
        while previews.len() >= 32
            || previews.iter().map(|p| p.plan.bytes()).sum::<usize>() + bytes > MAX_BYTES
        {
            previews.pop_front();
        }
        previews.push_back(Preview {
            revision,
            owner: caller.member,
            machine: id,
            created: Instant::now(),
            plan,
        });
        Ok(Json(result))
    })
    .await
    .map_err(|_| WorkError::unavailable("Hook preview failed."))?
}
async fn install(
    State(hooks): State<Arc<Hooks>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
    body: Result<Json<InstallHooks>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<serde_json::Value>, WorkError> {
    if caller.scope != TokenScope::Device {
        return Err(WorkError::forbidden(
            "Hook installation needs a device token.",
        ));
    }
    let Json(body) = body.map_err(|_| WorkError::invalid("Supply the hook preview revision."))?;
    tokio::task::spawn_blocking(move || {
        let id = machine(&hooks.work, &caller, &id)?;
        let previews = hooks
            .previews
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let preview = previews
            .iter()
            .find(|p| {
                p.revision == body.revision
                    && p.owner == caller.member
                    && p.machine == id
                    && p.created.elapsed() < TTL
            })
            .ok_or_else(|| {
                WorkError::conflict("The hook preview expired or is unknown. Preview again.")
            })?;
        preview.plan.apply().map_err(installer_error)?;
        Ok(Json(serde_json::json!({"installed": true})))
    })
    .await
    .map_err(|_| WorkError::unavailable("Hook installation failed."))?
}
