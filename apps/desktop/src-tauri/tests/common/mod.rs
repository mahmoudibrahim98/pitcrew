//! A fake daemon on a unix socket, as `pitcrewd serve --listen private` lays it out: a state
//! directory with `run/pitcrewd.sock` (in a 0700 directory) and `device.token` (0600).
//!
//! It answers like API v1, and on purpose adds headers that hold the token to every response,
//! so a test can prove that no daemon header but `Content-Type` reaches the webview. Its sockets
//! follow a script named in the query (`?script=order`); see [`socket`]. Without a script,
//! `/v1/stream` is the live stream of its asks ([`FakeDaemon::raise`]), resumable with `since`.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use axum::Router;
use axum::body::Bytes;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post};
use pitcrew_desktop::daemon::LocalConnector;
use pitcrew_desktop::daemon::endpoint::Endpoint;
use pitcrew_desktop::gateway::{Connector, Delivery, Sink, SinkClosed};
use pitcrew_desktop::registry::{
    Connection, LauncherKind, Registry, RemoteConnection, WorkspaceKind, WorkspaceRecord,
    WorkspaceState,
};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path as FsPath, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The demo workspace's id and name, as `GET /v1/workspace` answers.
pub const WORKSPACE_ID: &str = "01JA0000000000000000000000";
pub const WORKSPACE_NAME: &str = "Demo Lab";

/// The members: the person the token acts for (`GET /v1/me`), an agent, another person.
pub const SAM: &str = "01JA0000000000000000000001";
pub const WRITER: &str = "01JA0000000000000000000002";
pub const OTHER: &str = "01JA0000000000000000000003";

/// Registers `id` as the local workspace, ready, reached through `connector`, as the app does
/// once the local daemon answers.
pub fn register_local(registry: &Registry, id: &str, name: &str, connector: Arc<dyn Connector>) {
    registry.attach_local(connector);
    registry.set_local(id, name).unwrap();
}

/// Registers a remote workspace `id` on `hpc-login`, in `state`, reached through `connector`
/// (the gateway does not care how a connector reaches its daemon).
pub fn register_remote(
    registry: &Registry,
    id: &str,
    name: &str,
    connector: Arc<dyn Connector>,
    state: WorkspaceState,
) {
    let record = WorkspaceRecord {
        id: id.into(),
        name: name.into(),
        kind: WorkspaceKind::Remote,
        connection: Connection::Remote(Box::new(RemoteConnection {
            target: None,
            host: "hpc-login".into(),
            launcher: LauncherKind::Direct,
            root: "/home/sam/.pitcrew".into(),
            platform: "x86_64-unknown-linux-musl".into(),
            site: None,
            job: None,
            last_hop: None,
            transport: None,
        })),
    };
    registry.claim_remote(record, connector, state).unwrap();
}

/// Ask `n`'s id.
pub fn ask_id(n: u8) -> String {
    format!("01JB00000000000000000000{n:02}")
}

/// An open question, ask `n`, from `from` to `to`.
pub fn ask(n: u8, from: &str, to: &str) -> Value {
    json!({
        "id": ask_id(n), "kind": "question", "from": from, "to": to,
        "title": format!("Question {n}"), "body": "", "options": [], "receipts": [],
        "state": "open", "created": 1,
    })
}

fn members() -> Value {
    json!([
        { "id": SAM, "kind": "human", "handle": "@sam", "name": "Sam" },
        { "id": WRITER, "kind": "agent", "handle": "@writer", "name": "Writer", "owner": SAM },
        { "id": OTHER, "kind": "human", "handle": "@kim", "name": "Kim" },
    ])
}

/// The fake daemon's event log and asks.
#[derive(Debug)]
struct Log {
    rev: u64,
    id: String,
    /// `events` frames by their revision.
    history: Vec<(u64, Value)>,
    asks: Vec<Value>,
    /// `GET /v1/asks` answers 500.
    asks_fail: bool,
}

/// What a live stream is sent.
#[derive(Clone, Debug)]
enum Live {
    /// An `events` frame at this revision.
    Frame(u64, String),
    /// Drop the connection without a close frame.
    Drop,
}

/// What the fake daemon saw.
#[derive(Debug, Default)]
pub struct Seen {
    /// `Authorization` headers.
    pub authorizations: Vec<String>,
    /// `Sec-WebSocket-Protocol` headers.
    pub subprotocols: Vec<String>,
    /// Close codes its sockets received from the gateway, by script.
    pub closes: Vec<(String, Option<u16>)>,
    /// Pong payloads its sockets received.
    pub pongs: Vec<Vec<u8>>,
    /// Bytes its `flood` sockets sent before they stopped.
    pub flooded: Vec<usize>,
    /// Each live stream's `since`, in the order they opened.
    pub streams: Vec<Option<String>>,
    /// When each live stream opened.
    pub stream_times: Vec<Instant>,
    /// `GET /v1/asks` calls.
    pub ask_reads: usize,
}

#[derive(Clone)]
struct Fake {
    token: String,
    seen: Arc<Mutex<Seen>>,
    /// Becomes true when the fake daemon stops; handlers that hang end then.
    stopping: tokio::sync::watch::Receiver<bool>,
    log: Arc<Mutex<Log>>,
    live: tokio::sync::broadcast::Sender<Live>,
}

/// A fake daemon serving on its own thread and runtime.
pub struct FakeDaemon {
    pub state_dir: PathBuf,
    pub token: String,
    pub seen: Arc<Mutex<Seen>>,
    log: Arc<Mutex<Log>>,
    live: tokio::sync::broadcast::Sender<Live>,
    stop: Option<tokio::sync::watch::Sender<bool>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl FakeDaemon {
    /// Serves in `state_dir` (created) with `token`.
    pub fn start(state_dir: &FsPath, token: &str) -> Self {
        let run = state_dir.join("run");
        std::fs::create_dir_all(&run).unwrap();
        std::fs::set_permissions(state_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::set_permissions(&run, std::fs::Permissions::from_mode(0o700)).unwrap();
        let token_file = state_dir.join("device.token");
        std::fs::write(&token_file, format!("{token}\n")).unwrap();
        std::fs::set_permissions(&token_file, std::fs::Permissions::from_mode(0o600)).unwrap();

        let socket = run.join("pitcrewd.sock");
        let _ = std::fs::remove_file(&socket);
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        listener.set_nonblocking(true).unwrap();

        let seen = Arc::new(Mutex::new(Seen::default()));
        let (stop, stopping) = tokio::sync::watch::channel(false);
        let log = Arc::new(Mutex::new(Log {
            rev: 7,
            id: "log-1".into(),
            history: Vec::new(),
            asks: Vec::new(),
            asks_fail: false,
        }));
        let (live, _) = tokio::sync::broadcast::channel(256);
        let fake = Fake {
            token: token.to_owned(),
            seen: Arc::clone(&seen),
            stopping: stopping.clone(),
            log: Arc::clone(&log),
            live: live.clone(),
        };
        let thread = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async move {
                let listener = tokio::net::UnixListener::from_std(listener).unwrap();
                let mut stopping = stopping;
                axum::serve(listener, router(fake))
                    .with_graceful_shutdown(async move {
                        let _ = stopping.wait_for(|s| *s).await;
                    })
                    .await
                    .unwrap();
            });
            runtime.shutdown_timeout(Duration::from_secs(1));
        });
        Self {
            state_dir: state_dir.to_owned(),
            token: token.to_owned(),
            seen,
            log,
            live,
            stop: Some(stop),
            thread: Some(thread),
        }
    }

    /// Appends an event to the log and sends it to the live streams, as an `events` frame.
    fn append(&self, log: &mut Log, kind: &str, data: Value) {
        log.rev += 1;
        let rev = log.rev;
        let frame = json!({
            "type": "events", "from_rev": rev, "to_rev": rev,
            "events": [{
                "id": format!("01JE{rev:022}"), "at": 1, "workspace": WORKSPACE_ID,
                "author": WRITER, "body": { "type": kind, "data": data },
            }],
        });
        log.history.push((rev, frame.clone()));
        let _ = self.live.send(Live::Frame(rev, frame.to_string()));
    }

    /// Raises `ask` (from [`ask`]): `ask_raised`.
    pub fn raise(&self, ask: Value) {
        let mut log = self.log.lock().unwrap();
        log.asks.push(ask.clone());
        self.append(&mut log, "ask_raised", json!({ "ask": ask }));
    }

    /// Answers ask `n`: `ask_answered`.
    pub fn answer(&self, n: u8) {
        let mut log = self.log.lock().unwrap();
        let id = ask_id(n);
        for ask in &mut log.asks {
            if ask["id"] == id {
                ask["state"] = json!("answered");
            }
        }
        self.append(
            &mut log,
            "ask_answered",
            json!({ "ask": id, "answer": { "by": SAM, "text": "yes", "at": 2 } }),
        );
    }

    /// Another kind of event, which "needs you" ignores.
    pub fn other_event(&self) {
        let mut log = self.log.lock().unwrap();
        self.append(
            &mut log,
            "task_moved",
            json!({ "task": "01JC0000000000000000000000" }),
        );
    }

    /// The current revision.
    pub fn rev(&self) -> u64 {
        self.log.lock().unwrap().rev
    }

    /// Starts a new event log (the daemon's store was replaced): revisions start again.
    pub fn new_log(&self, id: &str) {
        let mut log = self.log.lock().unwrap();
        log.id = id.to_owned();
        log.rev = 1;
        log.history.clear();
    }

    /// Makes `GET /v1/asks` answer 500 (or not).
    pub fn fail_asks(&self, fail: bool) {
        self.log.lock().unwrap().asks_fail = fail;
    }

    /// When each live stream opened.
    pub fn stream_times(&self) -> Vec<Instant> {
        self.seen.lock().unwrap().stream_times.clone()
    }

    /// Drops every live stream's connection, without a close frame.
    pub fn drop_streams(&self) {
        let _ = self.live.send(Live::Drop);
    }

    /// Each live stream's `since`, in order.
    pub fn streams(&self) -> Vec<Option<String>> {
        self.seen.lock().unwrap().streams.clone()
    }

    /// Where it listens.
    pub fn endpoint(&self) -> Endpoint {
        Endpoint::Unix {
            dir: self.state_dir.join("run"),
        }
    }

    /// The token file.
    pub fn token_file(&self) -> PathBuf {
        self.state_dir.join("device.token")
    }

    /// A connector to it, as the app builds one.
    pub fn connector(&self) -> LocalConnector {
        LocalConnector::with_token_path(self.endpoint(), self.token_file())
    }

    /// Waits until `check` holds for what it saw.
    pub fn wait_for(&self, what: &str, check: impl Fn(&Seen) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !check(&self.seen.lock().unwrap()) {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}: {:?}",
                self.seen.lock().unwrap()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// The close codes its `script` sockets received.
    pub fn closes(&self, script: &str) -> Vec<Option<u16>> {
        self.seen
            .lock()
            .unwrap()
            .closes
            .iter()
            .filter(|(s, _)| s == script)
            .map(|(_, c)| *c)
            .collect()
    }
}

impl Drop for FakeDaemon {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(true);
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn router(fake: Fake) -> Router {
    Router::new()
        .route("/v1/workspace", get(workspace))
        .route("/v1/me", get(me))
        .route("/v1/members", get(list_members))
        .route("/v1/asks", get(list_asks))
        .route("/v1/tasks", get(tasks))
        .route("/v1/echo", post(echo))
        .route("/v1/query", get(query))
        .route("/v1/big", get(big))
        .route("/v1/plain", get(plain))
        .route("/v1/stream", any(stream))
        .route("/v1/sessions/{id}/terminal", any(terminal))
        .fallback(not_found)
        .layer(middleware::from_fn_with_state(fake.clone(), auth))
        .route("/v1/host/info", get(host_info))
        .layer(middleware::from_fn_with_state(fake.clone(), leaky_headers))
        .with_state(fake)
}

fn api_error(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "application/json")],
        serde_json::json!({ "code": code, "message": message }).to_string(),
    )
        .into_response()
}

/// Bearer token, or for WebSockets the `pitcrew.bearer.<token>` subprotocol.
async fn auth(State(fake): State<Fake>, request: Request, next: Next) -> Response {
    let headers = request.headers();
    let bearer = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let protocols = headers
        .get(header::SEC_WEBSOCKET_PROTOCOL)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    {
        let mut seen = fake.seen.lock().unwrap();
        if let Some(b) = &bearer {
            seen.authorizations.push(b.clone());
        }
        if let Some(p) = &protocols {
            seen.subprotocols.push(p.clone());
        }
    }
    let by_header = bearer.as_deref() == Some(&format!("Bearer {}", fake.token));
    let by_protocol = protocols.as_deref().is_some_and(|p| {
        p.split(',')
            .map(str::trim)
            .any(|p| p == format!("pitcrew.bearer.{}", fake.token))
    });
    if by_header || by_protocol {
        next.run(request).await
    } else {
        api_error(StatusCode::UNAUTHORIZED, "unauthorized", "no valid token")
    }
}

/// Headers no daemon should send, holding the token: the gateway must drop them.
async fn leaky_headers(State(fake): State<Fake>, request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let h = response.headers_mut();
    h.insert(
        "x-echo-authorization",
        HeaderValue::from_str(&format!("Bearer {}", fake.token)).unwrap(),
    );
    h.insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&format!("token={}", fake.token)).unwrap(),
    );
    response
}

async fn host_info() -> Response {
    (
        [(header::CONTENT_TYPE, "application/json")],
        serde_json::json!({ "name": "pitcrewd", "version": "0.0.0", "protocol": 1, "protocol_min": 1, "roles": ["hub"] }).to_string(),
    )
        .into_response()
}

async fn workspace() -> Response {
    (
        [(header::CONTENT_TYPE, "application/json")],
        serde_json::json!({ "workspace": { "id": WORKSPACE_ID, "name": WORKSPACE_NAME }, "rev": 7 }).to_string(),
    )
        .into_response()
}

async fn tasks() -> Response {
    (
        [(header::CONTENT_TYPE, "application/json")],
        r#"[{"id":"01JB","title":"Write the gateway"}]"#,
    )
        .into_response()
}

fn json_response(value: &Value) -> Response {
    (
        [(header::CONTENT_TYPE, "application/json")],
        value.to_string(),
    )
        .into_response()
}

async fn me() -> Response {
    json_response(&members()[0])
}

async fn list_members() -> Response {
    json_response(&members())
}

/// `GET /v1/asks?to=&state=`.
async fn list_asks(State(fake): State<Fake>, Query(q): Query<HashMap<String, String>>) -> Response {
    fake.seen.lock().unwrap().ask_reads += 1;
    if fake.log.lock().unwrap().asks_fail {
        return api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            "failing on purpose",
        );
    }
    let asks: Vec<Value> = fake
        .log
        .lock()
        .unwrap()
        .asks
        .iter()
        .filter(|a| q.get("to").is_none_or(|to| a["to"] == *to))
        .filter(|a| q.get("state").is_none_or(|state| a["state"] == *state))
        .cloned()
        .collect();
    json_response(&Value::Array(asks))
}

/// The live stream: `hello`, then what `since` missed, then live frames, until the client closes
/// or [`FakeDaemon::drop_streams`].
async fn live(mut socket: WebSocket, since: Option<String>, fake: Fake) {
    {
        let mut seen = fake.seen.lock().unwrap();
        seen.streams.push(since.clone());
        seen.stream_times.push(Instant::now());
    }
    let since: u64 = since.and_then(|s| s.parse().ok()).unwrap_or(u64::MAX);
    let (mut frames, hello, missed, mut sent) = {
        let log = fake.log.lock().unwrap();
        let frames = fake.live.subscribe();
        let hello = json!({ "type": "hello", "rev": log.rev, "log": log.id }).to_string();
        let missed: Vec<String> = log
            .history
            .iter()
            .filter(|(rev, _)| since < *rev)
            .map(|(_, frame)| frame.to_string())
            .collect();
        (frames, hello, missed, log.rev)
    };
    if socket.send(Message::Text(hello.into())).await.is_err() {
        return;
    }
    for frame in missed {
        if socket.send(Message::Text(frame.into())).await.is_err() {
            return;
        }
    }
    loop {
        tokio::select! {
            next = frames.recv() => match next {
                Ok(Live::Frame(rev, frame)) if rev > sent => {
                    sent = rev;
                    if socket.send(Message::Text(frame.into())).await.is_err() {
                        return;
                    }
                }
                Ok(Live::Frame(..)) => {}
                Ok(Live::Drop) | Err(_) => return,
            },
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => return,
                Some(Ok(_)) => {}
            },
        }
    }
}

async fn plain() -> Response {
    (StatusCode::NO_CONTENT, "").into_response()
}

/// Says what arrived: the content type, the host and the body.
async fn echo(headers: HeaderMap, body: Bytes) -> Response {
    let text = |name: header::HeaderName| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
    };
    (
        StatusCode::CREATED,
        [(header::CONTENT_TYPE, "application/json")],
        serde_json::json!({
            "contentType": text(header::CONTENT_TYPE),
            "host": text(header::HOST),
            "body": String::from_utf8_lossy(&body),
        })
        .to_string(),
    )
        .into_response()
}

async fn query(request: Request) -> Response {
    let q = request.uri().query().unwrap_or("").to_owned();
    ([(header::CONTENT_TYPE, "text/plain")], q).into_response()
}

async fn big(Query(q): Query<HashMap<String, usize>>) -> Response {
    let n = q.get("bytes").copied().unwrap_or(0);
    ([(header::CONTENT_TYPE, "text/plain")], "a".repeat(n)).into_response()
}

async fn not_found() -> Response {
    api_error(StatusCode::NOT_FOUND, "not_found", "no such route")
}

async fn stream(
    State(fake): State<Fake>,
    Query(q): Query<HashMap<String, String>>,
    ws: WebSocketUpgrade,
) -> Response {
    let ws = ws.protocols(["pitcrew.v1"]);
    match q.get("script").cloned() {
        Some(script) => ws.on_upgrade(move |socket| run(socket, script, fake)),
        None => {
            let since = q.get("since").cloned();
            ws.on_upgrade(move |socket| live(socket, since, fake))
        }
    }
}

async fn terminal(
    State(fake): State<Fake>,
    Path(id): Path<String>,
    Query(q): Query<HashMap<String, String>>,
    ws: WebSocketUpgrade,
) -> Response {
    match id.as_str() {
        "down" => api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "unavailable",
            "the session's machine is unreachable",
        ),
        "nope" => api_error(StatusCode::NOT_FOUND, "not_found", "no such session"),
        // Takes the upgrade request and does not answer it until the fake daemon stops.
        "hang" => {
            let mut stopping = fake.stopping.clone();
            let _ = stopping.wait_for(|s| *s).await;
            api_error(StatusCode::SERVICE_UNAVAILABLE, "unavailable", "stopping")
        }
        _ => {
            let script = q.get("script").cloned().unwrap_or_else(|| "echo".into());
            ws.protocols(["pitcrew.v1"])
                .on_upgrade(move |socket| run(socket, script, fake))
        }
    }
}

fn close(code: u16, reason: &'static str) -> Message {
    Message::Close(Some(CloseFrame {
        code,
        reason: reason.into(),
    }))
}

/// Reads until the client closes, and records its close code (`None` if the connection just
/// ended). With `echo`, sends back every text and binary message.
async fn until_closed(socket: &mut WebSocket, script: &str, fake: &Fake, echo: bool) {
    while let Some(Ok(message)) = socket.recv().await {
        match message {
            Message::Close(frame) => {
                fake.seen
                    .lock()
                    .unwrap()
                    .closes
                    .push((script.to_owned(), frame.map(|f| f.code)));
                // Sends the queued close reply.
                let _ = socket.recv().await;
                return;
            }
            Message::Pong(data) => fake.seen.lock().unwrap().pongs.push(data.to_vec()),
            Message::Text(t) if echo => {
                let _ = socket.send(Message::Text(t)).await;
            }
            Message::Binary(b) if echo => {
                let _ = socket.send(Message::Binary(b)).await;
            }
            _ => {}
        }
    }
    fake.seen
        .lock()
        .unwrap()
        .closes
        .push((script.to_owned(), None));
}

/// The socket scripts:
/// - `order`: text "a", binary [1, 2, 3], text "b", then close 1000 "bye";
/// - `ping`: a Ping, then once the Pong is back, text "pong:<payload>" and close 1000;
/// - `drop`: text "x", then the connection drops without a close frame;
/// - `flood`: 64 KiB binary frames, up to 16 MiB, then close 1000; it stops when the client
///   closes;
/// - `big`: one 9 MiB binary frame;
/// - `echo`: echoes text and binary;
/// - `hold`: stays open.
async fn run(mut socket: WebSocket, script: String, fake: Fake) {
    match script.as_str() {
        "order" => {
            let _ = socket.send(Message::Text("a".into())).await;
            let _ = socket.send(Message::Binary(vec![1, 2, 3].into())).await;
            let _ = socket.send(Message::Text("b".into())).await;
            let _ = socket.send(close(1000, "bye")).await;
            until_closed(&mut socket, &script, &fake, false).await;
        }
        "ping" => {
            let _ = socket
                .send(Message::Ping(b"are-you-there".to_vec().into()))
                .await;
            let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
            loop {
                match tokio::time::timeout_at(deadline, socket.recv()).await {
                    Ok(Some(Ok(Message::Pong(data)))) => {
                        fake.seen.lock().unwrap().pongs.push(data.to_vec());
                        let text = format!("pong:{}", String::from_utf8_lossy(&data));
                        let _ = socket.send(Message::Text(text.into())).await;
                        let _ = socket.send(close(1000, "")).await;
                        until_closed(&mut socket, &script, &fake, false).await;
                        return;
                    }
                    Ok(Some(Ok(_))) => {}
                    _ => return,
                }
            }
        }
        "drop" => {
            let _ = socket.send(Message::Text("x".into())).await;
            // Dropping the socket drops the connection: no close frame.
            drop(socket);
        }
        "flood" => {
            use futures_util::{SinkExt as _, StreamExt as _};
            let chunk = vec![7u8; 64 * 1024];
            let mut sent = 0usize;
            let mut stopped_by_client = false;
            let (mut tx, mut rx) = socket.split();
            while sent < 16 * 1024 * 1024 {
                tokio::select! {
                    biased;
                    incoming = rx.next() => {
                        if let Some(Ok(Message::Close(frame))) = incoming {
                            fake.seen.lock().unwrap().closes.push((script.clone(), frame.map(|f| f.code)));
                        } else {
                            fake.seen.lock().unwrap().closes.push((script.clone(), None));
                        }
                        stopped_by_client = true;
                        break;
                    }
                    result = tx.send(Message::Binary(chunk.clone().into())) => {
                        if result.is_err() {
                            break;
                        }
                        sent += chunk.len();
                    }
                }
            }
            fake.seen.lock().unwrap().flooded.push(sent);
            if let Ok(mut socket) = rx.reunite(tx) {
                if stopped_by_client {
                    let _ = socket.recv().await;
                } else {
                    let _ = socket.send(close(1000, "done")).await;
                    until_closed(&mut socket, &script, &fake, false).await;
                }
            }
        }
        "big" => {
            let _ = socket
                .send(Message::Binary(vec![1u8; 9 * 1024 * 1024].into()))
                .await;
            until_closed(&mut socket, &script, &fake, false).await;
        }
        "echo" => until_closed(&mut socket, &script, &fake, true).await,
        _ => until_closed(&mut socket, &script, &fake, false).await,
    }
}

/// A sink that records what the webview would receive. With `auto_take`, every probe completes
/// at once (the webview keeps up); without, probes wait for [`Recorder::take_all`].
#[derive(Default)]
pub struct Recorder {
    pub messages: Mutex<Vec<Delivery>>,
    pending: Mutex<Vec<Box<dyn FnOnce() + Send>>>,
    auto_take: bool,
}

impl Recorder {
    pub fn keeping_up() -> Arc<Self> {
        Arc::new(Self {
            auto_take: true,
            ..Self::default()
        })
    }

    pub fn stalled() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Completes the waiting probes.
    pub fn take_all(&self) {
        let pending: Vec<_> = self.pending.lock().unwrap().drain(..).collect();
        for done in pending {
            done();
        }
    }

    pub fn snapshot(&self) -> Vec<Delivery> {
        self.messages.lock().unwrap().clone()
    }

    /// Waits for the close message and returns everything, close last.
    pub fn wait_closed(&self) -> Vec<Delivery> {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let messages = self.snapshot();
            if matches!(messages.last(), Some(Delivery::Close { .. })) {
                return messages;
            }
            assert!(
                Instant::now() < deadline,
                "no close; got {} messages",
                messages.len()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Waits until at least `n` messages arrived.
    pub fn wait_for(&self, n: usize) -> Vec<Delivery> {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let messages = self.snapshot();
            if messages.len() >= n {
                return messages;
            }
            assert!(
                Instant::now() < deadline,
                "waited for {n} messages; got {messages:?}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Sink for Recorder {
    fn deliver(&self, message: Delivery) -> Result<(), SinkClosed> {
        self.messages.lock().unwrap().push(message);
        Ok(())
    }

    fn probe(&self, done: Box<dyn FnOnce() + Send>) -> Result<(), SinkClosed> {
        if self.auto_take {
            done();
        } else {
            self.pending.lock().unwrap().push(done);
        }
        Ok(())
    }
}

/// The close message's code and reason.
pub fn close_of(messages: &[Delivery]) -> (u16, String) {
    match messages.last() {
        Some(Delivery::Close { code, reason }) => (*code, reason.clone()),
        other => panic!("the last message is not a close: {other:?}"),
    }
}

/// How many `close` messages there are (always exactly one, last).
pub fn closes_in(messages: &[Delivery]) -> usize {
    messages
        .iter()
        .filter(|m| matches!(m, Delivery::Close { .. }))
        .count()
}
