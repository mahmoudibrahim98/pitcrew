//! Where a socket's messages go: the webview's `events` channel in the app, a recorder in tests.

/// One message for the webview, in the contract's shapes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Delivery {
    /// A text frame: `{ "type": "text", "data": … }`.
    Text(String),
    /// A binary frame: an `ArrayBuffer`, the raw bytes.
    Binary(Vec<u8>),
    /// Always the last message: `{ "type": "close", "code": …, "reason": … }`.
    Close {
        /// The WebSocket close code; 1006 when the connection broke without one.
        code: u16,
        /// The close reason, possibly empty.
        reason: String,
    },
}

impl Delivery {
    /// The JSON text of a text or close message; `None` for binary, which goes raw.
    #[must_use]
    pub fn json(&self) -> Option<String> {
        match self {
            Self::Text(data) => {
                Some(serde_json::json!({ "type": "text", "data": data }).to_string())
            }
            Self::Close { code, reason } => Some(
                serde_json::json!({ "type": "close", "code": code, "reason": reason }).to_string(),
            ),
            Self::Binary(_) => None,
        }
    }

    /// The bytes it counts for under back-pressure: the frame's payload.
    #[must_use]
    pub fn weight(&self) -> usize {
        match self {
            Self::Text(data) => data.len(),
            Self::Binary(data) => data.len(),
            Self::Close { .. } => 0,
        }
    }
}

/// The webview is gone; nothing more can be delivered.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("the webview is gone")]
pub struct SinkClosed;

/// The webview end of a socket.
pub trait Sink: Send + Sync + 'static {
    /// Hands a message to the webview, without waiting for it to be taken.
    ///
    /// # Errors
    /// The webview is gone.
    fn deliver(&self, message: Delivery) -> Result<(), SinkClosed>;

    /// Calls `done` once the webview has taken everything delivered before this call. Until
    /// then, those messages count against the socket's back-pressure budget.
    ///
    /// # Errors
    /// The webview is gone.
    fn probe(&self, done: Box<dyn FnOnce() + Send>) -> Result<(), SinkClosed>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_have_the_contract_shapes() {
        let text: serde_json::Value =
            serde_json::from_str(&Delivery::Text("hi \"you\"".into()).json().unwrap()).unwrap();
        assert_eq!(
            text,
            serde_json::json!({ "type": "text", "data": "hi \"you\"" })
        );
        let close: serde_json::Value = serde_json::from_str(
            &Delivery::Close {
                code: 1013,
                reason: "slow".into(),
            }
            .json()
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            close,
            serde_json::json!({ "type": "close", "code": 1013, "reason": "slow" })
        );
        assert_eq!(Delivery::Binary(vec![1, 2]).json(), None);
        assert_eq!(Delivery::Binary(vec![1, 2]).weight(), 2);
    }
}
