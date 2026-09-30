//! `GET /v1/sessions/{id}/terminal?cols=&rows=&from=`: a session's live terminal over a
//! WebSocket.
//!
//! - The [`Terminals`] seam finds a session's terminal and returns an [`Attachment`]:
//!   offset-addressed output, input, resize and exit. [`RuntimeTerminals`] implements it over any
//!   `Runtime` (the runner, stream D, will provide the session-to-terminal links).
//! - Output is polled from the runtime (it has no notifications) and sent as binary frames of at
//!   most [`TerminalConfig::max_frame`] bytes, starting at `from` (default 0, the start of the
//!   buffer). If the buffer no longer holds `from`, a `{"type":"truncated","from":N}` text frame
//!   comes first, and the bytes start at `N`. A client that counts the bytes it received
//!   reconnects with `from` = that offset and misses nothing.
//! - Client binary frames are keystrokes, written as-is. Client text frames are control messages:
//!   `{"type":"resize","cols":…,"rows":…}`; other `type`s are ignored; malformed JSON (or a
//!   malformed resize) closes the socket with 1007.
//! - When the program exits, the remaining output is sent, then `{"type":"exit"}`, then a
//!   normal close (1000).
//! - **Backpressure:** each client has a bounded queue. A client that stops reading is closed
//!   with 1013 and resumes by offset.
//! - **Several clients** may attach to one terminal. Each gets the output independently. All may
//!   type; their input is written in the order each message arrives, so keystrokes from two
//!   people can interleave. A single-writer rule, if wanted, belongs to the runner.

use crate::util::{close, close_code};
use axum::Router;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use pitcrew_auth::{ErrorResponse, WS_PROTOCOL};
use pitcrew_interfaces::runtime::{OutputChunk, Runtime, RuntimeError};
use pitcrew_protocol::api::ErrorCode;
use pitcrew_protocol::ids::{SessionId, TerminalId};
use serde::Deserialize;
use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, PoisonError, RwLock};
use std::time::Duration;
use tokio::sync::mpsc;

/// Failures of the terminal seam.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalError {
    /// No such session, or it has no terminal (`404 not_found`).
    NotFound(String),
    /// The session's machine or runtime cannot be reached (`503 unavailable`).
    Unavailable(String),
    /// Anything else (`500 internal`).
    Failed(String),
}

impl fmt::Display for TerminalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound(m) => write!(f, "not found: {m}"),
            Self::Unavailable(m) => write!(f, "unavailable: {m}"),
            Self::Failed(m) => write!(f, "failed: {m}"),
        }
    }
}

impl std::error::Error for TerminalError {}

impl TerminalError {
    fn response(&self) -> ErrorResponse {
        match self {
            Self::NotFound(m) => ErrorResponse::not_found(m.clone()),
            Self::Unavailable(m) => ErrorResponse::new(ErrorCode::Unavailable, m.clone()),
            Self::Failed(m) => ErrorResponse::new(ErrorCode::Internal, m.clone()),
        }
    }
}

impl From<RuntimeError> for TerminalError {
    fn from(error: RuntimeError) -> Self {
        match error {
            RuntimeError::NotFound(id) => Self::NotFound(format!("No terminal {id}.")),
            RuntimeError::Unavailable(m) => Self::Unavailable(m),
            other => Self::Failed(other.to_string()),
        }
    }
}

/// Finds the terminal of a session.
pub trait Terminals: Send + Sync + fmt::Debug + 'static {
    /// Attaches to the session's terminal. Blocking.
    ///
    /// # Errors
    /// [`TerminalError::NotFound`] if the session has no terminal;
    /// [`TerminalError::Unavailable`] if its machine cannot be reached.
    fn attach(&self, session: SessionId) -> Result<Arc<dyn Attachment>, TerminalError>;
}

/// One terminal, as the route uses it. All methods are blocking.
pub trait Attachment: Send + Sync + fmt::Debug {
    /// Output from `from`, at most `max` bytes (see `OutputChunk`).
    ///
    /// # Errors
    /// The terminal is gone or cannot be read.
    fn read(&self, from: u64, max: usize) -> Result<OutputChunk, TerminalError>;

    /// Writes keystrokes as-is.
    ///
    /// # Errors
    /// The terminal is gone or cannot be written.
    fn write(&self, bytes: &[u8]) -> Result<(), TerminalError>;

    /// Resizes the terminal.
    ///
    /// # Errors
    /// The terminal is gone or cannot be resized.
    fn resize(&self, cols: u16, rows: u16) -> Result<(), TerminalError>;

    /// Whether the program has exited.
    ///
    /// # Errors
    /// The terminal cannot be queried.
    fn exited(&self) -> Result<bool, TerminalError>;
}

/// [`Terminals`] over a `Runtime`, with an explicit map from sessions to terminals.
pub struct RuntimeTerminals<R: ?Sized> {
    runtime: Arc<R>,
    sessions: RwLock<HashMap<SessionId, TerminalId>>,
}

impl<R: ?Sized> fmt::Debug for RuntimeTerminals<R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let sessions = self
            .sessions
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .len();
        f.debug_struct("RuntimeTerminals")
            .field("sessions", &sessions)
            .finish_non_exhaustive()
    }
}

impl<R: Runtime + ?Sized + 'static> RuntimeTerminals<R> {
    /// No sessions linked yet.
    #[must_use]
    pub fn new(runtime: Arc<R>) -> Self {
        Self {
            runtime,
            sessions: RwLock::new(HashMap::new()),
        }
    }

    /// Records that `session` runs in `terminal`.
    pub fn link(&self, session: SessionId, terminal: TerminalId) {
        self.sessions
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(session, terminal);
    }

    /// Forgets the session's terminal.
    pub fn unlink(&self, session: SessionId) {
        self.sessions
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&session);
    }
}

impl<R: Runtime + ?Sized + 'static> Terminals for RuntimeTerminals<R> {
    fn attach(&self, session: SessionId) -> Result<Arc<dyn Attachment>, TerminalError> {
        let terminal = self
            .sessions
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&session)
            .copied()
            .ok_or_else(|| {
                TerminalError::NotFound(format!("Session {session} has no terminal."))
            })?;
        // Fails with NotFound if the runtime no longer knows the terminal.
        self.runtime.info(terminal)?;
        Ok(Arc::new(RuntimeAttachment {
            runtime: Arc::clone(&self.runtime),
            terminal,
        }))
    }
}

struct RuntimeAttachment<R: ?Sized> {
    runtime: Arc<R>,
    terminal: TerminalId,
}

impl<R: ?Sized> fmt::Debug for RuntimeAttachment<R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RuntimeAttachment")
            .field("terminal", &self.terminal)
            .finish_non_exhaustive()
    }
}

impl<R: Runtime + ?Sized> Attachment for RuntimeAttachment<R> {
    fn read(&self, from: u64, max: usize) -> Result<OutputChunk, TerminalError> {
        Ok(self.runtime.read_output(self.terminal, from, max)?)
    }

    fn write(&self, bytes: &[u8]) -> Result<(), TerminalError> {
        Ok(self.runtime.write(self.terminal, bytes)?)
    }

    fn resize(&self, cols: u16, rows: u16) -> Result<(), TerminalError> {
        Ok(self.runtime.resize(self.terminal, cols, rows)?)
    }

    fn exited(&self) -> Result<bool, TerminalError> {
        Ok(!self.runtime.info(self.terminal)?.alive)
    }
}

/// Tuning for terminal streaming.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TerminalConfig {
    /// Frames a client may have queued but unsent; with `max_frame`, this bounds its memory.
    pub queue_frames: usize,
    /// How long a frame may wait for room (or for the socket) before the client is dropped.
    pub send_timeout: Duration,
    /// The largest binary frame sent, in bytes.
    pub max_frame: usize,
    /// How often to poll the runtime for output when there is none.
    pub poll_every: Duration,
    /// The largest message a client may send (keystrokes, pastes, control).
    pub max_inbound: usize,
}

impl Default for TerminalConfig {
    fn default() -> Self {
        Self {
            queue_frames: 64,
            send_timeout: Duration::from_secs(10),
            max_frame: 64 * 1024,
            poll_every: Duration::from_millis(20),
            max_inbound: 1024 * 1024,
        }
    }
}

/// Why a terminal stream ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerminalEnd {
    /// The client went away.
    ClientGone,
    /// The client stopped reading.
    SlowClient,
    /// The program exited; everything was sent.
    Exited,
    /// The client sent malformed control JSON.
    Malformed,
    /// The runtime failed.
    Failed(TerminalError),
}

/// The terminal route. Mount it as a **device** route (`RouterParts::device`).
pub fn routes(terminals: Arc<dyn Terminals>, config: TerminalConfig) -> Router {
    Router::new()
        .route("/v1/sessions/{id}/terminal", get(terminal))
        .with_state(TerminalState { terminals, config })
}

#[derive(Clone, Debug)]
struct TerminalState {
    terminals: Arc<dyn Terminals>,
    config: TerminalConfig,
}

#[derive(Debug, Deserialize)]
struct TerminalQuery {
    cols: Option<u16>,
    rows: Option<u16>,
    from: Option<u64>,
}

async fn terminal(
    State(state): State<TerminalState>,
    path: Result<Path<String>, axum::extract::rejection::PathRejection>,
    query: Result<Query<TerminalQuery>, axum::extract::rejection::QueryRejection>,
    upgrade: Result<WebSocketUpgrade, axum::extract::ws::rejection::WebSocketUpgradeRejection>,
) -> Response {
    let invalid = |m: &str| ErrorResponse::new(ErrorCode::Invalid, m).into_response();
    let session = match path.map(|Path(id)| id.parse::<SessionId>()) {
        Ok(Ok(session)) => session,
        _ => return ErrorResponse::not_found("No such session.").into_response(),
    };
    let Ok(Query(query)) = query else {
        return invalid("`cols` and `rows` must be sizes and `from` an offset.");
    };
    let size = match (query.cols, query.rows) {
        (None, None) => None,
        (Some(cols), Some(rows)) if cols > 0 && rows > 0 => Some((cols, rows)),
        _ => return invalid("Give both `cols` and `rows`, each at least 1."),
    };
    let terminals = Arc::clone(&state.terminals);
    let attached = blocking(move || {
        let attachment = terminals.attach(session)?;
        if let Some((cols, rows)) = size {
            attachment.resize(cols, rows)?;
        }
        Ok(attachment)
    })
    .await;
    let attachment = match attached {
        Ok(attachment) => attachment,
        Err(e) => return e.response().into_response(),
    };
    let Ok(upgrade) = upgrade else {
        return invalid("GET /v1/sessions/{id}/terminal needs a WebSocket upgrade.");
    };
    let config = state.config;
    upgrade
        .protocols([WS_PROTOCOL])
        .max_message_size(config.max_inbound)
        .on_upgrade(move |socket| session_loop(socket, attachment, query.from.unwrap_or(0), config))
}

/// A control message from the client.
#[derive(Debug, PartialEq, Eq)]
enum Control {
    Resize(u16, u16),
    Ignore,
    Malformed,
}

fn control(text: &str) -> Control {
    let Ok(serde_json::Value::Object(message)) = serde_json::from_str(text) else {
        return Control::Malformed;
    };
    match message.get("type").and_then(serde_json::Value::as_str) {
        Some("resize") => {
            let size = |key: &str| {
                message
                    .get(key)
                    .and_then(serde_json::Value::as_u64)
                    .and_then(|n| u16::try_from(n).ok())
                    .filter(|n| *n > 0)
            };
            match (size("cols"), size("rows")) {
                (Some(cols), Some(rows)) => Control::Resize(cols, rows),
                _ => Control::Malformed,
            }
        }
        Some(_) => Control::Ignore,
        None => Control::Malformed,
    }
}

async fn session_loop(
    mut socket: WebSocket,
    attachment: Arc<dyn Attachment>,
    from: u64,
    config: TerminalConfig,
) {
    let (frames, mut queue) = mpsc::channel(config.queue_frames.max(1));
    let pump = pump(Arc::clone(&attachment), from, config, frames);
    tokio::pin!(pump);
    let end = loop {
        tokio::select! {
            end = &mut pump => break end,
            Some(frame) = queue.recv() => {
                if let Err(end) = send(&mut socket, frame, config.send_timeout).await {
                    break end;
                }
            }
            incoming = socket.recv() => match incoming {
                None | Some(Err(_) | Ok(Message::Close(_))) => break TerminalEnd::ClientGone,
                Some(Ok(Message::Binary(bytes))) => {
                    let attachment = Arc::clone(&attachment);
                    if let Err(e) = blocking(move || attachment.write(&bytes)).await {
                        break TerminalEnd::Failed(e);
                    }
                }
                Some(Ok(Message::Text(text))) => match control(&text) {
                    Control::Resize(cols, rows) => {
                        let attachment = Arc::clone(&attachment);
                        if let Err(e) = blocking(move || attachment.resize(cols, rows)).await {
                            break TerminalEnd::Failed(e);
                        }
                    }
                    Control::Ignore => {}
                    Control::Malformed => break TerminalEnd::Malformed,
                },
                Some(Ok(_)) => {}
            },
        }
    };
    tracing::debug!(?end, "terminal stream closed");
    let (code, reason) = match &end {
        TerminalEnd::ClientGone => return,
        TerminalEnd::Exited => {
            // The pump has finished; send what it queued, ending with `exit`.
            while let Ok(frame) = queue.try_recv() {
                if send(&mut socket, frame, config.send_timeout).await.is_err() {
                    return;
                }
            }
            (close_code::NORMAL, "exited")
        }
        TerminalEnd::SlowClient => (
            close_code::TRY_AGAIN_LATER,
            "too slow; resume from your offset",
        ),
        TerminalEnd::Malformed => (close_code::INVALID_DATA, "malformed control message"),
        TerminalEnd::Failed(_) => (close_code::INTERNAL, "the terminal failed"),
    };
    close(&mut socket, code, reason).await;
}

async fn send(socket: &mut WebSocket, frame: Message, wait: Duration) -> Result<(), TerminalEnd> {
    match tokio::time::timeout(wait, socket.send(frame)).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) => Err(TerminalEnd::ClientGone),
        Err(_) => Err(TerminalEnd::SlowClient),
    }
}

/// Reads output from `from` and queues it as frames until the program exits, the client goes
/// away or falls behind, or the runtime fails.
pub async fn pump(
    attachment: Arc<dyn Attachment>,
    from: u64,
    config: TerminalConfig,
    out: mpsc::Sender<Message>,
) -> TerminalEnd {
    let max_frame = config.max_frame.clamp(1, 1024 * 1024);
    let poll_every = config.poll_every.max(Duration::from_millis(1));
    let queue = |frame: Message| {
        let out = out.clone();
        async move {
            match tokio::time::timeout(config.send_timeout, out.send(frame)).await {
                Ok(Ok(())) => Ok(()),
                Ok(Err(_)) => Err(TerminalEnd::ClientGone),
                Err(_) => Err(TerminalEnd::SlowClient),
            }
        }
    };
    let mut offset = from;
    loop {
        // Ask whether it exited *before* reading, so output written just before the exit is
        // read in this round.
        let attached = Arc::clone(&attachment);
        let read = blocking(move || {
            let exited = attached.exited()?;
            let chunk = attached.read(offset, max_frame)?;
            Ok((exited, chunk))
        })
        .await;
        let (exited, chunk) = match read {
            Ok(read) => read,
            Err(e) => return TerminalEnd::Failed(e),
        };
        // A buffer that lost `offset`, or a `from` past the end: tell the client where the bytes
        // really start, so its offset stays right.
        if chunk.truncated || chunk.offset != offset {
            let text = serde_json::json!({"type": "truncated", "from": chunk.offset}).to_string();
            if let Err(end) = queue(Message::Text(text.into())).await {
                return end;
            }
        }
        offset = chunk.offset + chunk.data.len() as u64;
        let more = offset < chunk.end;
        if !chunk.data.is_empty()
            && let Err(end) = queue(Message::Binary(chunk.data.into())).await
        {
            return end;
        }
        if more {
            continue;
        }
        if exited {
            let text = serde_json::json!({"type": "exit"}).to_string();
            return match queue(Message::Text(text.into())).await {
                Ok(()) => TerminalEnd::Exited,
                Err(end) => end,
            };
        }
        tokio::select! {
            () = tokio::time::sleep(poll_every) => {}
            () = out.closed() => return TerminalEnd::ClientGone,
        }
    }
}

/// Runs a blocking call off the async threads.
async fn blocking<T, F>(f: F) -> Result<T, TerminalError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, TerminalError> + Send + 'static,
{
    tokio::task::spawn_blocking(f).await.unwrap_or_else(|e| {
        Err(TerminalError::Failed(format!(
            "the runtime call panicked: {e}"
        )))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pitcrew_interfaces::fake::FakeRuntime;
    use pitcrew_interfaces::runtime::StartSpec;

    fn started() -> (Arc<FakeRuntime>, TerminalId) {
        let runtime = Arc::new(FakeRuntime::default());
        let info = runtime
            .start(&StartSpec {
                program: "agent".into(),
                args: vec![],
                cwd: "/work".into(),
                env: vec![],
                name: "t".into(),
                cols: 80,
                rows: 24,
            })
            .unwrap();
        (runtime, info.id)
    }

    #[test]
    fn control_messages() {
        assert_eq!(
            control(r#"{"type":"resize","cols":120,"rows":40}"#),
            Control::Resize(120, 40)
        );
        assert_eq!(control(r#"{"type":"focus"}"#), Control::Ignore);
        for bad in [
            "{not json",
            "[]",
            r#"{"cols":1}"#,
            r#"{"type":"resize","cols":0,"rows":1}"#,
            r#"{"type":"resize","cols":70000,"rows":1}"#,
            r#"{"type":"resize"}"#,
        ] {
            assert_eq!(control(bad), Control::Malformed, "{bad}");
        }
    }

    #[test]
    fn unknown_sessions_and_terminals_are_not_found() {
        let (runtime, terminal) = started();
        let terminals = RuntimeTerminals::new(runtime.clone());
        let session = SessionId::new();
        assert!(matches!(
            terminals.attach(session),
            Err(TerminalError::NotFound(_))
        ));
        terminals.link(session, TerminalId::new());
        assert!(matches!(
            terminals.attach(session),
            Err(TerminalError::NotFound(_))
        ));
        terminals.link(session, terminal);
        assert!(terminals.attach(session).is_ok());
        terminals.unlink(session);
        assert!(terminals.attach(session).is_err());
    }

    #[tokio::test]
    async fn frames_are_capped_and_exit_comes_last() {
        let (runtime, terminal) = started();
        runtime.write(terminal, &[b'x'; 10]).unwrap();
        let terminals = RuntimeTerminals::new(runtime.clone());
        let session = SessionId::new();
        terminals.link(session, terminal);
        let config = TerminalConfig {
            max_frame: 4,
            poll_every: Duration::from_millis(1),
            ..TerminalConfig::default()
        };
        let (frames, mut queue) = mpsc::channel(16);
        let task = tokio::spawn(pump(terminals.attach(session).unwrap(), 0, config, frames));
        let mut sizes = Vec::new();
        while sizes.iter().sum::<usize>() < 10 {
            match queue.recv().await.unwrap() {
                Message::Binary(bytes) => sizes.push(bytes.len()),
                other => panic!("unexpected {other:?}"),
            }
        }
        assert_eq!(sizes, vec![4, 4, 2]);
        runtime.kill(terminal).unwrap();
        assert_eq!(
            queue.recv().await.unwrap(),
            Message::Text(r#"{"type":"exit"}"#.into())
        );
        assert_eq!(task.await.unwrap(), TerminalEnd::Exited);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_slow_client_is_dropped_and_others_keep_receiving() {
        let (runtime, terminal) = started();
        let terminals = RuntimeTerminals::new(runtime.clone());
        let session = SessionId::new();
        terminals.link(session, terminal);
        let slow_config = TerminalConfig {
            queue_frames: 2,
            send_timeout: Duration::from_millis(100),
            max_frame: 1,
            poll_every: Duration::from_millis(1),
            ..TerminalConfig::default()
        };
        let fast_config = TerminalConfig {
            send_timeout: Duration::from_secs(10),
            ..slow_config
        };
        let (slow_frames, slow_queue) = mpsc::channel(slow_config.queue_frames);
        let (fast_frames, mut fast_queue) = mpsc::channel(fast_config.queue_frames);
        let slow = tokio::spawn(pump(
            terminals.attach(session).unwrap(),
            0,
            slow_config,
            slow_frames,
        ));
        let fast = tokio::spawn(pump(
            terminals.attach(session).unwrap(),
            0,
            fast_config,
            fast_frames,
        ));
        let mut received = Vec::new();
        for byte in 0u8..20 {
            runtime.write(terminal, &[byte]).unwrap();
            while let Ok(Some(Message::Binary(bytes))) =
                tokio::time::timeout(Duration::from_millis(20), fast_queue.recv()).await
            {
                received.extend_from_slice(&bytes);
            }
        }
        let end = tokio::time::timeout(Duration::from_secs(5), slow)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(end, TerminalEnd::SlowClient);
        assert_eq!(slow_queue.max_capacity(), 2);
        assert!(slow_queue.len() <= 2);
        while received.len() < 20 {
            match tokio::time::timeout(Duration::from_secs(1), fast_queue.recv()).await {
                Ok(Some(Message::Binary(bytes))) => received.extend_from_slice(&bytes),
                other => panic!("fast client stalled: {other:?}"),
            }
        }
        assert_eq!(received, (0u8..20).collect::<Vec<_>>());
        assert!(!fast.is_finished());
    }
}
