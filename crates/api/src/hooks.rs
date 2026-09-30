//! `POST /v1/hooks/{engine}/{event}`: agent hooks report session events.
//!
//! The route validates the request, answers `202` at once, and hands the payload to a
//! [`HookSink`] through a bounded channel. When the channel is full the event is dropped and
//! counted, so a hook never blocks its agent.

use crate::util::now_ms;
use axum::Router;
use axum::body::Bytes;
use axum::extract::rejection::{BytesRejection, PathRejection};
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::StatusCode;
use axum::routing::post;
use pitcrew_auth::{Authenticated, ErrorResponse};
use pitcrew_protocol::api::{Caller, ErrorCode};
use pitcrew_protocol::model::{Engine, TimestampMs};
use std::fmt;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::mpsc;

/// The largest hook body accepted.
pub const MAX_BODY: usize = 1 << 20;

/// One hook call, as received.
#[derive(Clone, Debug, PartialEq)]
pub struct HookEvent {
    /// The engine that ran the hook.
    pub engine: Engine,
    /// The engine's own event name, e.g. `SessionStart`.
    pub event: String,
    /// Who sent it (from the token).
    pub caller: Caller,
    /// The hook's JSON payload.
    pub payload: serde_json::Map<String, serde_json::Value>,
    /// When the hub received it.
    pub received_at: TimestampMs,
}

/// Receives hook events. The runner (stream D) implements it; until then, [`LogHookSink`].
///
/// `deliver` runs on a dedicated thread, one event at a time, so it **may block** (e.g. write to
/// SQLite). While it blocks, new events queue up to the intake's capacity and then are dropped.
/// A panic in `deliver` is logged and the next event is delivered as usual.
pub trait HookSink: Send + Sync + fmt::Debug + 'static {
    /// Handles one event.
    fn deliver(&self, event: HookEvent);
}

/// A sink that only logs what it receives.
#[derive(Clone, Copy, Debug, Default)]
pub struct LogHookSink;

impl HookSink for LogHookSink {
    fn deliver(&self, event: HookEvent) {
        tracing::debug!(
            engine = ?event.engine,
            event = %event.event,
            member = %event.caller.member,
            "hook received"
        );
    }
}

/// The intake: the bounded channel in front of a sink, and the count of dropped events.
#[derive(Clone, Debug)]
pub struct HookIntake {
    queue: mpsc::Sender<HookEvent>,
    dropped: Arc<AtomicU64>,
}

impl HookIntake {
    /// Starts a thread that feeds `sink` from a channel holding at most `capacity` events. The
    /// thread ends when every `HookIntake` clone is dropped.
    ///
    /// # Errors
    /// The thread cannot be spawned.
    pub fn start(sink: Arc<dyn HookSink>, capacity: usize) -> std::io::Result<Self> {
        let (queue, mut events) = mpsc::channel::<HookEvent>(capacity.max(1));
        std::thread::Builder::new()
            .name("pitcrew-hook-sink".to_owned())
            .spawn(move || {
                while let Some(event) = events.blocking_recv() {
                    let delivered =
                        std::panic::catch_unwind(AssertUnwindSafe(|| sink.deliver(event)));
                    if delivered.is_err() {
                        tracing::error!("the hook sink panicked; continuing with the next event");
                    }
                }
            })?;
        Ok(Self {
            queue,
            dropped: Arc::new(AtomicU64::new(0)),
        })
    }

    /// A channel that nothing reads, for tests: it fills after `capacity` events.
    #[doc(hidden)]
    #[must_use]
    pub fn unread(capacity: usize) -> (Self, mpsc::Receiver<HookEvent>) {
        let (queue, events) = mpsc::channel(capacity.max(1));
        let intake = Self {
            queue,
            dropped: Arc::new(AtomicU64::new(0)),
        };
        (intake, events)
    }

    /// How many events were dropped because the channel was full.
    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    fn offer(&self, event: HookEvent) {
        if self.queue.try_send(event).is_err() {
            let total = self.dropped.fetch_add(1, Ordering::Relaxed) + 1;
            tracing::warn!(
                dropped = total,
                "hook channel full or closed; dropped an event"
            );
        }
    }
}

/// The hook route. Mount it as an **agent** route (`RouterParts::agent`).
pub fn routes(intake: HookIntake) -> Router {
    Router::new()
        .route("/v1/hooks/{engine}/{event}", post(receive))
        .layer(DefaultBodyLimit::max(MAX_BODY))
        .with_state(intake)
}

async fn receive(
    State(intake): State<HookIntake>,
    Authenticated(caller): Authenticated,
    path: Result<Path<(String, String)>, PathRejection>,
    body: Result<Bytes, BytesRejection>,
) -> Result<StatusCode, ErrorResponse> {
    let invalid = |message: String| ErrorResponse::new(ErrorCode::Invalid, message);
    let Path((engine, event)) =
        path.map_err(|_| invalid("The engine and event must be plain text.".to_owned()))?;
    let engine: Engine = serde_json::from_value(serde_json::Value::String(engine.clone()))
        .map_err(|_| invalid(format!("Unknown engine {engine:?}.")))?;
    if !is_event_name(&event) {
        return Err(invalid(format!(
            "Event names match [A-Za-z][A-Za-z0-9_-]{{0,63}}; got {:?}.",
            event.chars().take(80).collect::<String>()
        )));
    }
    let body = body.map_err(|rejection| {
        if rejection.status() == StatusCode::PAYLOAD_TOO_LARGE {
            invalid(format!("The body must be at most {MAX_BODY} bytes."))
        } else {
            invalid("The body could not be read.".to_owned())
        }
    })?;
    let payload = match serde_json::from_slice(&body) {
        Ok(serde_json::Value::Object(payload)) => payload,
        _ => return Err(invalid("The body must be a JSON object.".to_owned())),
    };
    intake.offer(HookEvent {
        engine,
        event,
        caller,
        payload,
        received_at: now_ms(),
    });
    Ok(StatusCode::ACCEPTED)
}

/// `[A-Za-z][A-Za-z0-9_-]{0,63}`.
fn is_event_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && name.len() <= 64
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

#[cfg(test)]
mod tests {
    use super::is_event_name;

    #[test]
    fn event_names() {
        for good in [
            "SessionStart",
            "Stop",
            "a",
            "pre_tool-use2",
            &"a".repeat(64),
        ] {
            assert!(is_event_name(good), "{good}");
        }
        for bad in ["", "1abc", "_x", "a b", "a.b", "ä", &"a".repeat(65)] {
            assert!(!is_event_name(bad), "{bad}");
        }
    }
}
