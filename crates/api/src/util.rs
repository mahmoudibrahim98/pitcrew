//! Small helpers shared by the routes.

use axum::extract::ws::{CloseFrame, Message, Utf8Bytes, WebSocket};
use pitcrew_protocol::model::TimestampMs;
use std::time::Duration;
use tokio::sync::watch;

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
    /// The server is going away (the hub is shutting down, or the event source closed).
    pub(crate) const GOING_AWAY: u16 = 1001;
    /// A message the server cannot accept (malformed control JSON).
    pub(crate) const INVALID_DATA: u16 = 1007;
    /// A message over the socket's size limit.
    pub(crate) const TOO_BIG: u16 = 1009;
    /// An unexpected server-side failure.
    pub(crate) const INTERNAL: u16 = 1011;
    /// The client fell behind; it should reconnect and resume.
    pub(crate) const TRY_AGAIN_LATER: u16 = 1013;
}

/// How long a closing handshake may take. A client that stopped reading may never finish it.
const CLOSE_WAIT: Duration = Duration::from_secs(1);

/// Closes the socket: sends a Close frame, then waits for the client's reply, so browsers see a
/// clean close. Gives up after a second.
pub(crate) async fn close(socket: &mut WebSocket, code: u16, reason: &'static str) {
    let frame = Message::Close(Some(CloseFrame {
        code,
        reason: Utf8Bytes::from_static(reason),
    }));
    let _ = tokio::time::timeout(CLOSE_WAIT, async {
        if socket.send(frame).await.is_ok() {
            drain(socket).await;
        }
    })
    .await;
}

/// After the client's Close: sends the reply the socket has queued and waits for the end of the
/// connection (at most a second). Dropping the socket instead would lose the reply.
pub(crate) async fn finish(socket: &mut WebSocket) {
    let _ = tokio::time::timeout(CLOSE_WAIT, drain(socket)).await;
}

/// Reads until the connection ends. Reading also writes the replies the socket has queued.
async fn drain(socket: &mut WebSocket) {
    while let Some(Ok(_)) = socket.recv().await {}
}

/// Whether a receive error is the socket's size limit (`max_message_size`/`max_frame_size`).
/// The socket cannot be read after it, but a Close (1009) can still be sent.
pub(crate) fn too_big(error: &axum::Error) -> bool {
    std::error::Error::source(error)
        .and_then(|inner| inner.downcast_ref::<tungstenite::Error>())
        .is_some_and(|e| matches!(e, tungstenite::Error::Capacity(_)))
}

/// Tells WebSocket routes that the hub is shutting down, so they close with 1001.
/// [`crate::Bound::serve`] adds it to every request as an extension.
///
/// It is also how `serve` counts open sockets: each one **holds its receiver for its whole life,
/// through its closing handshake**, and `serve` waits until every receiver is gone.
#[derive(Clone, Debug)]
pub(crate) struct HubShutdown(watch::Receiver<bool>);

impl HubShutdown {
    /// The sender says `true` when the hub starts shutting down. It learns that every socket has
    /// finished with `closed()`, since each open socket holds a receiver.
    pub(crate) fn channel() -> (watch::Sender<bool>, Self) {
        let (sender, receiver) = watch::channel(false);
        (sender, Self(receiver))
    }

    /// Completes when the hub starts shutting down, or never without a signal (a router served
    /// some other way, e.g. in tests). It only borrows the signal: the caller keeps it alive
    /// until the socket is closed.
    pub(crate) async fn wait(signal: &mut Option<Self>) {
        let Some(Self(receiver)) = signal else {
            return std::future::pending().await;
        };
        // An error means the server is gone, which is a shutdown too.
        let _ = receiver.wait_for(|down| *down).await;
    }
}
