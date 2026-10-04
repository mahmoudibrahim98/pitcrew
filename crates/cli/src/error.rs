//! Errors and exit codes.
//!
//! | Exit | Kind | When |
//! |---|---|---|
//! | 0 | | Success |
//! | 1 | `internal`, `untrusted`, `incompatible` | Anything unexpected; the daemon failed the identity check; its protocol is too old or too new |
//! | 2 | `invalid` | Bad arguments or environment, or the daemon answered `400 invalid` |
//! | 3 | `unauthorized`, `forbidden` | The token was refused, or it does not allow this |
//! | 4 | `conflict` | The rules say no, e.g. a move `can_move` rejects |
//! | 5 | `unavailable` | The daemon cannot be reached, or answered `503` |
//! | 6 | `not_found` | No such task, ask or member |

use pitcrew_protocol::api::{ApiError, ErrorCode};
use std::fmt;

/// What went wrong, as far as a script needs to know.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Bad input: arguments, environment, or a request the daemon called malformed.
    Invalid,
    /// The daemon did not accept the token.
    Unauthorized,
    /// The token does not allow this.
    Forbidden,
    /// No such resource.
    NotFound,
    /// Allowed, but the rules say no.
    Conflict,
    /// The daemon cannot be reached.
    Unavailable,
    /// The daemon (or the token file) failed a security check, so the token was not sent.
    Untrusted,
    /// The daemon speaks a protocol this build does not.
    Incompatible,
    /// Anything else.
    Internal,
}

impl Kind {
    /// The process exit code.
    #[must_use]
    pub const fn exit_code(self) -> i32 {
        match self {
            Self::Internal | Self::Untrusted | Self::Incompatible => 1,
            Self::Invalid => 2,
            Self::Unauthorized | Self::Forbidden => 3,
            Self::Conflict => 4,
            Self::Unavailable => 5,
            Self::NotFound => 6,
        }
    }

    /// The name used in `--json` error output.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Invalid => "invalid",
            Self::Unauthorized => "unauthorized",
            Self::Forbidden => "forbidden",
            Self::NotFound => "not_found",
            Self::Conflict => "conflict",
            Self::Unavailable => "unavailable",
            Self::Untrusted => "untrusted",
            Self::Incompatible => "incompatible",
            Self::Internal => "internal",
        }
    }

    /// The kind for an API error code.
    #[must_use]
    pub const fn from_code(code: ErrorCode) -> Self {
        match code {
            ErrorCode::Invalid => Self::Invalid,
            ErrorCode::Unauthorized => Self::Unauthorized,
            ErrorCode::Forbidden => Self::Forbidden,
            ErrorCode::NotFound => Self::NotFound,
            ErrorCode::Conflict => Self::Conflict,
            ErrorCode::Unavailable => Self::Unavailable,
            ErrorCode::TooLarge => Self::Invalid,
            ErrorCode::Unsupported => Self::Unavailable,
            ErrorCode::Internal => Self::Internal,
        }
    }

    /// The kind for an HTTP status whose body is not an `ApiError`.
    #[must_use]
    pub const fn from_status(status: u16) -> Self {
        match status {
            400 | 413 | 422 => Self::Invalid,
            401 => Self::Unauthorized,
            403 => Self::Forbidden,
            404 => Self::NotFound,
            409 => Self::Conflict,
            501..=504 => Self::Unavailable,
            _ => Self::Internal,
        }
    }
}

/// A failure, with a sentence for people.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    /// What kind.
    pub kind: Kind,
    /// What happened, and what to do about it where that is known.
    pub message: String,
}

impl Error {
    /// An error of `kind`.
    pub fn new(kind: Kind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    /// Bad input.
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(Kind::Invalid, message)
    }

    /// Something unexpected.
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(Kind::Internal, message)
    }

    /// A failed request: the daemon's `ApiError` if the body is one, else the status alone.
    #[must_use]
    pub fn from_response(status: u16, body: &[u8]) -> Self {
        match serde_json::from_slice::<ApiError>(body) {
            Ok(api) => {
                let kind = Kind::from_code(api.code);
                let message = match kind {
                    Kind::Unauthorized => format!(
                        "the daemon did not accept the token (PITCREW_TOKEN or PITCREW_TOKEN_FILE): {}",
                        api.message
                    ),
                    _ => api.message,
                };
                Self::new(kind, message)
            }
            Err(_) => Self::new(
                Kind::from_status(status),
                format!("the daemon answered HTTP {status}"),
            ),
        }
    }

    /// The exit code.
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        self.kind.exit_code()
    }

    /// `{"code": …, "message": …}`, the same shape as the API's errors.
    #[must_use]
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({ "code": self.kind.as_str(), "message": self.message })
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

/// Shorthand.
pub type Result<T, E = Error> = std::result::Result<T, E>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_errors_map_to_exit_codes() {
        let cases = [
            (400, "invalid", 2),
            (401, "unauthorized", 3),
            (403, "forbidden", 3),
            (404, "not_found", 6),
            (409, "conflict", 4),
            (503, "unavailable", 5),
            (500, "internal", 1),
        ];
        for (status, code, exit) in cases {
            let body = format!(r#"{{"code":"{code}","message":"m"}}"#);
            let err = Error::from_response(status, body.as_bytes());
            assert_eq!(err.exit_code(), exit, "{code}");
            assert_eq!(err.kind.as_str(), code);
            // Without an ApiError body, the status decides.
            assert_eq!(Error::from_response(status, b"<html>").exit_code(), exit);
        }
    }

    #[test]
    fn the_body_code_wins_over_the_status() {
        let err = Error::from_response(500, br#"{"code":"conflict","message":"no"}"#);
        assert_eq!(err.kind, Kind::Conflict);
        assert_eq!(err.message, "no");
    }
    #[test]
    fn file_error_codes_keep_the_existing_exit_categories() {
        for (code, status, kind) in [
            (ErrorCode::TooLarge, 413, Kind::Invalid),
            (ErrorCode::Unsupported, 501, Kind::Unavailable),
        ] {
            assert_eq!(Kind::from_code(code), kind);
            assert_eq!(Kind::from_status(status), kind);
        }
    }
}
