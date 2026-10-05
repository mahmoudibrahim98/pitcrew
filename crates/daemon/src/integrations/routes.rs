//! The integrations' routes (api-v1.md, "Integrations"). Device tokens only: the API layer's
//! `device_only` refuses an agent before any of these runs. Bodies are at most 64 KiB, and a body
//! that does not parse is `400` with a fixed message, never one that could echo a secret back.

use super::{Integrations, Refusal};
use axum::extract::rejection::JsonRejection;
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Extension, Json, Router};
use pitcrew_protocol::api::{ApiError, Caller, ErrorCode};
use pitcrew_protocol::ids::IntegrationId;
use pitcrew_protocol::integrations::{NewCredential, NewIntegration};
use std::sync::Arc;

/// The largest body these routes read.
const MAX_BODY: usize = 64 * 1024;

impl IntoResponse for Refusal {
    fn into_response(self) -> Response {
        let status = StatusCode::from_u16(self.code.http_status())
            .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        (
            status,
            Json(ApiError {
                code: self.code,
                message: self.message,
            }),
        )
            .into_response()
    }
}

type Shared = State<Arc<Integrations>>;

/// The routes, over `integrations`.
pub fn routes(integrations: Arc<Integrations>) -> Router {
    Router::new()
        .route("/v1/integrations", get(list).post(add))
        .route("/v1/integrations/{id}", get(one).delete(remove))
        .route("/v1/integrations/{id}/test", post(test))
        .route("/v1/integrations/{id}/sync", post(sync_now))
        .route("/v1/integrations/{id}/credential", put(credential))
        .layer(DefaultBodyLimit::max(MAX_BODY))
        .with_state(integrations)
}

/// An id in the path; anything else names no integration (`404`).
fn id(raw: &str) -> Result<IntegrationId, Refusal> {
    raw.parse()
        .map_err(|_| Refusal::new(ErrorCode::NotFound, "No such integration."))
}

fn body<T>(parsed: Result<Json<T>, JsonRejection>, shape: &str) -> Result<T, Refusal> {
    parsed.map(|Json(value)| value).map_err(|_| {
        Refusal::new(
            ErrorCode::Invalid,
            format!("The body must be JSON shaped as {shape}."),
        )
    })
}

async fn list(State(i): Shared) -> Result<Response, Refusal> {
    Ok(Json(i.list().await?).into_response())
}

async fn one(State(i): Shared, Path(raw): Path<String>) -> Result<Response, Refusal> {
    Ok(Json(i.get(&id(&raw)?).await?).into_response())
}

async fn add(
    State(i): Shared,
    Extension(caller): Extension<Caller>,
    parsed: Result<Json<NewIntegration>, JsonRejection>,
) -> Result<Response, Refusal> {
    let new = body(parsed, "NewIntegration (api-v1.md, \"Integrations\")")?;
    Ok((StatusCode::CREATED, Json(i.add(&caller, new).await?)).into_response())
}

async fn remove(
    State(i): Shared,
    Extension(caller): Extension<Caller>,
    Path(raw): Path<String>,
) -> Result<Response, Refusal> {
    i.remove(&caller, &id(&raw)?)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

async fn test(State(i): Shared, Path(raw): Path<String>) -> Result<Response, Refusal> {
    Ok(Json(i.check(&id(&raw)?).await?).into_response())
}

async fn sync_now(State(i): Shared, Path(raw): Path<String>) -> Result<Response, Refusal> {
    Ok((StatusCode::ACCEPTED, Json(i.sync_now(&id(&raw)?).await?)).into_response())
}

async fn credential(
    State(i): Shared,
    Extension(caller): Extension<Caller>,
    Path(raw): Path<String>,
    parsed: Result<Json<NewCredential>, JsonRejection>,
) -> Result<Response, Refusal> {
    let id = id(&raw)?;
    // The path first: an unknown integration is 404 whatever the body holds.
    i.record(&id)?;
    let new = body(parsed, "{\"secret\": String}")?;
    let integrations = Arc::clone(&i);
    tokio::task::spawn_blocking(move || integrations.set_credential(&caller, &id, &new.secret))
        .await
        .map_err(|_| Refusal::internal("Storing the secret"))??;
    Ok(StatusCode::NO_CONTENT.into_response())
}
