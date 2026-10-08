//! The Orchestrator's routes (api-v1.md, "Orchestrator"; see [`crate::orchestrator`]), apart from
//! the work routes, so a composition mounts them on purpose. Mount [`orchestrator_routes`] with
//! `RouterParts::device`: each is for a person's device token only, and each handler refuses
//! agents and readers itself too. They see only the caller's own conversations.
//!
//! | Route | Answer |
//! |---|---|
//! | `GET /v1/orchestrator` | `Orchestrator` |
//! | `POST /v1/orchestrator/questions` | `Question` → `Conversation` (202) |
//! | `POST /v1/orchestrator/conversations/{id}/cancel` | `Conversation` |
//! | `DELETE /v1/orchestrator/conversations` | 204 |

use crate::error::WorkError;
use crate::routes::{Person, Reply, Segments, Work, blocking, json, path_id};
use axum::Json;
use axum::Router;
use axum::body::Body as RawBody;
use axum::http::StatusCode;
use axum::routing::{delete, get, post};
use pitcrew_protocol::ids::ConversationId;
use pitcrew_protocol::orchestrator::{Conversation, Orchestrator, Question};

/// The routes. See the [module docs](self).
pub fn orchestrator_routes<S>() -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/v1/orchestrator", get(read))
        .route("/v1/orchestrator/questions", post(ask))
        .route("/v1/orchestrator/conversations", delete(clear))
        .route("/v1/orchestrator/conversations/{id}/cancel", post(cancel))
}

async fn read(Work(w): Work, Person(caller): Person) -> Reply<Orchestrator> {
    Ok(Json(blocking(w, move |w| w.orchestrator(&caller)).await?))
}

async fn ask(
    Work(w): Work,
    Person(caller): Person,
    body: RawBody,
) -> Result<(StatusCode, Json<Conversation>), WorkError> {
    let question: Question = json(body).await?;
    let conversation = blocking(w, move |w| w.ask_orchestrator(&caller, question)).await?;
    Ok((StatusCode::ACCEPTED, Json(conversation)))
}

async fn cancel(
    Work(w): Work,
    Person(caller): Person,
    Segments(id): Segments<String>,
) -> Reply<Conversation> {
    let id: ConversationId = path_id(&id, "conversation")?;
    Ok(Json(
        blocking(w, move |w| w.cancel_answer(&caller, &id)).await?,
    ))
}

async fn clear(Work(w): Work, Person(caller): Person) -> Result<StatusCode, WorkError> {
    blocking(w, move |w| w.clear_conversations(&caller)).await?;
    Ok(StatusCode::NO_CONTENT)
}
