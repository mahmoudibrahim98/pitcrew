//! `GatewayError`: how a gateway command fails when the daemon never answered (the contract's
//! "Gateway failures"). A daemon's own error is a response, never one of these.

use serde::Serialize;

/// The contract's error codes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// No workspace has this id.
    UnknownWorkspace,
    /// The workspace needs pairing before the gateway can reach it.
    NeedsPairing,
    /// The daemon cannot be reached, or a socket's upgrade answered `503 unavailable`.
    Unreachable,
    /// The call itself is malformed: a bad path, argument or socket.
    Invalid,
    /// A body or message over its limit.
    TooLarge,
    /// Anything else.
    Internal,
}

/// A failed gateway call, as the webview receives it: `{ "code": …, "message": … }`.
///
/// Messages are for people. They never hold a token, a request body or a frame.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, thiserror::Error)]
#[error("{message}")]
pub struct GatewayError {
    /// What went wrong.
    pub code: ErrorCode,
    /// A sentence for people.
    pub message: String,
}

impl GatewayError {
    /// An error with this code.
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// `invalid`.
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Invalid, message)
    }

    /// `unreachable`.
    pub fn unreachable(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Unreachable, message)
    }

    /// `too_large`.
    pub fn too_large(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::TooLarge, message)
    }

    /// `internal`.
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Internal, message)
    }

    /// `needs_pairing`.
    pub fn needs_pairing(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::NeedsPairing, message)
    }

    /// `unknown_workspace` for `id`.
    pub fn unknown_workspace(id: &str) -> Self {
        Self::new(
            ErrorCode::UnknownWorkspace,
            format!("no workspace {}", shorten(id)),
        )
    }
}

/// `s`, cut to a length that is safe to show in a message.
pub(crate) fn shorten(s: &str) -> String {
    const MAX: usize = 64;
    let mut out: String = s.chars().filter(|c| !c.is_control()).take(MAX).collect();
    if s.chars().count() > MAX {
        out.push('…');
    }
    format!("{out:?}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_serializes_as_the_contract_says() {
        let e = GatewayError::unknown_workspace("01J");
        assert_eq!(
            serde_json::to_value(&e).unwrap(),
            serde_json::json!({ "code": "unknown_workspace", "message": "no workspace \"01J\"" })
        );
        for (code, name) in [
            (ErrorCode::NeedsPairing, "needs_pairing"),
            (ErrorCode::Unreachable, "unreachable"),
            (ErrorCode::Invalid, "invalid"),
            (ErrorCode::TooLarge, "too_large"),
            (ErrorCode::Internal, "internal"),
        ] {
            assert_eq!(serde_json::to_value(code).unwrap(), name);
        }
    }

    #[test]
    fn ids_in_messages_are_cut_and_cleaned() {
        let long = "x".repeat(500);
        let e = GatewayError::unknown_workspace(&long);
        assert!(e.message.len() < 100, "{}", e.message);
        let e = GatewayError::unknown_workspace("a\u{0}b\nc");
        assert_eq!(e.message, "no workspace \"abc\"");
    }
}
