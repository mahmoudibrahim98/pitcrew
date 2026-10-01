//! The one error type of the work model. It carries an [`ErrorCode`], so a route answers with an
//! `ApiError` body and the code's HTTP status.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use pitcrew_protocol::api::{ApiError, ErrorCode};
use std::fmt::Write as _;

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

    /// `500 internal`.
    #[must_use]
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Internal, message)
    }

    /// The code.
    #[must_use]
    pub fn code(&self) -> ErrorCode {
        self.code
    }

    /// The message.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    /// The `ApiError` body.
    #[must_use]
    pub fn to_api(&self) -> ApiError {
        ApiError {
            code: self.code,
            message: self.message.clone(),
        }
    }

    /// An internal error from a lower layer, with its whole cause chain in the message, logged.
    fn from_cause(what: &str, error: &(dyn std::error::Error + 'static)) -> Self {
        let mut message = format!("{what}: {error}");
        let mut source = error.source();
        while let Some(cause) = source {
            let _ = write!(message, ": {cause}");
            source = cause.source();
        }
        tracing::error!(%message, "work model failure");
        Self::internal(message)
    }
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
