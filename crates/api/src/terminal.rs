//! `GET /v1/sessions/{id}/terminal?cols=&rows=&from=`: a session's live terminal over a
//! WebSocket.
//!
//! - The [`Terminals`] seam finds a session's terminal and returns an [`Attachment`]:
//!   offset-addressed output, input, resize and exit. [`RuntimeTerminals`] implements it over any
//!   `Runtime` (the runner, stream D, will provide the session-to-terminal links). Read the
//!   contract on [`Attachment`] before implementing it.
//! - `cols` and `rows` are in [`SIZES`] (400 outside it). Nothing touches the terminal until the
//!   request is known to be a WebSocket upgrade: the initial resize comes after that check.
//! - Output is sent as binary frames of at most [`TerminalConfig::max_frame`] bytes, starting at
//!   `from` (default 0, the start of the buffer). If the buffer no longer holds `from`, a
//!   `{"type":"truncated","from":N}` text frame comes first, and the bytes start at `N`. A client
//!   that counts the bytes it received reconnects with `from` = that offset and misses nothing.
//! - Output is waited for with [`Attachment::changes`] when the attachment offers it, and
//!   otherwise polled, backing off while idle (from [`TerminalConfig::poll_min`] to
//!   [`TerminalConfig::poll_max`]) and starting over on any input or output.
//! - Client binary frames are keystrokes, written as-is. Client text frames are control messages:
//!   `{"type":"resize","cols":…,"rows":…}`; other `type`s are ignored; malformed JSON, or a resize
//!   outside [`SIZES`], closes the socket with 1007. A message over
//!   [`TerminalConfig::max_inbound`] closes it with 1009.
//! - Input is written in arrival order by its own loop, through a queue of 16 messages, so a
//!   terminal that is slow to take input never holds up output or pings. While the queue is full
//!   the socket is not read; a write that times out ends the stream with 1011.
//! - When the program exits (or its terminal disappears), the remaining output is sent, then
//!   `{"type":"exit"}`, then a normal close (1000).
//! - **Backpressure:** each client has a bounded queue. A client that stops reading is closed
//!   with 1013 and resumes by offset.
//! - **Keepalive:** a WebSocket Ping every [`TerminalConfig::ping_every`]; a client that sends no
//!   Pong within [`TerminalConfig::pong_timeout`] is closed with 1013, like a slow one. That time
//!   only counts while the socket is read: while input backs up, the client's Pong may be queued
//!   behind keystrokes the route has not read yet, so the deadline waits.
//! - **Bounded calls:** every call into the seam runs on the blocking pool, at most
//!   [`TerminalConfig::max_calls`] at once across all clients, and is given up after
//!   [`TerminalConfig::call_timeout`] (503 before the upgrade, 1011 after it). A stalled runtime
//!   therefore ties up at most `max_calls` blocking threads. The limit is shared by every
//!   terminal of the route **on purpose**: once `max_calls` calls hang on one runtime, calls for
//!   other terminals wait for a permit too (and time out), rather than tying up more threads.
//! - 1001 when the hub shuts down; 1011 when the runtime fails. A client's Close is answered
//!   before the socket is dropped.
//! - **Several clients** may attach to one terminal. Each gets the output independently. All may
//!   type; their input is written in the order each message arrives, so keystrokes from two
//!   people can interleave. A single-writer rule, if wanted, belongs to the runner.

use crate::util::{HubShutdown, close, close_code, finish, too_big};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Extension, Router};
use pitcrew_auth::{ErrorResponse, WS_PROTOCOL};
use pitcrew_interfaces::runtime::{OutputChunk, Runtime, RuntimeError};
use pitcrew_protocol::api::ErrorCode;
use pitcrew_protocol::ids::{SessionId, TerminalId};
use serde::Deserialize;
use std::collections::HashMap;
use std::fmt;
use std::ops::RangeInclusive;
use std::sync::{Arc, PoisonError, RwLock};
use std::time::Duration;
use tokio::sync::{Notify, Semaphore, mpsc, watch};
use tokio::time::{Instant, MissedTickBehavior};

/// The terminal sizes the API accepts, for `cols` and `rows` alike.
pub const SIZES: RangeInclusive<u16> = 1..=1000;

/// Failures of the terminal seam.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalError {
    /// No such session, or it has no terminal (`404 not_found`). During a stream: the terminal
    /// is gone, which counts as the program having exited.
    NotFound(String),
    /// The session's machine or runtime cannot be reached, or did not answer in time
    /// (`503 unavailable`).
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
    /// Attaches to the session's terminal. Blocking, but it must return within bounded time, as
    /// every [`Attachment`] method must.
    ///
    /// # Errors
    /// [`TerminalError::NotFound`] if the session has no terminal;
    /// [`TerminalError::Unavailable`] if its machine cannot be reached.
    fn attach(&self, session: SessionId) -> Result<Arc<dyn Attachment>, TerminalError>;
}

/// One terminal, as the route uses it.
///
/// # Contract
///
/// - **Blocking, but bounded.** Every method may block (the route calls them on the blocking
///   pool), but must return within bounded time: a runtime that cannot answer returns
///   [`TerminalError::Unavailable`] instead of hanging. The route gives up on a call after
///   [`TerminalConfig::call_timeout`] anyway, but a call that never returns keeps its thread.
/// - **Exit after output.** [`exited`](Self::exited) may return `true` only once all of the
///   program's output is readable with [`read`](Self::read). The route stops at the first read
///   that finds nothing after `exited` said `true`.
/// - **Gone means ended.** [`TerminalError::NotFound`] from `read` or `exited` during a stream
///   means the program ended: the client is sent `exit`, not an error.
/// - **Push, optionally.** [`changes`](Self::changes) lets the route wait instead of polling.
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

    /// Whether the program has exited. `true` only once all its output is readable.
    ///
    /// # Errors
    /// The terminal cannot be queried.
    fn exited(&self) -> Result<bool, TerminalError>;

    /// A push hint, if the runtime has one: a receiver whose value changes whenever output is
    /// appended or the program exits (the value itself is not used; the output's end offset is
    /// a natural choice). With it the route waits for a change instead of polling. If the sender
    /// is dropped, the route falls back to polling. `None`, the default, means poll.
    fn changes(&self) -> Option<watch::Receiver<u64>> {
        None
    }
}

/// [`Terminals`] over a `Runtime`, with an explicit map from sessions to terminals.
///
/// It relies on the runtime reporting `alive = false` only after the program's output is in its
/// buffer (the [`Attachment`] contract), and polls: `Runtime` has no notifications.
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
    /// The first wait for output when there is none. Each idle round doubles it, up to
    /// `poll_max`; any input or output starts over. Unused while an attachment pushes changes.
    pub poll_min: Duration,
    /// The longest wait between polls while idle.
    pub poll_max: Duration,
    /// The largest message (and frame) a client may send: keystrokes, pastes, control.
    pub max_inbound: usize,
    /// How long one call into the seam may take before the route gives up on it.
    pub call_timeout: Duration,
    /// Calls into the seam in flight at once, across all clients of the route.
    pub max_calls: usize,
    /// How often to send a WebSocket Ping.
    pub ping_every: Duration,
    /// How long to wait for the Pong before closing the socket.
    pub pong_timeout: Duration,
}

impl Default for TerminalConfig {
    fn default() -> Self {
        Self {
            queue_frames: 64,
            send_timeout: Duration::from_secs(10),
            max_frame: 64 * 1024,
            poll_min: Duration::from_millis(20),
            poll_max: Duration::from_millis(250),
            max_inbound: 1024 * 1024,
            call_timeout: Duration::from_secs(5),
            max_calls: 64,
            ping_every: Duration::from_secs(20),
            pong_timeout: Duration::from_secs(20),
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
    /// The client did not answer a Ping in time.
    Unresponsive,
    /// The program exited (or its terminal disappeared); everything was sent.
    Exited,
    /// The client sent malformed control JSON, or a size outside [`SIZES`].
    Malformed,
    /// The client sent a message over [`TerminalConfig::max_inbound`].
    TooBig,
    /// The hub is shutting down.
    ShuttingDown,
    /// The runtime failed, or did not answer in time.
    Failed(TerminalError),
}

/// The terminal route. Mount it as a **device** route (`RouterParts::device`).
pub fn routes(terminals: Arc<dyn Terminals>, config: TerminalConfig) -> Router {
    Router::new()
        .route("/v1/sessions/{id}/terminal", get(terminal))
        .with_state(TerminalState {
            terminals,
            calls: Calls::new(&config),
            config,
        })
}

#[derive(Clone, Debug)]
struct TerminalState {
    terminals: Arc<dyn Terminals>,
    calls: Calls,
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
    shutdown: Option<Extension<HubShutdown>>,
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
        (Some(cols), Some(rows)) if SIZES.contains(&cols) && SIZES.contains(&rows) => {
            Some((cols, rows))
        }
        _ => return invalid("Give both `cols` and `rows`, each from 1 to 1000."),
    };
    let TerminalState {
        terminals,
        calls,
        config,
    } = state;
    let attachment = match calls.run(move || terminals.attach(session)).await {
        Ok(attachment) => attachment,
        Err(e) => return e.response().into_response(),
    };
    // Only now, with 404 answered first, is the request known to be an upgrade; nothing has
    // touched the terminal yet.
    let Ok(upgrade) = upgrade else {
        return invalid("GET /v1/sessions/{id}/terminal needs a WebSocket upgrade.");
    };
    if let Some((cols, rows)) = size {
        let attached = Arc::clone(&attachment);
        if let Err(e) = calls.run(move || attached.resize(cols, rows)).await {
            return e.response().into_response();
        }
    }
    let shutdown = shutdown.map(|Extension(shutdown)| shutdown);
    let from = query.from.unwrap_or(0);
    upgrade
        .protocols([WS_PROTOCOL])
        .max_message_size(config.max_inbound)
        .max_frame_size(config.max_inbound)
        .on_upgrade(move |socket| session_loop(socket, attachment, from, config, calls, shutdown))
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
                    .filter(|n| SIZES.contains(n))
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

/// Input from the client, waiting to be written to the terminal in arrival order.
#[derive(Debug)]
enum Input {
    Keys(axum::body::Bytes),
    Resize(u16, u16),
}

/// Input messages that may wait for a slow terminal before the socket stops being read. With
/// `max_inbound`, this bounds a client's queued input (16 MiB at the defaults).
const INPUT_QUEUE: usize = 16;

/// Runs one client: the pump's frames out, pings, and input in, until either side ends.
///
/// Nothing here waits on the terminal: input goes through a bounded queue to [`write_input`], so
/// a stalled write or resize never holds up output, pings or the shutdown signal. While the queue
/// is full the socket is not read, which pushes back on the client.
///
/// `shutdown` lives until this returns, after the closing handshake: `Bound::serve` counts open
/// sockets by it.
async fn session_loop(
    mut socket: WebSocket,
    attachment: Arc<dyn Attachment>,
    from: u64,
    config: TerminalConfig,
    calls: Calls,
    mut shutdown: Option<HubShutdown>,
) {
    let (frames, mut queue) = mpsc::channel(config.queue_frames.max(1));
    let nudge = Arc::new(Notify::new());
    let pump = pump(
        Arc::clone(&attachment),
        from,
        config,
        calls.clone(),
        Arc::clone(&nudge),
        frames,
    );
    tokio::pin!(pump);
    let (input, inputs) = mpsc::channel(INPUT_QUEUE);
    let writer = write_input(attachment, calls, inputs, nudge);
    tokio::pin!(writer);
    // Input read from the socket while the queue was full. While it waits, the socket is not
    // read, and the Pong deadline is paused.
    let mut waiting: Option<Input> = None;
    let hub_down = HubShutdown::wait(&mut shutdown);
    tokio::pin!(hub_down);
    // `interval` panics on a zero period.
    let ping_every = config.ping_every.max(Duration::from_millis(1));
    let mut ping = tokio::time::interval_at(Instant::now() + ping_every, ping_every);
    ping.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut pong = PongDeadline::default();
    let end = loop {
        tokio::select! {
            end = &mut pump => break end,
            end = &mut writer => break end,
            () = &mut hub_down => break TerminalEnd::ShuttingDown,
            Some(frame) = queue.recv() => {
                if let Err(end) = send(&mut socket, frame, config.send_timeout).await {
                    break end;
                }
            }
            room = input.reserve(), if waiting.is_some() => match (room, waiting.take()) {
                (Ok(room), Some(next)) => {
                    room.send(next);
                    pong.resume();
                }
                // The writer only stops by returning, which the branch above catches first.
                _ => break TerminalEnd::Failed(TerminalError::Failed("input stopped".to_owned())),
            },
            _ = ping.tick() => {
                // One Ping at a time: its Pong is what the deadline waits for.
                if !pong.awaited() {
                    pong.start(config.pong_timeout);
                    let ping = Message::Ping(axum::body::Bytes::new());
                    if let Err(end) = send(&mut socket, ping, config.send_timeout).await {
                        break end;
                    }
                }
            }
            () = sleep_until(pong.running()), if pong.running().is_some() => {
                break TerminalEnd::Unresponsive;
            }
            incoming = socket.recv(), if waiting.is_none() => {
                let next = match incoming {
                    None => break TerminalEnd::ClientGone,
                    Some(Err(e)) if too_big(&e) => break TerminalEnd::TooBig,
                    Some(Err(_)) => break TerminalEnd::ClientGone,
                    Some(Ok(Message::Close(_))) => {
                        finish(&mut socket).await;
                        break TerminalEnd::ClientGone;
                    }
                    Some(Ok(Message::Pong(_))) => {
                        pong.answered();
                        continue;
                    }
                    Some(Ok(Message::Binary(bytes))) => Input::Keys(bytes),
                    Some(Ok(Message::Text(text))) => match control(&text) {
                        Control::Resize(cols, rows) => Input::Resize(cols, rows),
                        Control::Ignore => continue,
                        Control::Malformed => break TerminalEnd::Malformed,
                    },
                    // Pings are answered by the socket itself.
                    Some(Ok(Message::Ping(_))) => continue,
                };
                if let Err(mpsc::error::TrySendError::Full(next)) = input.try_send(next) {
                    waiting = Some(next);
                    pong.pause();
                }
            }
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
        TerminalEnd::Unresponsive => (
            close_code::TRY_AGAIN_LATER,
            "no pong; resume from your offset",
        ),
        TerminalEnd::Malformed => (close_code::INVALID_DATA, "malformed control message"),
        TerminalEnd::TooBig => (close_code::TOO_BIG, "message too big"),
        TerminalEnd::ShuttingDown => (close_code::GOING_AWAY, "the hub is shutting down"),
        TerminalEnd::Failed(_) => (close_code::INTERNAL, "the terminal failed"),
    };
    close(&mut socket, code, reason).await;
}

/// Writes queued input to the terminal, one call at a time and in order, nudging the pump after
/// each so it looks for the echo. Returns only when a call fails (a stalled runtime times out
/// here, giving 1011); otherwise it runs until the session drops it.
async fn write_input(
    attachment: Arc<dyn Attachment>,
    calls: Calls,
    mut inputs: mpsc::Receiver<Input>,
    nudge: Arc<Notify>,
) -> TerminalEnd {
    while let Some(input) = inputs.recv().await {
        let attached = Arc::clone(&attachment);
        let written = match input {
            Input::Keys(bytes) => calls.run(move || attached.write(&bytes)).await,
            Input::Resize(cols, rows) => calls.run(move || attached.resize(cols, rows)).await,
        };
        if let Err(end) = settle(written) {
            return end;
        }
        nudge.notify_one();
    }
    // The session dropped its sender, so it is ending and will drop this too.
    std::future::pending().await
}

/// The outcome of writing input or resizing. A terminal that is gone is not an error here: the
/// pump finds it gone on its next read (the input nudges it) and ends the stream with `exit`.
fn settle(result: Result<(), TerminalError>) -> Result<(), TerminalEnd> {
    match result {
        Ok(()) | Err(TerminalError::NotFound(_)) => Ok(()),
        Err(e) => Err(TerminalEnd::Failed(e)),
    }
}

async fn send(socket: &mut WebSocket, frame: Message, wait: Duration) -> Result<(), TerminalEnd> {
    match tokio::time::timeout(wait, socket.send(frame)).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) => Err(TerminalEnd::ClientGone),
        Err(_) => Err(TerminalEnd::SlowClient),
    }
}

/// When the Pong to the last Ping is due. The deadline **runs only while the socket is read**.
///
/// While input backs up, the socket is not read (back-pressure on the client), and a Pong the
/// client did send sits unread behind its keystrokes: a WebSocket's frames arrive in order, so
/// there is no reading the Pong without reading them. That time does not count against the
/// client. A client that is really gone still runs out of time once reading resumes, and a write
/// that never returns ends the stream after `call_timeout` (1011) anyway.
#[derive(Debug, Default)]
struct PongDeadline {
    /// When the Pong is due, `None` when no Ping awaits one. Moved later on resuming, by the
    /// time the deadline was paused.
    due: Option<Instant>,
    /// Since when the socket has not been read.
    paused: Option<Instant>,
}

impl PongDeadline {
    /// A Ping was sent: its Pong is due within `timeout` of reading time.
    fn start(&mut self, timeout: Duration) {
        let now = Instant::now();
        self.due = Some(now + timeout);
        if self.paused.is_some() {
            // Only the pause from now on delays this Ping's deadline.
            self.paused = Some(now);
        }
    }

    /// Whether a Ping still awaits its Pong.
    fn awaited(&self) -> bool {
        self.due.is_some()
    }

    /// The Pong arrived.
    fn answered(&mut self) {
        self.due = None;
    }

    /// The socket stops being read.
    fn pause(&mut self) {
        if self.paused.is_none() {
            self.paused = Some(Instant::now());
        }
    }

    /// The socket is read again: the deadline moves on by the time it was paused.
    fn resume(&mut self) {
        if let (Some(since), Some(due)) = (self.paused.take(), self.due.as_mut()) {
            *due += since.elapsed();
        }
    }

    /// The deadline to wait for: `None` while paused or when no Ping awaits a Pong.
    fn running(&self) -> Option<Instant> {
        if self.paused.is_some() {
            None
        } else {
            self.due
        }
    }
}

async fn sleep_until(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

/// What woke an idle pump.
enum Woke {
    Input,
    Change,
    ChangesGone,
    Timer,
}

/// Reads output from `from` and queues it as frames until the program exits, the client goes
/// away or falls behind, or the runtime fails. `input` is notified on every keystroke or resize,
/// so the pump looks for their echo at once. Public for tests and benchmarks.
#[doc(hidden)]
pub async fn pump(
    attachment: Arc<dyn Attachment>,
    from: u64,
    config: TerminalConfig,
    calls: Calls,
    input: Arc<Notify>,
    out: mpsc::Sender<Message>,
) -> TerminalEnd {
    let max_frame = config.max_frame.clamp(1, 1024 * 1024);
    let poll_min = config.poll_min.max(Duration::from_millis(1));
    let poll_max = config.poll_max.max(poll_min);
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
    let exit = || async {
        match queue(Message::Text(r#"{"type":"exit"}"#.into())).await {
            Ok(()) => TerminalEnd::Exited,
            Err(end) => end,
        }
    };
    let mut changes = attachment.changes();
    let mut offset = from;
    let mut idle = poll_min;
    let mut exited = false;
    loop {
        // Mark the current value seen *before* reading, so a change during the read wakes us.
        if let Some(changes) = changes.as_mut() {
            changes.borrow_and_update();
        }
        let attached = Arc::clone(&attachment);
        let chunk = match calls.run(move || attached.read(offset, max_frame)).await {
            Ok(chunk) => chunk,
            // The terminal is gone: the program ended.
            Err(TerminalError::NotFound(_)) => return exit().await,
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
        offset = chunk.offset.saturating_add(chunk.data.len() as u64);
        if !chunk.data.is_empty() {
            if let Err(end) = queue(Message::Binary(chunk.data.into())).await {
                return end;
            }
            idle = poll_min;
            // Read again at once. Only a read that finds nothing leads to `exited()` and to
            // waiting, so an empty chunk never loops, whatever its `end` says.
            continue;
        }
        if exited {
            // Everything the program wrote has been sent (see the `Attachment` contract).
            return exit().await;
        }
        let attached = Arc::clone(&attachment);
        match calls.run(move || attached.exited()).await {
            // One more read collects what it wrote last; a terminal that is gone has ended too.
            Ok(true) | Err(TerminalError::NotFound(_)) => {
                exited = true;
                continue;
            }
            Ok(false) => {}
            Err(e) => return TerminalEnd::Failed(e),
        }
        let woke = tokio::select! {
            () = out.closed() => return TerminalEnd::ClientGone,
            () = input.notified() => Woke::Input,
            woke = idle_wait(changes.as_mut(), idle) => woke,
        };
        match woke {
            Woke::Input => idle = poll_min,
            Woke::Change => {}
            Woke::ChangesGone => changes = None,
            Woke::Timer => idle = idle.saturating_mul(2).min(poll_max),
        }
    }
}

/// Waits for a pushed change, or for `idle` without one.
async fn idle_wait(changes: Option<&mut watch::Receiver<u64>>, idle: Duration) -> Woke {
    match changes {
        Some(changes) => match changes.changed().await {
            Ok(()) => Woke::Change,
            Err(_) => Woke::ChangesGone,
        },
        None => {
            tokio::time::sleep(idle).await;
            Woke::Timer
        }
    }
}

/// Runs blocking calls into the seam: on the blocking pool, at most `max_calls` at once, each
/// given up after `call_timeout`. A call that never returns keeps its permit, so a stalled
/// runtime ties up at most `max_calls` threads. Public for tests and benchmarks.
#[doc(hidden)]
#[derive(Clone, Debug)]
pub struct Calls {
    permits: Arc<Semaphore>,
    timeout: Duration,
}

impl Calls {
    /// Calls limited as `config` says.
    #[must_use]
    pub fn new(config: &TerminalConfig) -> Self {
        Self {
            permits: Arc::new(Semaphore::new(config.max_calls.max(1))),
            timeout: config.call_timeout.max(Duration::from_millis(1)),
        }
    }

    async fn run<T, F>(&self, f: F) -> Result<T, TerminalError>
    where
        T: Send + 'static,
        F: FnOnce() -> Result<T, TerminalError> + Send + 'static,
    {
        let deadline = Instant::now() + self.timeout;
        let late = || {
            TerminalError::Unavailable(format!(
                "The terminal did not answer within {} ms.",
                self.timeout.as_millis()
            ))
        };
        let permit = match tokio::time::timeout_at(
            deadline,
            Arc::clone(&self.permits).acquire_owned(),
        )
        .await
        {
            Ok(Ok(permit)) => permit,
            Ok(Err(_)) => return Err(TerminalError::Failed("no permits".to_owned())),
            Err(_) => return Err(late()),
        };
        let call = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            f()
        });
        match tokio::time::timeout_at(deadline, call).await {
            Ok(Ok(result)) => result,
            Ok(Err(e)) => Err(TerminalError::Failed(format!(
                "the runtime call panicked: {e}"
            ))),
            Err(_) => Err(late()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pitcrew_interfaces::fake::FakeRuntime;
    use pitcrew_interfaces::runtime::StartSpec;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

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

    /// Starts a pump on `attachment`; returns its queue, its input nudge, and its task.
    fn spawn(
        attachment: Arc<dyn Attachment>,
        config: TerminalConfig,
    ) -> (
        mpsc::Receiver<Message>,
        Arc<Notify>,
        tokio::task::JoinHandle<TerminalEnd>,
    ) {
        let (frames, queue) = mpsc::channel(config.queue_frames);
        let input = Arc::new(Notify::new());
        let task = tokio::spawn(pump(
            attachment,
            0,
            config,
            Calls::new(&config),
            Arc::clone(&input),
            frames,
        ));
        (queue, input, task)
    }

    fn exit_frame() -> Message {
        Message::Text(r#"{"type":"exit"}"#.into())
    }

    /// An attachment with scripted output that counts calls, can push changes, and can make
    /// `read` or `exited` answer `NotFound` or hang.
    #[derive(Debug, Default)]
    struct Scripted {
        output: Mutex<Vec<u8>>,
        exited: AtomicBool,
        /// The value `end` reports beyond the real output (a lying runtime).
        extra_end: AtomicUsize,
        reads: AtomicUsize,
        exits: AtomicUsize,
        read_gone: AtomicBool,
        exited_gone: AtomicBool,
        hang: AtomicBool,
        changes: Mutex<Option<watch::Sender<u64>>>,
    }

    impl Scripted {
        fn pushing() -> Arc<Self> {
            let scripted = Self::default();
            *scripted.changes.lock().unwrap() = Some(watch::channel(0).0);
            Arc::new(scripted)
        }

        fn append(&self, bytes: &[u8]) {
            let mut output = self.output.lock().unwrap();
            output.extend_from_slice(bytes);
            self.push(output.len());
        }

        fn exit(&self) {
            self.exited.store(true, Ordering::SeqCst);
            self.push(usize::MAX);
        }

        fn push(&self, value: usize) {
            if let Some(changes) = self.changes.lock().unwrap().as_ref() {
                changes.send_replace(value as u64);
            }
        }

        fn calls(&self) -> (usize, usize) {
            (
                self.reads.load(Ordering::SeqCst),
                self.exits.load(Ordering::SeqCst),
            )
        }

        fn hang_if_asked(&self) {
            while self.hang.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }

    impl Attachment for Scripted {
        fn read(&self, from: u64, max: usize) -> Result<OutputChunk, TerminalError> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            self.hang_if_asked();
            if self.read_gone.load(Ordering::SeqCst) {
                return Err(TerminalError::NotFound("gone".into()));
            }
            let output = self.output.lock().unwrap();
            let start = usize::try_from(from).unwrap().min(output.len());
            let stop = (start + max).min(output.len());
            Ok(OutputChunk {
                offset: start as u64,
                data: output[start..stop].to_vec(),
                end: (output.len() + self.extra_end.load(Ordering::SeqCst)) as u64,
                truncated: false,
            })
        }

        fn write(&self, bytes: &[u8]) -> Result<(), TerminalError> {
            self.append(bytes);
            Ok(())
        }

        fn resize(&self, _cols: u16, _rows: u16) -> Result<(), TerminalError> {
            Ok(())
        }

        fn exited(&self) -> Result<bool, TerminalError> {
            self.exits.fetch_add(1, Ordering::SeqCst);
            self.hang_if_asked();
            if self.exited_gone.load(Ordering::SeqCst) {
                return Err(TerminalError::NotFound("gone".into()));
            }
            Ok(self.exited.load(Ordering::SeqCst))
        }

        fn changes(&self) -> Option<watch::Receiver<u64>> {
            self.changes
                .lock()
                .unwrap()
                .as_ref()
                .map(watch::Sender::subscribe)
        }
    }

    /// Reads binary frames until `want` bytes arrived.
    async fn bytes(queue: &mut mpsc::Receiver<Message>, want: usize) -> Vec<u8> {
        let mut got = Vec::new();
        while got.len() < want {
            match tokio::time::timeout(Duration::from_secs(5), queue.recv()).await {
                Ok(Some(Message::Binary(data))) => got.extend_from_slice(&data),
                other => panic!("expected output, got {other:?}"),
            }
        }
        got
    }

    #[test]
    fn control_messages() {
        assert_eq!(
            control(r#"{"type":"resize","cols":120,"rows":40}"#),
            Control::Resize(120, 40)
        );
        assert_eq!(
            control(r#"{"type":"resize","cols":1000,"rows":1}"#),
            Control::Resize(1000, 1)
        );
        assert_eq!(control(r#"{"type":"focus"}"#), Control::Ignore);
        for bad in [
            "{not json",
            "[]",
            r#"{"cols":1}"#,
            r#"{"type":"resize","cols":0,"rows":1}"#,
            r#"{"type":"resize","cols":1001,"rows":1}"#,
            r#"{"type":"resize","cols":1,"rows":1001}"#,
            r#"{"type":"resize","cols":70000,"rows":1}"#,
            r#"{"type":"resize"}"#,
        ] {
            assert_eq!(control(bad), Control::Malformed, "{bad}");
        }
    }

    #[test]
    fn the_pong_deadline_waits_while_the_socket_is_not_read() {
        const TIMEOUT: Duration = Duration::from_millis(50);
        const PAUSE: Duration = Duration::from_millis(250);
        let mut pong = PongDeadline::default();
        assert_eq!(pong.running(), None);
        assert!(!pong.awaited());

        pong.start(TIMEOUT);
        let due = pong.running().unwrap();
        assert!(pong.awaited());
        pong.pause();
        assert_eq!(pong.running(), None, "paused");
        std::thread::sleep(PAUSE);
        pong.resume();
        // Later by the pause, so the time left is what it was: still in the future.
        let moved = pong.running().unwrap();
        assert!(moved >= due + PAUSE, "{moved:?} vs {due:?}");
        assert!(moved > Instant::now());
        pong.answered();
        assert_eq!(pong.running(), None);
        assert!(!pong.awaited());

        // A Ping sent during a pause is delayed only by the rest of that pause.
        pong.pause();
        std::thread::sleep(PAUSE);
        pong.start(TIMEOUT);
        let started = Instant::now();
        assert_eq!(pong.running(), None);
        pong.resume();
        let due = pong.running().unwrap();
        assert!(due >= started + TIMEOUT);
        assert!(
            due < started + TIMEOUT + PAUSE,
            "the earlier pause counted too"
        );

        // Resuming without a pause, or without a Ping, changes nothing.
        pong.resume();
        assert_eq!(pong.running(), Some(due));
        pong.answered();
        pong.pause();
        pong.resume();
        assert_eq!(pong.running(), None);
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
            poll_min: Duration::from_millis(1),
            ..TerminalConfig::default()
        };
        let (mut queue, _input, task) = spawn(terminals.attach(session).unwrap(), config);
        let mut sizes = Vec::new();
        while sizes.iter().sum::<usize>() < 10 {
            match queue.recv().await.unwrap() {
                Message::Binary(bytes) => sizes.push(bytes.len()),
                other => panic!("unexpected {other:?}"),
            }
        }
        assert_eq!(sizes, vec![4, 4, 2]);
        runtime.kill(terminal).unwrap();
        assert_eq!(queue.recv().await.unwrap(), exit_frame());
        assert_eq!(task.await.unwrap(), TerminalEnd::Exited);
    }

    #[tokio::test]
    async fn output_written_just_before_the_exit_is_sent_before_exit() {
        let scripted = Arc::new(Scripted::default());
        let config = TerminalConfig {
            poll_min: Duration::from_millis(1),
            ..TerminalConfig::default()
        };
        let (mut queue, _input, task) = spawn(scripted.clone(), config);
        tokio::time::sleep(Duration::from_millis(20)).await;
        // Appended after the pump's last empty read, then exited at once.
        scripted.append(b"last words");
        scripted.exit();
        assert_eq!(bytes(&mut queue, 10).await, b"last words");
        assert_eq!(queue.recv().await.unwrap(), exit_frame());
        assert_eq!(task.await.unwrap(), TerminalEnd::Exited);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_idle_terminal_backs_off_and_input_wakes_it() {
        let scripted = Arc::new(Scripted::default());
        let (mut queue, input, _task) = spawn(scripted.clone(), TerminalConfig::default());
        // 20 + 40 + 80 + 160 ms reach the 250 ms ceiling.
        tokio::time::sleep(Duration::from_millis(600)).await;
        let before = scripted.calls();
        tokio::time::sleep(Duration::from_secs(1)).await;
        let after = scripted.calls();
        let (reads, exits) = (after.0 - before.0, after.1 - before.1);
        // At most 5 rounds a second, each one read and one `exited()` (the old loop made 50).
        assert!(reads <= 5 && exits <= 5, "{reads} reads, {exits} exits");
        assert!(reads >= 1, "still polling");

        // Input resets the back-off: its echo arrives well before the next 250 ms poll.
        let started = std::time::Instant::now();
        scripted.write(b"k").unwrap();
        input.notify_one();
        assert_eq!(bytes(&mut queue, 1).await, b"k");
        assert!(started.elapsed() < Duration::from_millis(200));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_empty_chunk_never_spins_whatever_its_end_says() {
        let scripted = Arc::new(Scripted::default());
        scripted.extra_end.store(100, Ordering::SeqCst);
        let config = TerminalConfig {
            poll_min: Duration::from_millis(20),
            poll_max: Duration::from_millis(20),
            ..TerminalConfig::default()
        };
        let (_queue, _input, _task) = spawn(scripted.clone(), config);
        tokio::time::sleep(Duration::from_millis(300)).await;
        let (reads, _) = scripted.calls();
        assert!(reads <= 20, "{reads} reads in 300 ms");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_pushing_attachment_is_not_polled() {
        let scripted = Scripted::pushing();
        let (mut queue, _input, task) = spawn(scripted.clone(), TerminalConfig::default());
        tokio::time::sleep(Duration::from_millis(500)).await;
        // One round to find nothing, then waiting on the hint.
        assert_eq!(scripted.calls(), (1, 1));

        scripted.append(b"pushed");
        assert_eq!(bytes(&mut queue, 6).await, b"pushed");
        scripted.exit();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), queue.recv())
                .await
                .unwrap()
                .unwrap(),
            exit_frame()
        );
        assert_eq!(task.await.unwrap(), TerminalEnd::Exited);
    }

    #[tokio::test]
    async fn a_program_that_exited_before_attaching_ends_with_a_pushing_attachment_too() {
        let scripted = Scripted::pushing();
        scripted.append(b"done");
        scripted.exit();
        let (mut queue, _input, task) = spawn(scripted, TerminalConfig::default());
        assert_eq!(bytes(&mut queue, 4).await, b"done");
        assert_eq!(queue.recv().await.unwrap(), exit_frame());
        assert_eq!(task.await.unwrap(), TerminalEnd::Exited);
    }

    #[tokio::test]
    async fn a_dropped_push_hint_falls_back_to_polling() {
        let scripted = Scripted::pushing();
        let config = TerminalConfig {
            poll_min: Duration::from_millis(1),
            ..TerminalConfig::default()
        };
        let (mut queue, _input, _task) = spawn(scripted.clone(), config);
        tokio::time::sleep(Duration::from_millis(20)).await;
        *scripted.changes.lock().unwrap() = None;
        scripted.output.lock().unwrap().extend_from_slice(b"polled");
        assert_eq!(bytes(&mut queue, 6).await, b"polled");
    }

    #[tokio::test]
    async fn a_terminal_that_disappears_is_an_exit() {
        for gone in ["read", "exited"] {
            let scripted = Arc::new(Scripted::default());
            scripted.append(b"out");
            let config = TerminalConfig {
                poll_min: Duration::from_millis(1),
                ..TerminalConfig::default()
            };
            let (mut queue, _input, task) = spawn(scripted.clone(), config);
            assert_eq!(bytes(&mut queue, 3).await, b"out");
            match gone {
                "read" => scripted.read_gone.store(true, Ordering::SeqCst),
                _ => scripted.exited_gone.store(true, Ordering::SeqCst),
            }
            assert_eq!(queue.recv().await.unwrap(), exit_frame(), "{gone}");
            assert_eq!(task.await.unwrap(), TerminalEnd::Exited, "{gone}");
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_call_that_hangs_is_given_up_and_holds_its_permit() {
        let scripted = Arc::new(Scripted::default());
        scripted.hang.store(true, Ordering::SeqCst);
        let config = TerminalConfig {
            call_timeout: Duration::from_millis(100),
            max_calls: 1,
            ..TerminalConfig::default()
        };
        let calls = Calls::new(&config);
        let (frames, _queue) = mpsc::channel(4);
        let started = std::time::Instant::now();
        let end = pump(
            scripted.clone(),
            0,
            config,
            calls.clone(),
            Arc::new(Notify::new()),
            frames,
        )
        .await;
        assert!(
            matches!(end, TerminalEnd::Failed(TerminalError::Unavailable(_))),
            "{end:?}"
        );
        assert!(started.elapsed() < Duration::from_secs(1));
        // The hung call still holds the only permit: the next call is refused without running.
        let attached = scripted.clone();
        let refused = calls.run(move || attached.exited()).await;
        assert!(matches!(refused, Err(TerminalError::Unavailable(_))));
        assert_eq!(scripted.calls(), (1, 0));
        scripted.hang.store(false, Ordering::SeqCst);
        let attached = scripted.clone();
        let mut answered = calls.run(move || attached.exited()).await;
        for _ in 0..50 {
            if answered.is_ok() {
                break;
            }
            let attached = scripted.clone();
            answered = calls.run(move || attached.exited()).await;
        }
        assert_eq!(answered, Ok(false));
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
            poll_min: Duration::from_millis(1),
            poll_max: Duration::from_millis(1),
            ..TerminalConfig::default()
        };
        let fast_config = TerminalConfig {
            send_timeout: Duration::from_secs(10),
            ..slow_config
        };
        let (slow_queue, _slow_input, slow) =
            spawn(terminals.attach(session).unwrap(), slow_config);
        let (mut fast_queue, _fast_input, fast) =
            spawn(terminals.attach(session).unwrap(), fast_config);
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
