//! The board-draft routes of API v1 (api-v1.md, "Board drafts"; see [`crate::board`]), apart
//! from the work routes, so a composition mounts them on purpose:
//!
//! - [`board_agent_routes`]: `POST /v1/board-drafts/{id}/proposal`, the drafting agent's answer.
//!   Mount with `RouterParts::agent`; only the draft's own agent may call it (`403` for anyone
//!   else, a person included, before the body is read). The body is at most
//!   [`MAX_PROPOSAL_BYTES`].
//! - [`board_device_routes`]: the preview, the start, the list, one draft, and the review. Mount
//!   with `RouterParts::device`; each handler refuses agents itself too.

use crate::error::WorkError;
use crate::routes::{
    Created, Person, Reply, Segments, Who, Work, blocking, decode, exists, json, path_id,
};
use axum::Json;
use axum::Router;
use axum::body::Body as RawBody;
use axum::extract::{FromRequestParts, Query};
use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::routing::{get, post};
use pitcrew_protocol::board::{
    BoardDraft, BoardProposal, DraftPreview, DraftReview, DraftReviewed, MAX_PROPOSAL_BYTES,
    StartDraft,
};
use pitcrew_protocol::ids::{DraftId, WorkstreamId};
use std::sync::Arc;

/// The agent's route. See the [module docs](self).
pub fn board_agent_routes<S>() -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    Router::new().route("/v1/board-drafts/{id}/proposal", post(propose))
}

/// The people's routes. See the [module docs](self).
pub fn board_device_routes<S>() -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/v1/workstreams/{id}/board-draft", get(preview))
        .route("/v1/workstreams/{id}/board-drafts", post(start))
        .route("/v1/board-drafts", get(list))
        .route("/v1/board-drafts/{id}", get(read))
        .route("/v1/board-drafts/{id}/review", post(review))
}

/// `?workstream=`, an id; empty counts as absent.
struct ListQuery(Option<WorkstreamId>);

impl<S: Send + Sync> FromRequestParts<S> for ListQuery {
    type Rejection = WorkError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, WorkError> {
        let Query(pairs) = Query::<Vec<(String, String)>>::from_request_parts(parts, state)
            .await
            .map_err(|e| WorkError::invalid(format!("Malformed query: {e}.")))?;
        let value = pairs
            .iter()
            .find(|(k, v)| k == "workstream" && !v.is_empty())
            .map(|(_, v)| v.as_str());
        value
            .map(|v| {
                v.parse().map_err(|_| {
                    WorkError::invalid(format!("workstream: {v:?} is not a valid id."))
                })
            })
            .transpose()
            .map(Self)
    }
}

async fn preview(
    Work(w): Work,
    Person(caller): Person,
    Segments(id): Segments<String>,
) -> Reply<DraftPreview> {
    let id: WorkstreamId = path_id(&id, "workstream")?;
    Ok(Json(
        blocking(w, move |w| w.draft_preview(&caller, &id)).await?,
    ))
}

async fn start(
    Work(w): Work,
    Person(caller): Person,
    Segments(id): Segments<String>,
    body: RawBody,
) -> Result<(StatusCode, Json<BoardDraft>), WorkError> {
    let id: WorkstreamId = path_id(&id, "workstream")?;
    exists(&w, move |w| w.workstream(&id)).await?;
    let start: StartDraft = json(body).await?;
    let draft = blocking(w, move |w| w.start_draft(&caller, &id, start)).await?;
    Ok((StatusCode::ACCEPTED, Json(draft)))
}

async fn list(Work(w): Work, Person(_): Person, query: ListQuery) -> Reply<Vec<BoardDraft>> {
    let ListQuery(workstream) = query;
    Ok(Json(
        blocking(w, move |w| w.board_drafts(workstream.as_ref())).await?,
    ))
}

async fn read(
    Work(w): Work,
    Person(_): Person,
    Segments(id): Segments<String>,
) -> Reply<BoardDraft> {
    let id: DraftId = path_id(&id, "board draft")?;
    Ok(Json(blocking(w, move |w| w.board_draft(&id)).await?))
}

async fn review(
    Work(w): Work,
    Person(caller): Person,
    Segments(id): Segments<String>,
    body: RawBody,
) -> Reply<DraftReviewed> {
    let id: DraftId = path_id(&id, "board draft")?;
    exists(&w, move |w| w.board_draft(&id)).await?;
    let review: DraftReview = json(body).await?;
    Ok(Json(
        blocking(w, move |w| w.review_draft(&caller, &id, review)).await?,
    ))
}

async fn propose(
    Work(w): Work,
    Who(caller): Who,
    Segments(id): Segments<String>,
    body: RawBody,
) -> Created<BoardDraft> {
    let id: DraftId = path_id(&id, "board draft")?;
    blocking(Arc::clone(&w), move |w| {
        w.check_draft_proposer(&caller, &id)
    })
    .await?;
    let bytes = axum::body::to_bytes(body, MAX_PROPOSAL_BYTES)
        .await
        .map_err(|_| {
            WorkError::invalid(format!(
                "The proposal is larger than {} KiB, or could not be read.",
                MAX_PROPOSAL_BYTES / 1024
            ))
        })?;
    let value: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|e| WorkError::invalid(format!("Malformed proposal: {e}.")))?;
    if !value.is_object() {
        return Err(WorkError::invalid("The proposal must be a JSON object."));
    }
    let proposal: BoardProposal = decode(value)?;
    let draft = blocking(w, move |w| w.propose_board(&caller, &id, proposal)).await?;
    Ok((StatusCode::CREATED, Json(draft)))
}
