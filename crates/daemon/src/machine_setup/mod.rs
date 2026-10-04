//! Machine setup (api-v1.md, "Machine setup"): onboarding's machine steps on the hub's own
//! machine. Device routes (`RouterParts::device`): an agent token gets `403`.
//!
//! | Route | What |
//! |---|---|
//! | `GET /v1/machines/{id}/check[?row=<item>]` | [`MachineCheck`]: each agent CLI and its version, tmux, git, gh, free disk, SLURM where it is there ([`check`]) |
//! | `GET /v1/machines/{id}/agents` | [`AgentAccount`]s, as each CLI's own status command reports them ([`accounts`]) |
//! | `POST /v1/machines/{id}/agents/{engine}/sign-in` | Starts the CLI's own login in a terminal ([`sign_in`]): `201` [`SignIn`], or `200` with the one already running |
//! | `GET /v1/machines/{id}/agents/{engine}/sign-in` | That sign-in, and whether its login still runs; `404` when there is none |
//!
//! - **Which machine:** only the hub's own (the workspace's first local machine, as the scan). An
//!   unknown or malformed id is `404`; another machine of the workspace is `409`: it is checked
//!   through its own hub (a remote workspace's, through the desktop gateway), or over SSH before
//!   PitCrew runs there (`pitcrew_remote::check`).
//! - **Nothing is installed or fixed here.** A row's fix is for the client: opening the tool's
//!   install page from its own table. No route runs a package manager, `sudo`, or anything but
//!   the version and status commands named in [`check`] and [`accounts`], and the logins in
//!   [`sign_in`].
//! - **Never a token.** Accounts come from what each CLI prints; no credential file is opened. The
//!   log records counts and outcomes, never a command's output or an account.

mod accounts;
mod check;
mod disk;
mod sign_in;
mod tools;

pub use sign_in::{SignInTerminals, SignIns};

use crate::runtime::TerminalRuntime;
use axum::Json;
use axum::Router;
use axum::body::Bytes;
use axum::extract::rejection::{PathRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use check::CheckEnv;
use pitcrew_auth::ErrorResponse;
use pitcrew_hub_work::WorkService;
use pitcrew_protocol::api::ErrorCode;
use pitcrew_protocol::ids::MachineId;
#[cfg(doc)]
use pitcrew_protocol::machine_setup::{AgentAccount, MachineCheck, SignIn};
use pitcrew_protocol::machine_setup::{MachineCheckItem, SignInMethod, StartSignIn};
use pitcrew_protocol::model::{Engine, Machine, MachineKind};
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tools::Tools;

/// How long a route waits to learn the workspace's machines.
const LOOKUP_TIMEOUT: Duration = Duration::from_secs(15);

/// The routes' state.
#[derive(Debug)]
pub struct MachineSetup {
    work: Arc<WorkService>,
    check: CheckEnv,
    tools: Tools,
    sign_ins: Arc<SignIns>,
}

impl MachineSetup {
    /// Machine setup for the hub `work` serves, whose state directory is `state` and whose
    /// terminals run in `runtime`. Tools are found on the daemon's `PATH`; logins run in the
    /// person's home folder (the state directory where there is none).
    pub fn new(work: Arc<WorkService>, state: PathBuf, runtime: &TerminalRuntime) -> Self {
        let tools = Tools::from_env();
        let home = directories::BaseDirs::new()
            .map(|dirs| dirs.home_dir().to_path_buf())
            .filter(|home| home.is_absolute() && home.is_dir())
            .unwrap_or_else(|| state.clone());
        let sign_ins = Arc::new(SignIns::new(runtime.runtime(), tools.clone(), home));
        Self {
            work,
            check: CheckEnv {
                tools: tools.clone(),
                state,
                runtime: runtime.capability(),
            },
            tools,
            sign_ins,
        }
    }

    /// The sign-in terminals, for the terminals route ([`SignInTerminals`]).
    pub fn sign_ins(&self) -> Arc<SignIns> {
        Arc::clone(&self.sign_ins)
    }

    /// Starts removing expired sign-in terminals (and an earlier daemon's). In a Tokio runtime.
    pub fn start(&self) {
        SignIns::start_sweeping(&self.sign_ins);
    }
}

/// The routes. Mount them as **device** routes (`RouterParts::device`).
pub fn routes(setup: Arc<MachineSetup>) -> Router {
    Router::new()
        .route("/v1/machines/{id}/check", get(check_machine))
        .route("/v1/machines/{id}/agents", get(agents))
        .route(
            "/v1/machines/{id}/agents/{engine}/sign-in",
            get(sign_in_status).post(start_sign_in),
        )
        .with_state(setup)
}

/// The hub's own machine: the workspace's first local one, as `serve` picks it.
fn own_machine(machines: &[Machine]) -> Option<&Machine> {
    machines.iter().find(|m| m.kind == MachineKind::Local)
}

fn internal() -> ErrorResponse {
    ErrorResponse::new(
        ErrorCode::Internal,
        "The workspace's machines could not be read.",
    )
}

/// `id`, if it is the hub's own machine; otherwise why not.
async fn hub_machine(setup: &MachineSetup, id: &str) -> Result<Machine, ErrorResponse> {
    let no_machine = || ErrorResponse::not_found(format!("No machine {id}."));
    let machine: MachineId = id.parse().map_err(|_| no_machine())?;
    let work = Arc::clone(&setup.work);
    let lookup = tokio::task::spawn_blocking(move || work.machines());
    let machines = match tokio::time::timeout(LOOKUP_TIMEOUT, lookup).await {
        Ok(Ok(Ok(machines))) => machines,
        Ok(Ok(Err(e))) => {
            tracing::error!(error = %e, "cannot list the machines for machine setup");
            return Err(internal());
        }
        Ok(Err(e)) => {
            tracing::error!(error = %e, "listing the machines for machine setup failed");
            return Err(internal());
        }
        Err(_) => {
            return Err(ErrorResponse::new(
                ErrorCode::Unavailable,
                "The workspace's machines took too long to read.",
            ));
        }
    };
    let Some(target) = machines.iter().find(|m| m.id == machine) else {
        return Err(no_machine());
    };
    match own_machine(&machines) {
        Some(own) if own.id == machine => Ok(target.clone()),
        _ => Err(ErrorResponse::new(
            ErrorCode::Conflict,
            format!(
                "{} is not this hub's own machine: set it up through its own hub, or while \
                 connecting it.",
                target.name
            ),
        )),
    }
}

fn plain_id(id: Result<Path<String>, PathRejection>) -> Result<String, ErrorResponse> {
    id.map(|Path(id)| id)
        .map_err(|_| ErrorResponse::new(ErrorCode::Invalid, "The machine id must be plain text."))
}

#[derive(Debug, Deserialize)]
struct CheckQuery {
    row: Option<String>,
}

async fn check_machine(
    State(setup): State<Arc<MachineSetup>>,
    id: Result<Path<String>, PathRejection>,
    query: Result<Query<CheckQuery>, QueryRejection>,
) -> Response {
    let answer = async {
        let id = plain_id(id)?;
        let Query(query) =
            query.map_err(|_| ErrorResponse::new(ErrorCode::Invalid, "The query is malformed."))?;
        let only = match query.row.as_deref() {
            None => None,
            Some(name) => Some(MachineCheckItem::parse(name).ok_or_else(|| {
                ErrorResponse::new(ErrorCode::Invalid, format!("No check row {name:?}."))
            })?),
        };
        hub_machine(&setup, &id).await?;
        let checked = check::check(&setup.check, only).await;
        tracing::info!(
            rows = checked.rows.len(),
            ok = checked
                .rows
                .iter()
                .filter(|r| r.status == pitcrew_protocol::machine_setup::MachineCheckStatus::Ok)
                .count(),
            "checked this machine"
        );
        Ok::<_, ErrorResponse>(checked)
    };
    match answer.await {
        Ok(checked) => Json(checked).into_response(),
        Err(refused) => refused.into_response(),
    }
}

async fn agents(
    State(setup): State<Arc<MachineSetup>>,
    id: Result<Path<String>, PathRejection>,
) -> Response {
    let answer = async {
        let id = plain_id(id)?;
        hub_machine(&setup, &id).await?;
        let found = accounts::accounts(&setup.tools).await;
        tracing::info!(
            signed_in = found.iter().filter(|a| a.signed_in == Some(true)).count(),
            unknown = found.iter().filter(|a| a.signed_in.is_none()).count(),
            "read the agents' accounts from their CLIs"
        );
        Ok::<_, ErrorResponse>(found)
    };
    match answer.await {
        Ok(found) => Json(found).into_response(),
        Err(refused) => refused.into_response(),
    }
}

/// The machine and the engine named in a sign-in's path.
async fn sign_in_target(
    setup: &MachineSetup,
    path: Result<Path<(String, String)>, PathRejection>,
) -> Result<Engine, ErrorResponse> {
    let Path((id, engine)) =
        path.map_err(|_| ErrorResponse::new(ErrorCode::Invalid, "The path must be plain text."))?;
    let parsed: Option<Engine> = serde_json::from_value(serde_json::Value::String(engine.clone()))
        .ok()
        .filter(|e| accounts::program(*e).is_some());
    let engine =
        parsed.ok_or_else(|| ErrorResponse::not_found(format!("No agent CLI {engine:?}.")))?;
    hub_machine(setup, &id).await?;
    Ok(engine)
}

async fn sign_in_status(
    State(setup): State<Arc<MachineSetup>>,
    path: Result<Path<(String, String)>, PathRejection>,
) -> Response {
    let answer = async {
        let engine = sign_in_target(&setup, path).await?;
        let sign_ins = setup.sign_ins();
        let found = tokio::task::spawn_blocking(move || sign_ins.get(engine))
            .await
            .map_err(|_| {
                ErrorResponse::new(ErrorCode::Internal, "The sign-in could not be read.")
            })?;
        found.ok_or_else(|| {
            ErrorResponse::not_found(format!(
                "No sign-in to {} is open here.",
                accounts::label(engine)
            ))
        })
    };
    match answer.await {
        Ok(found) => Json(found).into_response(),
        Err(refused) => refused.into_response(),
    }
}

async fn start_sign_in(
    State(setup): State<Arc<MachineSetup>>,
    path: Result<Path<(String, String)>, PathRejection>,
    body: Bytes,
) -> Response {
    let answer = async {
        let start: StartSignIn = if body.iter().all(u8::is_ascii_whitespace) {
            StartSignIn::default()
        } else {
            serde_json::from_slice(&body).map_err(|_| {
                ErrorResponse::new(
                    ErrorCode::Invalid,
                    "The body must be {} or {\"method\": \"browser\" | \"device_code\"}.",
                )
            })?
        };
        let engine = sign_in_target(&setup, path).await?;
        let method = start.method.unwrap_or(SignInMethod::Browser);
        let sign_ins = setup.sign_ins();
        let started = tokio::task::spawn_blocking(move || sign_ins.start(engine, method))
            .await
            .map_err(|_| ErrorResponse::new(ErrorCode::Internal, "The sign-in could not start."))?;
        started.map_err(|e| match e {
            sign_in::StartError::NotInstalled(m) => ErrorResponse::new(ErrorCode::Conflict, m),
            sign_in::StartError::Method(m) => ErrorResponse::new(ErrorCode::Invalid, m),
            sign_in::StartError::Unavailable(m) => {
                tracing::warn!(why = %m, "a sign-in terminal could not start");
                ErrorResponse::new(ErrorCode::Unavailable, m)
            }
            sign_in::StartError::Failed(m) => {
                tracing::warn!(why = %m, "a sign-in terminal could not start");
                ErrorResponse::new(ErrorCode::Internal, m)
            }
        })
    };
    match answer.await {
        Ok((found, true)) => (StatusCode::CREATED, Json(found)).into_response(),
        Ok((found, false)) => Json(found).into_response(),
        Err(refused) => refused.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pitcrew_protocol::model::Liveness;

    fn machine(name: &str, kind: MachineKind) -> Machine {
        Machine {
            id: MachineId::new(),
            name: name.into(),
            kind,
            info: None,
            liveness: Liveness::Live,
        }
    }

    #[test]
    fn only_the_hubs_first_local_machine_is_its_own() {
        let cluster = machine("a SLURM cluster", MachineKind::Ssh);
        let laptop = machine("This laptop", MachineKind::Local);
        let machines = [cluster, laptop.clone()];
        assert_eq!(own_machine(&machines).map(|m| m.id), Some(laptop.id));
        assert!(own_machine(&[]).is_none());
    }
}
