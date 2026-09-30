//! Small helpers shared by the routes.

use axum::extract::ws::{CloseFrame, Message, Utf8Bytes, WebSocket};
use pitcrew_protocol::model::TimestampMs;
use std::time::Duration;

/// Now, in UTC milliseconds.
pub(crate) fn now_ms() -> TimestampMs {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| {
            TimestampMs::try_from(d.as_millis()).unwrap_or(TimestampMs::MAX)
        })
}

/// WebSocket close codes used by the API.
pub(crate) mod close_code {
    /// The work is done (e.g. the terminal's program exited).
    pub(crate) const NORMAL: u16 = 1000;
    /// The server is going away (e.g. the event source shut down).
    pub(crate) const GOING_AWAY: u16 = 1001;
    /// A message the server cannot accept (malformed control JSON).
    pub(crate) const INVALID_DATA: u16 = 1007;
    /// An unexpected server-side failure.
    pub(crate) const INTERNAL: u16 = 1011;
    /// The client fell behind; it should reconnect and resume.
    pub(crate) const TRY_AGAIN_LATER: u16 = 1013;
}

/// Sends a Close frame, giving up after a second (a client that stopped reading may never take
/// it).
pub(crate) async fn close(socket: &mut WebSocket, code: u16, reason: &'static str) {
    let frame = Message::Close(Some(CloseFrame {
        code,
        reason: Utf8Bytes::from_static(reason),
    }));
    let _ = tokio::time::timeout(Duration::from_secs(1), socket.send(frame)).await;
}
