//! Gateway sockets: the WebSocket to the daemon, and the pump that carries its frames to the
//! webview and the webview's messages back.
//!
//! - Frames reach the webview in order; `close` is always the last message.
//! - Pings are the gateway's business: tungstenite answers each one with a Pong as it reads, and
//!   neither ever reaches the webview.
//! - **Back-pressure.** Every frame handed to the webview counts against an 8 MiB budget until the
//!   webview has taken it. The pump knows that by a probe: a no-op script evaluated in the webview
//!   after the frames, whose completion means the webview's JavaScript ran every script before it.
//!   Small frames are delivered by those scripts themselves, so they have reached the page's
//!   handler. A large frame (JSON of 8 KiB or more, binary of 1 KiB or more) is only *fetched* by
//!   its script, so the probe releases it once that fetch has started, not once the page has
//!   handled it. One probe is in flight at a time. Past the budget the socket closes with 1013,
//!   and the UI reconnects with `since` or `from`, as it does for a slow client today.

use super::error::GatewayError;
use super::sink::{Delivery, Sink};
use crate::daemon::endpoint::BoxIo;
use crate::redact::redact;
use crate::token::DeviceToken;
use futures_util::{SinkExt as _, StreamExt as _};
use http::HeaderValue;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::{CloseFrame, WebSocketConfig};
use tokio_tungstenite::tungstenite::{Error as WsError, Message};

/// The subprotocol the daemon answers with.
pub(crate) const SUBPROTOCOL: &str = "pitcrew.v1";

/// A WebSocket to a daemon.
pub(crate) type Ws = WebSocketStream<BoxIo>;

/// Close codes the gateway itself uses.
pub(crate) mod codes {
    /// Normal closure, the default for `gateway_socket_close`.
    pub const NORMAL: u16 = 1000;
    /// The page or the app went away.
    pub const GOING_AWAY: u16 = 1001;
    /// A close frame without a code.
    pub const NO_STATUS: u16 = 1005;
    /// The connection broke without a close frame.
    pub const ABNORMAL: u16 = 1006;
    /// A message over its limit.
    pub const TOO_BIG: u16 = 1009;
    /// The webview is not keeping up.
    pub const TRY_AGAIN_LATER: u16 = 1013;
}

/// Opens the WebSocket at `path` on `io`, with the token as a subprotocol:
/// `Sec-WebSocket-Protocol: pitcrew.v1, pitcrew.bearer.<token>`. Resolves once the upgrade has
/// succeeded.
///
/// `max_message` bounds each frame the daemon may send; over it the socket closes with 1009.
///
/// # Errors
/// The daemon refused the upgrade (its status is in the message; 503 is `unreachable`), or the
/// connection broke.
pub(crate) async fn open(
    io: BoxIo,
    token: &DeviceToken,
    path: &str,
    max_message: usize,
) -> Result<Ws, GatewayError> {
    use tokio_tungstenite::tungstenite::handshake::client::generate_key;

    let mut protocols =
        HeaderValue::try_from(format!("{SUBPROTOCOL}, pitcrew.bearer.{}", token.expose()))
            .map_err(|_| GatewayError::internal("the device token cannot go in a header"))?;
    protocols.set_sensitive(true);
    let request = http::Request::builder()
        .method("GET")
        .uri(format!("ws://localhost{path}"))
        .header("Host", "localhost")
        .header("Connection", "Upgrade")
        .header("Upgrade", "websocket")
        .header("Sec-WebSocket-Version", "13")
        .header("Sec-WebSocket-Key", generate_key())
        .header("Sec-WebSocket-Protocol", protocols)
        .body(())
        .map_err(|e| GatewayError::internal(format!("cannot build the upgrade request: {e}")))?;
    let config = WebSocketConfig::default()
        .max_message_size(Some(max_message))
        .max_frame_size(Some(max_message));
    let (ws, response) = tokio_tungstenite::client_async_with_config(request, io, Some(config))
        .await
        .map_err(upgrade_error)?;
    let accepted = response
        .headers()
        .get("Sec-WebSocket-Protocol")
        .is_some_and(|p| p == SUBPROTOCOL);
    if !accepted {
        return Err(GatewayError::internal(format!(
            "the daemon did not answer with the {SUBPROTOCOL} subprotocol"
        )));
    }
    Ok(ws)
}

/// A refused or failed upgrade as a `GatewayError`. The daemon's status is in the message, with
/// its `ApiError` message when it sent one; `503 unavailable` maps to `unreachable`.
fn upgrade_error(e: WsError) -> GatewayError {
    match e {
        WsError::Http(response) => {
            let status = response.status().as_u16();
            let said = response
                .body()
                .as_deref()
                .and_then(|body| serde_json::from_slice::<serde_json::Value>(body).ok())
                .and_then(|v| v.get("message").and_then(|m| m.as_str()).map(str::to_owned))
                .map(|m| format!(": {}", redact(&m.chars().take(300).collect::<String>())))
                .unwrap_or_default();
            let message = format!("the daemon refused the socket: HTTP {status}{said}");
            match status {
                503 => GatewayError::unreachable(message),
                400..=499 if status != 401 => GatewayError::invalid(message),
                _ => GatewayError::internal(message),
            }
        }
        WsError::Io(e) => GatewayError::unreachable(format!(
            "the connection to the daemon broke during the upgrade: {e}"
        )),
        WsError::ConnectionClosed | WsError::AlreadyClosed => {
            GatewayError::unreachable("the connection to the daemon closed during the upgrade")
        }
        other => GatewayError::internal(format!("the upgrade failed: {other}")),
    }
}

/// How the gateway ends a socket. These travel apart from the webview's messages, so a full
/// message queue never delays or loses them.
#[derive(Debug)]
pub(crate) enum Control {
    /// Close with this code and reason, then report the close to the webview.
    Close(u16, String),
    /// The webview is gone (closed or reloaded): close with 1001 and report nothing.
    Abandon,
}

/// The back-pressure budget: bytes handed to the webview, and bytes it has taken.
#[derive(Debug)]
struct Budget {
    limit: u64,
    sent: u64,
    taken: u64,
    probing: bool,
}

impl Budget {
    fn pending(&self) -> u64 {
        self.sent - self.taken
    }

    fn fits(&self, weight: usize) -> bool {
        self.pending() + weight as u64 <= self.limit
    }

    fn wants_probe(&self) -> bool {
        !self.probing && self.sent > self.taken
    }
}

/// How a socket ended, for the log.
#[derive(Debug)]
pub(crate) struct Ended {
    pub code: u16,
    pub reported: bool,
}

/// Runs a socket until it closes, then reports the close to the webview (unless it was
/// abandoned). Returns how it ended.
pub(crate) async fn pump(
    mut ws: Ws,
    mut messages: mpsc::Receiver<Message>,
    mut control: mpsc::UnboundedReceiver<Control>,
    sink: Arc<dyn Sink>,
    limits: super::Limits,
) -> Ended {
    let grace = limits.close_grace;
    let (ack_tx, mut acks) = mpsc::unbounded_channel::<u64>();
    let mut budget = Budget {
        limit: limits.backlog as u64,
        sent: 0,
        taken: 0,
        probing: false,
    };
    // The close to report: the first one decided (ours, or the daemon's).
    let mut outcome: Option<(u16, String)> = None;
    let mut report = true;
    // Set once a close frame went out (or came in): wait this long for the end.
    let mut closing: Option<Instant> = None;

    loop {
        let deadline = closing.unwrap_or_else(|| Instant::now() + Duration::from_secs(3600));
        tokio::select! {
            biased;
            command = control.recv(), if closing.is_none() => match command {
                Some(Control::Close(code, reason)) => {
                    closing = Some(start_close(&mut ws, code, &reason, grace).await);
                    outcome.get_or_insert((code, reason));
                }
                Some(Control::Abandon) => {
                    report = false;
                    outcome.get_or_insert((codes::GOING_AWAY, String::new()));
                    closing = Some(start_close(&mut ws, codes::GOING_AWAY, "the page went away", grace).await);
                }
                // The gateway is gone: the app is quitting.
                None => {
                    outcome.get_or_insert((codes::GOING_AWAY, String::new()));
                    closing = Some(start_close(&mut ws, codes::GOING_AWAY, "the app is closing", grace).await);
                }
            },
            Some(upto) = acks.recv() => {
                budget.taken = budget.taken.max(upto);
                budget.probing = false;
                if budget.wants_probe() && !probe(&*sink, &ack_tx, &mut budget) && closing.is_none() {
                    report = false;
                    outcome.get_or_insert((codes::GOING_AWAY, String::new()));
                    closing = Some(start_close(&mut ws, codes::GOING_AWAY, "the page went away", grace).await);
                }
            }
            Some(message) = messages.recv(), if closing.is_none() => {
                // A daemon that stops reading must not hold the pump (and so Close and Abandon).
                let sent = tokio::time::timeout(limits.send_timeout, ws.send(message)).await;
                if !matches!(sent, Ok(Ok(()))) {
                    outcome.get_or_insert((codes::ABNORMAL, String::new()));
                    break;
                }
            }
            frame = ws.next() => match frame {
                Some(Ok(Message::Text(text))) => {
                    let text = text.as_str().to_owned();
                    deliver(Delivery::Text(text), &mut ws, &*sink, &ack_tx, &mut budget, &mut outcome, &mut closing, &mut report, grace).await;
                }
                Some(Ok(Message::Binary(data))) => {
                    deliver(Delivery::Binary(data.to_vec()), &mut ws, &*sink, &ack_tx, &mut budget, &mut outcome, &mut closing, &mut report, grace).await;
                }
                Some(Ok(Message::Close(frame))) => {
                    // tungstenite queues the reply and sends it on the next read.
                    let (code, reason) = frame.map_or((codes::NO_STATUS, String::new()), |f| {
                        (u16::from(f.code), f.reason.as_str().to_owned())
                    });
                    outcome.get_or_insert((code, reason));
                    if closing.is_none() {
                        closing = Some(Instant::now() + grace);
                    }
                }
                // Ping (answered by tungstenite), Pong, raw frames: not the webview's business.
                Some(Ok(_)) => {}
                Some(Err(WsError::Capacity(_))) => {
                    outcome.get_or_insert((codes::TOO_BIG, "message too big".into()));
                    let _ = start_close(&mut ws, codes::TOO_BIG, "message too big", grace).await;
                    break;
                }
                Some(Err(_)) => {
                    outcome.get_or_insert((codes::ABNORMAL, String::new()));
                    break;
                }
                None => break,
            },
            () = tokio::time::sleep_until(deadline), if closing.is_some() => break,
        }
    }

    let (code, reason) = outcome.unwrap_or((codes::ABNORMAL, String::new()));
    if report {
        let _ = sink.deliver(Delivery::Close {
            code,
            reason: reason.clone(),
        });
    }
    Ended {
        code,
        reported: report,
    }
}

/// Hands one frame to the webview, or closes with 1013 when it would overflow the budget. Frames
/// after a close has started are dropped.
#[allow(clippy::too_many_arguments)]
async fn deliver(
    message: Delivery,
    ws: &mut Ws,
    sink: &dyn Sink,
    ack_tx: &mpsc::UnboundedSender<u64>,
    budget: &mut Budget,
    outcome: &mut Option<(u16, String)>,
    closing: &mut Option<Instant>,
    report: &mut bool,
    grace: Duration,
) {
    if closing.is_some() {
        return;
    }
    let weight = message.weight();
    if !budget.fits(weight) {
        let reason = "the app is not keeping up; reconnect";
        outcome.get_or_insert((codes::TRY_AGAIN_LATER, reason.into()));
        *closing = Some(start_close(ws, codes::TRY_AGAIN_LATER, reason, grace).await);
        return;
    }
    if sink.deliver(message).is_err() {
        *report = false;
        outcome.get_or_insert((codes::GOING_AWAY, String::new()));
        *closing = Some(start_close(ws, codes::GOING_AWAY, "the page went away", grace).await);
        return;
    }
    budget.sent += weight as u64;
    if budget.wants_probe() && !probe(sink, ack_tx, budget) {
        // The webview is gone: close as the acknowledgement path does.
        *report = false;
        outcome.get_or_insert((codes::GOING_AWAY, String::new()));
        *closing = Some(start_close(ws, codes::GOING_AWAY, "the page went away", grace).await);
    }
}

/// Starts a probe for everything delivered so far. False if the webview is gone.
fn probe(sink: &dyn Sink, ack_tx: &mpsc::UnboundedSender<u64>, budget: &mut Budget) -> bool {
    let upto = budget.sent;
    let ack_tx = ack_tx.clone();
    budget.probing = true;
    sink.probe(Box::new(move || {
        let _ = ack_tx.send(upto);
    }))
    .is_ok()
}

/// Sends a close frame, waiting at most `grace` for it to go out. Returns until when to wait for
/// the daemon's end of the handshake: `grace` from now, or now when the frame could not be sent.
async fn start_close(ws: &mut Ws, code: u16, reason: &str, grace: Duration) -> Instant {
    let frame = CloseFrame {
        code: CloseCode::from(code),
        reason: reason.to_owned().into(),
    };
    match tokio::time::timeout(grace, ws.send(Message::Close(Some(frame)))).await {
        Ok(Ok(())) => Instant::now() + grace,
        _ => Instant::now(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_budget_counts_what_the_webview_has_not_taken() {
        let mut b = Budget {
            limit: 10,
            sent: 0,
            taken: 0,
            probing: false,
        };
        assert!(b.fits(10));
        assert!(!b.fits(11));
        b.sent = 8;
        assert!(b.wants_probe());
        assert!(b.fits(2));
        assert!(!b.fits(3));
        b.probing = true;
        assert!(!b.wants_probe());
        b.taken = 8;
        b.probing = false;
        assert_eq!(b.pending(), 0);
        assert!(!b.wants_probe());
        assert!(b.fits(10));
    }

    #[test]
    fn upgrade_errors_map_to_the_contract() {
        use tokio_tungstenite::tungstenite::http::Response;
        let refused = |status: u16, body: &str| {
            let mut response = Response::new(Some(body.as_bytes().to_vec()));
            *response.status_mut() = http::StatusCode::from_u16(status).unwrap();
            upgrade_error(WsError::Http(Box::new(response)))
        };
        let e = refused(
            503,
            r#"{"code":"unavailable","message":"the session's machine is unreachable"}"#,
        );
        assert_eq!(e.code, super::super::error::ErrorCode::Unreachable);
        assert_eq!(
            e.message,
            "the daemon refused the socket: HTTP 503: the session's machine is unreachable"
        );
        let e = refused(404, r#"{"code":"not_found","message":"no such session"}"#);
        assert_eq!(e.code, super::super::error::ErrorCode::Invalid);
        assert!(e.message.contains("HTTP 404"), "{}", e.message);
        assert_eq!(
            refused(500, "").code,
            super::super::error::ErrorCode::Internal
        );
        assert_eq!(
            refused(401, "").code,
            super::super::error::ErrorCode::Internal
        );
    }
}
