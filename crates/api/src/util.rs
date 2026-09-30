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

/// Tells WebSocket routes that the hub is shutting down, so they close with 1001.
/// [`crate::Bound::serve`] adds it to every request as an extension.
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
    /// some other way, e.g. in tests).
    pub(crate) async fn wait(signal: Option<Self>) {
        let Some(Self(mut receiver)) = signal else {
            return std::future::pending().await;
        };
        // An error means the server is gone, which is a shutdown too.
        let _ = receiver.wait_for(|down| *down).await;
    }
}
