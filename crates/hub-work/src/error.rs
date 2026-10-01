//! The one error type of the work model. It carries an [`ErrorCode`], so a route answers with an
//! `ApiError` body and the code's HTTP status.
//!
//! **Internal failures stay internal.** A `500 internal` is logged in full when it is made, with
//! its whole cause chain, but the client only ever sees [`INTERNAL_MESSAGE`]: no SQLite text, no
//! table, column or constraint names. A uniqueness violation from the store is a `409 conflict`
//! (a racing change took the slot), with a generic message too.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use pitcrew_protocol::api::{ApiError, ErrorCode};
use std::fmt::Write as _;

/// What a client is told about any internal failure.
pub const INTERNAL_MESSAGE: &str =
    "Something went wrong in the hub's work model. The hub's log has the details.";

/// What a client is told when the store refused a change as a duplicate.
const CONFLICT_MESSAGE: &str = "Another change got there first. Reload and try again.";

/// A refused or failed work command or query.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct WorkError {
    code: ErrorCode,
    message: String,
}

impl WorkError {
    /// An error with `code` and a sentence for people.
    #[must_use]
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// `400 invalid`: a malformed request, or an unknown id in the body.
    #[must_use]
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Invalid, message)
    }

    /// `404 not_found`: no such resource in the path.
    #[must_use]
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::NotFound, message)
    }

    /// `403 forbidden`: the caller's scope does not allow it.
    #[must_use]
    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Forbidden, message)
    }

    /// `409 conflict`: an allowed caller, but the rules say no.
    #[must_use]
    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Conflict, message)
    }

    /// `503 unavailable`: the machine that would do it cannot be reached.
    #[must_use]
    pub fn unavailable(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Unavailable, message)
    }

    /// `500 internal`. `detail` is logged now and kept in [`WorkError::message`] for the hub's
    /// own callers; a client sees only [`INTERNAL_MESSAGE`].
    #[must_use]
    pub fn internal(detail: impl Into<String>) -> Self {
        let detail = detail.into();
        tracing::error!(detail = %detail, "work model failure");
        Self::new(ErrorCode::Internal, detail)
    }

    /// The code.
    #[must_use]
    pub fn code(&self) -> ErrorCode {
        self.code
    }

    /// The message. For an internal error this is the full detail, which is never sent to a
    /// client (see [`WorkError::to_api`]).
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    /// The `ApiError` body a client gets: the message, except for internal errors, which all
    /// read [`INTERNAL_MESSAGE`].
    #[must_use]
    pub fn to_api(&self) -> ApiError {
        let message = if self.code == ErrorCode::Internal {
            INTERNAL_MESSAGE.to_owned()
        } else {
            self.message.clone()
        };
        ApiError {
            code: self.code,
            message,
        }
    }

    /// An error from a lower layer: a `409` when the store refused a duplicate, else a `500`.
    /// Either way the whole cause chain is logged and none of it reaches the client.
    fn from_cause(what: &str, error: &(dyn std::error::Error + 'static)) -> Self {
        let mut detail = format!("{what}: {error}");
        let mut source = error.source();
        while let Some(cause) = source {
            let _ = write!(detail, ": {cause}");
            source = cause.source();
        }
        if is_constraint_violation(error) {
            tracing::warn!(detail = %detail, "work change refused by a uniqueness rule");
            return Self::conflict(CONFLICT_MESSAGE);
        }
        Self::internal(detail)
    }
}

/// Whether SQLite refused a write because of a constraint (a unique key, most of all), anywhere in
/// the cause chain.
fn is_constraint_violation(error: &(dyn std::error::Error + 'static)) -> bool {
    let mut next = Some(error);
    while let Some(e) = next {
        let sql = e
            .downcast_ref::<pitcrew_store::DbError>()
            .map(pitcrew_store::DbError::as_sql)
            .or_else(|| e.downcast_ref::<pitcrew_store::sql::Error>());
        if sql.and_then(pitcrew_store::sql::Error::sqlite_error_code)
            == Some(pitcrew_store::sql::ErrorCode::ConstraintViolation)
        {
            return true;
        }
        next = e.source();
    }
    false
}

impl From<pitcrew_store::Error> for WorkError {
    fn from(e: pitcrew_store::Error) -> Self {
        Self::from_cause("store", &e)
    }
}

impl From<pitcrew_store::sql::Error> for WorkError {
    fn from(e: pitcrew_store::sql::Error) -> Self {
        Self::from_cause("database", &e)
    }
}

impl From<serde_json::Error> for WorkError {
    fn from(e: serde_json::Error) -> Self {
        Self::from_cause("json", &e)
    }
}

impl IntoResponse for WorkError {
    fn into_response(self) -> Response {
        let status = StatusCode::from_u16(self.code.http_status())
            .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        (status, Json(self.to_api())).into_response()
    }
}

/// Shorthand for results of the work model.
pub type Result<T, E = WorkError> = std::result::Result<T, E>;

#[cfg(test)]
mod tests {
    use super::*;
    use pitcrew_store::sql::{Connection, ffi};

    fn unique_violation() -> pitcrew_store::sql::Error {
        let conn = Connection::open_in_memory().expect("memory db");
        conn.execute_batch(
            "CREATE TABLE secret_table (k TEXT PRIMARY KEY); INSERT INTO secret_table VALUES ('a');",
        )
        .expect("setup");
        conn.execute("INSERT INTO secret_table VALUES ('a')", [])
            .expect_err("duplicate key")
    }

    #[test]
    fn internal_errors_reach_clients_without_their_detail() {
        let error = WorkError::from(pitcrew_store::sql::Error::SqliteFailure(
            ffi::Error::new(ffi::SQLITE_CORRUPT),
            Some("database disk image is malformed in work_tasks".into()),
        ));
        assert_eq!(error.code(), ErrorCode::Internal);
        assert!(error.message().contains("work_tasks"), "kept for the log");
        let body = error.to_api();
        assert_eq!(body.message, INTERNAL_MESSAGE);
        assert!(!body.message.contains("work_tasks"));
    }

    #[test]
    fn a_uniqueness_violation_is_a_conflict_without_the_constraint_name() {
        let error = WorkError::from(unique_violation());
        assert_eq!(error.code(), ErrorCode::Conflict);
        let body = error.to_api();
        assert!(!body.message.contains("secret_table"), "{}", body.message);
        assert!(!body.message.to_lowercase().contains("constraint"));

        // Through the store's own wrapping too.
        let projection = pitcrew_store::Error::Projection {
            name: "work.tasks".into(),
            rev: 7,
            source: Box::new(unique_violation()),
        };
        assert_eq!(WorkError::from(projection).code(), ErrorCode::Conflict);
    }
}
