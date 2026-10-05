//! The integrations' routes (api-v1.md, "Integrations" and "Outward writes"). Device tokens only:
//! the API layer's `device_only` refuses an agent before any of these runs. Bodies are at most
//! 64 KiB, and a body that does not parse is `400` with a fixed message, never one that could
//! echo a secret back.

use super::{Integrations, Refusal};
use axum::extract::rejection::JsonRejection;
use axum::extract::{DefaultBodyLimit, Path, RawQuery, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Extension, Json, Router};
use pitcrew_hub_work::WriteFilter;
use pitcrew_protocol::api::{ApiError, Caller, ErrorCode};
use pitcrew_protocol::ids::{AskId, IntegrationId, TaskId};
use pitcrew_protocol::integrations::{NewCredential, NewIntegration};
use pitcrew_protocol::writes::{NewWrite, WriteState};
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
        .route("/v1/writes", get(list_writes).post(request_write))
        .route("/v1/writes/{id}", get(one_write))
        .route("/v1/writes/{id}/retry", post(retry_write))
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

/// A write's id in the path: its approval ask's. Anything else names no write (`404`).
fn write_id(raw: &str) -> Result<AskId, Refusal> {
    raw.parse()
        .map_err(|_| Refusal::new(ErrorCode::NotFound, "No such write."))
}

/// `?task=&state=` (`state` may repeat; empty values are ignored); other parameters are ignored.
fn write_filter(query: Option<&str>) -> Result<WriteFilter, Refusal> {
    let mut filter = WriteFilter::default();
    for (name, value) in url::form_urlencoded::parse(query.unwrap_or_default().as_bytes()) {
        // An empty value counts as absent, as on the other list routes.
        if value.is_empty() {
            continue;
        }
        match name.as_ref() {
            "task" => {
                let task: TaskId = value
                    .parse()
                    .map_err(|_| Refusal::new(ErrorCode::Invalid, "task must be a task id."))?;
                filter.task = Some(task);
            }
            "state" => {
                let state: WriteState = serde_json::from_value(serde_json::Value::String(
                    value.into_owned(),
                ))
                .map_err(|_| {
                    Refusal::new(
                        ErrorCode::Invalid,
                        "state must be pending, approved, denied, sending, sent, failed \
                                 or not_sent.",
                    )
                })?;
                filter.states.push(state);
            }
            _ => {}
        }
    }
    Ok(filter)
}

async fn list_writes(State(i): Shared, RawQuery(query): RawQuery) -> Result<Response, Refusal> {
    let filter = write_filter(query.as_deref())?;
    Ok(Json(i.list_writes(filter).await?).into_response())
}

async fn one_write(State(i): Shared, Path(raw): Path<String>) -> Result<Response, Refusal> {
    Ok(Json(i.get_write(write_id(&raw)?).await?).into_response())
}

async fn request_write(
    State(i): Shared,
    Extension(caller): Extension<Caller>,
    parsed: Result<Json<NewWrite>, JsonRejection>,
) -> Result<Response, Refusal> {
    let new = body(parsed, "NewWrite (api-v1.md, \"Outward writes\")")?;
    Ok((
        StatusCode::CREATED,
        Json(i.request_write(&caller, new).await?),
    )
        .into_response())
}

async fn retry_write(
    State(i): Shared,
    Extension(caller): Extension<Caller>,
    Path(raw): Path<String>,
) -> Result<Response, Refusal> {
    let ask = write_id(&raw)?;
    Ok((
        StatusCode::ACCEPTED,
        Json(i.retry_write(&caller, ask).await?),
    )
        .into_response())
}
