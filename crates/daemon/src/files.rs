//! Device routes: roots come exclusively from the work model, I/O runs on the blocking pool.
use axum::extract::{
    DefaultBodyLimit, Path, Query, State,
    rejection::{JsonRejection, PathRejection, QueryRejection},
};
use axum::response::{IntoResponse as _, Response};
use axum::routing::get;
use axum::{Json, Router};
use pitcrew_hub_work::WorkService;
use pitcrew_protocol::{
    api::ErrorCode,
    files::{MAX_BODY_BYTES, WriteFile},
    ids::WorkstreamId,
    model::MachineKind,
};
use pitcrew_runner::files::{FileError, Files};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Debug)]
struct FileRoutes {
    work: Arc<WorkService>,
    files: Files,
}
pub fn routes(work: Arc<WorkService>, state: &std::path::Path) -> Router {
    Router::new()
        .route("/v1/workstreams/{id}/files", get(list))
        .route("/v1/workstreams/{id}/files/content", get(read).put(write))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .with_state(Arc::new(FileRoutes {
            work,
            files: Files::new(state),
        }))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileQuery {
    loc: String,
    path: String,
}
fn error(code: ErrorCode, message: &'static str) -> FileError {
    FileError {
        code,
        message,
        size: None,
        current_revision: None,
    }
}
fn root(state: &FileRoutes, id: WorkstreamId, loc: &str) -> Result<PathBuf, FileError> {
    if loc.is_empty() || !loc.bytes().all(|c| c.is_ascii_digit()) {
        return Err(error(ErrorCode::Invalid, "Invalid location index."));
    }
    let index: usize = loc
        .parse()
        .map_err(|_| error(ErrorCode::Invalid, "Invalid location index."))?;
    let stream = state
        .work
        .workstream(&id)
        .map_err(|e| error(e.code(), "Workstream lookup failed."))?;
    let location = stream
        .locations
        .get(index)
        .ok_or_else(|| error(ErrorCode::NotFound, "Location not found."))?;
    let machines = state
        .work
        .machines()
        .map_err(|_| error(ErrorCode::Unavailable, "Machine lookup failed."))?;
    if !machines
        .iter()
        .find(|m| m.kind == MachineKind::Local)
        .is_some_and(|m| m.id == location.machine)
    {
        return Err(error(
            ErrorCode::Unsupported,
            "Files on another machine are unsupported.",
        ));
    }
    Ok(PathBuf::from(&location.path))
}
fn response(result: Result<Value, FileError>) -> Response {
    let mut response = match result {
        Ok(value) => Json(value).into_response(),
        Err(e) => {
            tracing::debug!(count = 1, code = ?e.code, reason = e.message, "file operation refused");
            let mut body = json!({ "code": e.code, "message": e.message });
            if let Some(size) = e.size {
                body["size"] = json!(size);
            }
            if let Some(revision) = e.current_revision {
                body["current_revision"] = json!(revision);
            }
            (
                axum::http::StatusCode::from_u16(e.code.http_status())
                    .unwrap_or(axum::http::StatusCode::INTERNAL_SERVER_ERROR),
                Json(body),
            )
                .into_response()
        }
    };
    response.headers_mut().insert(
        "cache-control",
        axum::http::HeaderValue::from_static("no-store"),
    );
    response.headers_mut().insert(
        "x-content-type-options",
        axum::http::HeaderValue::from_static("nosniff"),
    );
    response
}
type Id = Result<Path<WorkstreamId>, PathRejection>;
type QueryResult = Result<Query<FileQuery>, QueryRejection>;
async fn run(
    state: Arc<FileRoutes>,
    id: Id,
    query: QueryResult,
    operation: Option<Option<WriteFile>>,
) -> Response {
    let (Ok(Path(id)), Ok(Query(query))) = (id, query) else {
        return response(Err(error(ErrorCode::Invalid, "Invalid file request.")));
    };
    response(
        tokio::task::spawn_blocking(move || {
            let list = operation.is_none();
            pitcrew_runner::files::validate_path(
                &query.path,
                list,
                operation.as_ref().is_some_and(Option::is_some),
            )?;
            let root = root(&state, id, &query.loc)?;
            let value = match operation {
                None => serde_json::to_value(state.files.list(&root, &query.path)?),
                Some(None) => serde_json::to_value(state.files.read(&root, &query.path)?),
                Some(Some(body)) => {
                    serde_json::to_value(state.files.write(&root, &query.path, body)?)
                }
            };
            value.map_err(|_| error(ErrorCode::Internal, "File response failed."))
        })
        .await
        .unwrap_or_else(|_| Err(error(ErrorCode::Unavailable, "File operation failed."))),
    )
}
async fn list(State(state): State<Arc<FileRoutes>>, id: Id, query: QueryResult) -> Response {
    run(state, id, query, None).await
}
async fn read(State(state): State<Arc<FileRoutes>>, id: Id, query: QueryResult) -> Response {
    run(state, id, query, Some(None)).await
}
async fn write(
    State(state): State<Arc<FileRoutes>>,
    id: Id,
    query: QueryResult,
    body: Result<Json<WriteFile>, JsonRejection>,
) -> Response {
    match body {
        Ok(Json(body)) => run(state, id, query, Some(Some(body))).await,
        Err(e) => response(Err(error(
            if e.status() == axum::http::StatusCode::PAYLOAD_TOO_LARGE {
                ErrorCode::TooLarge
            } else {
                ErrorCode::Invalid
            },
            "Invalid file body.",
        ))),
    }
}
