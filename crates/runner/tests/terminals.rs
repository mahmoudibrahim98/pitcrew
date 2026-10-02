//! `GET /v1/sessions/{id}/terminal` through the API's real router, over development TCP, with a
//! WebSocket client, served by `RunnerTerminals` over `FakeRuntime`: pitcrew-api's terminal
//! tests, plus the runner's own contract (bounded calls, a terminal that disappears, links that
//! survive a restart).

#![allow(clippy::unwrap_used)]

mod common;

use axum::body::{Body, to_bytes};
use axum::http::Request;
use common::CollectSink;
use pitcrew_api::{Bound, Listen, RouterParts, TerminalConfig, local_host_info, terminal};
use pitcrew_auth::{FileTokenStore, TokenStore};
use pitcrew_interfaces::fake::FakeRuntime;
use pitcrew_interfaces::runtime::{
    OutputChunk, Runtime, RuntimeError, RuntimeKind, Screen, StartSpec, TerminalInfo,
};
use pitcrew_protocol::api::{Caller, TokenScope};
use pitcrew_protocol::ids::{MachineId, MemberId, SessionId, TerminalId, WorkspaceId};
use pitcrew_protocol::runner::Key;
use pitcrew_runner::{RunnerConfig, RunnerHandle, RunnerTerminals, TerminalOptions};
use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tower::ServiceExt as _;
use tungstenite::client::IntoClientRequest as _;
use tungstenite::http::HeaderValue;
use tungstenite::protocol::frame::coding::CloseCode;
use tungstenite::{Message, WebSocket};

/// `FakeRuntime`, plus a replay buffer that has lost its first `dropped` bytes, a record of
/// resizes, terminals that can vanish, calls that can hang, and tmux-like targets.
#[derive(Debug, Default)]
struct TestRuntime {
    inner: FakeRuntime,
    dropped: AtomicU64,
    resizes: Mutex<Vec<(u16, u16)>>,
    gone: AtomicBool,
    hang: AtomicBool,
}

impl TestRuntime {
    fn check(&self, id: TerminalId) -> Result<(), RuntimeError> {
        while self.hang.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(5));
        }
        if self.gone.load(Ordering::SeqCst) {
            return Err(RuntimeError::NotFound(id));
        }
        Ok(())
    }
}

impl Runtime for TestRuntime {
    fn kind(&self) -> RuntimeKind {
        RuntimeKind::Tmux
    }
    fn start(&self, spec: &StartSpec) -> Result<TerminalInfo, RuntimeError> {
        self.inner.start(spec)
    }
    fn write(&self, id: TerminalId, bytes: &[u8]) -> Result<(), RuntimeError> {
        self.check(id)?;
        self.inner.write(id, bytes)
    }
    fn send_keys(&self, id: TerminalId, keys: &[Key]) -> Result<(), RuntimeError> {
        self.check(id)?;
        self.inner.send_keys(id, keys)
    }
    fn resize(&self, id: TerminalId, cols: u16, rows: u16) -> Result<(), RuntimeError> {
        self.check(id)?;
        self.resizes.lock().unwrap().push((cols, rows));
        self.inner.resize(id, cols, rows)
    }
    fn screen(&self, id: TerminalId) -> Result<Screen, RuntimeError> {
        self.inner.screen(id)
    }
    fn read_output(
        &self,
        id: TerminalId,
        from: u64,
        max: usize,
    ) -> Result<OutputChunk, RuntimeError> {
        self.check(id)?;
        let dropped = self.dropped.load(Ordering::SeqCst);
        let mut chunk = self.inner.read_output(id, from.max(dropped), max)?;
        chunk.truncated = from < dropped;
        Ok(chunk)
    }
    fn info(&self, id: TerminalId) -> Result<TerminalInfo, RuntimeError> {
        self.check(id)?;
        self.inner.info(id)
    }
    fn list(&self) -> Result<Vec<TerminalInfo>, RuntimeError> {
        self.inner.list()
    }
    fn kill(&self, id: TerminalId) -> Result<(), RuntimeError> {
        self.inner.kill(id)
    }
}

struct Tokens {
    store: Arc<FileTokenStore>,
    device: String,
    agent: String,
}

impl Tokens {
    fn new() -> Self {
        let store = Arc::new(FileTokenStore::in_memory());
        let person = Caller {
            member: MemberId::new(),
            scope: TokenScope::Device,
            on_behalf_of: None,
        };
        let agent = Caller {
            member: MemberId::new(),
            scope: TokenScope::Agent,
            on_behalf_of: Some(person.member),
        };
        let (_, device) = store.mint(person).unwrap();
        let (_, agent) = store.mint(agent).unwrap();
        Self {
            store,
            device: device.into_string(),
            agent: agent.into_string(),
        }
    }
}

struct Setup {
    tokens: Tokens,
    runtime: Arc<TestRuntime>,
    terminal: TerminalId,
    session: SessionId,
    app: axum::Router,
    _runner: RunnerHandle,
    _state: tempfile::TempDir,
}

fn runner(state: &Path) -> RunnerHandle {
    let config = RunnerConfig::new(WorkspaceId::new(), MachineId::new(), MemberId::new(), state);
    pitcrew_runner::start(config, Vec::new(), Arc::new(CollectSink::default())).unwrap()
}

fn options() -> TerminalOptions {
    TerminalOptions {
        call_timeout: Duration::from_millis(300),
        exit_settle: Duration::from_millis(20),
        ..TerminalOptions::default()
    }
}

fn spec() -> StartSpec {
    StartSpec {
        program: "agent".into(),
        args: vec![],
        cwd: "/work".into(),
        env: vec![],
        name: "t".into(),
        cols: 80,
        rows: 24,
    }
}

fn setup() -> Setup {
    let tokens = Tokens::new();
    let state = tempfile::tempdir().unwrap();
    let runner = runner(state.path());
    let runtime = Arc::new(TestRuntime::default());
    let terminal = runtime.start(&spec()).unwrap().id;
    let session = SessionId::new();
    let terminals = runner.terminals_with(runtime.clone(), options()).unwrap();
    terminals.link(session, terminal).unwrap();
    let config = TerminalConfig {
        poll_min: Duration::from_millis(2),
        poll_max: Duration::from_millis(20),
        max_frame: 8,
        ..TerminalConfig::default()
    };
    let store: Arc<dyn TokenStore> = tokens.store.clone();
    let app = pitcrew_api::router(
        local_host_info("0.0.0-test", vec![], vec![]),
        store,
        RouterParts::new().device(terminal::routes(Arc::new(terminals), config)),
    );
    Setup {
        tokens,
        runtime,
        terminal,
        session,
        app,
        _runner: runner,
        _state: state,
    }
}

/// Serves `app` on loopback TCP for the rest of the test.
async fn serve(app: axum::Router) -> SocketAddr {
    let bound = Bound::bind(&Listen::DevTcp {
        addr: "127.0.0.1:0".parse().unwrap(),
    })
    .await
    .unwrap();
    let addr = bound.tcp_addr().unwrap();
    tokio::spawn(bound.serve(app, std::future::pending()));
    addr
}

fn connect(addr: SocketAddr, token: &str, path: &str) -> WebSocket<TcpStream> {
    let stream = TcpStream::connect(addr).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut request = format!("ws://{addr}{path}").into_client_request().unwrap();
    request.headers_mut().insert(
        "sec-websocket-protocol",
        HeaderValue::from_str(&format!("pitcrew.v1, pitcrew.bearer.{token}")).unwrap(),
    );
    let (socket, response) = tungstenite::client(request, stream).unwrap();
    assert_eq!(response.headers()["sec-websocket-protocol"], "pitcrew.v1");
    socket
}

/// Reads binary frames until `want` bytes arrived; returns them. Panics on text frames.
fn read_bytes(socket: &mut WebSocket<TcpStream>, want: usize) -> Vec<u8> {
    let mut bytes = Vec::new();
    while bytes.len() < want {
        match socket.read().unwrap() {
            Message::Binary(data) => bytes.extend_from_slice(&data),
            other => panic!("expected output, got {other:?}"),
        }
    }
    assert_eq!(bytes.len(), want, "more bytes than expected");
    bytes
}

fn read_text(socket: &mut WebSocket<TcpStream>) -> serde_json::Value {
    match socket.read().unwrap() {
        Message::Text(text) => serde_json::from_str(&text).unwrap(),
        other => panic!("expected a text frame, got {other:?}"),
    }
}

fn read_close(socket: &mut WebSocket<TcpStream>) -> CloseCode {
    loop {
        match socket.read() {
            Ok(Message::Close(Some(frame))) => return frame.code,
            Ok(Message::Close(None)) => panic!("close without a code"),
            Ok(_) => {}
            Err(e) => panic!("no close frame: {e}"),
        }
    }
}

fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn output(runtime: &TestRuntime, terminal: TerminalId) -> Vec<u8> {
    runtime.read_output(terminal, 0, usize::MAX).unwrap().data
}

async fn call(app: axum::Router, path: &str, bearer: Option<&str>) -> (u16, serde_json::Value) {
    let mut builder = Request::builder().uri(path);
    if let Some(token) = bearer {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    let response = app
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status().as_u16();
    let body = to_bytes(response.into_body(), 1 << 20).await.unwrap();
    (status, serde_json::from_slice(&body).unwrap_or_default())
}

#[tokio::test(flavor = "multi_thread")]
async fn replay_live_and_resume_by_offset_lose_nothing() {
    let s = setup();
    let addr = serve(s.app.clone()).await;
    let token = s.tokens.device.clone();
    let (runtime, terminal, session) = (s.runtime.clone(), s.terminal, s.session);
    tokio::task::spawn_blocking(move || {
        runtime.write(terminal, b"replayed output, ").unwrap();
        let path = format!("/v1/sessions/{session}/terminal");
        let mut socket = connect(addr, &token, &path);
        let mut seen = read_bytes(&mut socket, 17);
        runtime.write(terminal, b"live one").unwrap();
        seen.extend(read_bytes(&mut socket, 8));
        drop(socket);

        // Missed while disconnected; resume from the count of bytes seen.
        runtime.write(terminal, b", while away, ").unwrap();
        let mut socket = connect(addr, &token, &format!("{path}?from={}", seen.len()));
        seen.extend(read_bytes(&mut socket, 14));
        runtime.write(terminal, b"and back").unwrap();
        seen.extend(read_bytes(&mut socket, 8));
        assert_eq!(seen, output(&runtime, terminal));
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn truncated_comes_first_when_the_buffer_lost_the_start() {
    let s = setup();
    let addr = serve(s.app.clone()).await;
    let token = s.tokens.device.clone();
    let (runtime, terminal, session) = (s.runtime.clone(), s.terminal, s.session);
    tokio::task::spawn_blocking(move || {
        runtime.write(terminal, b"0123456789").unwrap();
        runtime.dropped.store(4, Ordering::SeqCst);
        let mut socket = connect(addr, &token, &format!("/v1/sessions/{session}/terminal"));
        assert_eq!(
            read_text(&mut socket),
            serde_json::json!({"type": "truncated", "from": 4})
        );
        assert_eq!(read_bytes(&mut socket, 6), b"456789");
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn input_resize_unknown_and_malformed_control() {
    let s = setup();
    let addr = serve(s.app.clone()).await;
    let token = s.tokens.device.clone();
    let (runtime, terminal, session) = (s.runtime.clone(), s.terminal, s.session);
    tokio::task::spawn_blocking(move || {
        let path = format!("/v1/sessions/{session}/terminal?cols=100&rows=30");
        let mut socket = connect(addr, &token, &path);
        wait_for("the initial size", || {
            runtime.resizes.lock().unwrap().contains(&(100, 30))
        });

        // Keystrokes arrive byte for byte (the fake echoes them into its output).
        let typed = b"ls -la\r\x1b[A\x03\xff".to_vec();
        socket.send(Message::Binary(typed.clone().into())).unwrap();
        assert_eq!(read_bytes(&mut socket, typed.len()), typed);
        assert_eq!(output(&runtime, terminal), typed);

        socket
            .send(Message::Text(
                r#"{"type":"resize","cols":120,"rows":40}"#.into(),
            ))
            .unwrap();
        wait_for("the resize", || {
            runtime.resizes.lock().unwrap().contains(&(120, 40))
        });

        socket
            .send(Message::Text(r#"{"type":"something_new"}"#.into()))
            .unwrap();
        socket
            .send(Message::Binary(b"still here".to_vec().into()))
            .unwrap();
        assert_eq!(read_bytes(&mut socket, 10), b"still here");

        socket.send(Message::Text("{not json".into())).unwrap();
        assert_eq!(read_close(&mut socket), CloseCode::Invalid);
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn exit_is_sent_after_the_last_output() {
    let s = setup();
    let addr = serve(s.app.clone()).await;
    let token = s.tokens.device.clone();
    let (runtime, terminal, session) = (s.runtime.clone(), s.terminal, s.session);
    tokio::task::spawn_blocking(move || {
        let mut socket = connect(addr, &token, &format!("/v1/sessions/{session}/terminal"));
        runtime.write(terminal, b"bye").unwrap();
        runtime.kill(terminal).unwrap();
        assert_eq!(read_bytes(&mut socket, 3), b"bye");
        assert_eq!(read_text(&mut socket), serde_json::json!({"type": "exit"}));
        assert_eq!(read_close(&mut socket), CloseCode::Normal);
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn two_clients_share_one_terminal_and_each_sees_the_exit() {
    let s = setup();
    let addr = serve(s.app.clone()).await;
    let token = s.tokens.device.clone();
    let (runtime, terminal, session) = (s.runtime.clone(), s.terminal, s.session);
    tokio::task::spawn_blocking(move || {
        let path = format!("/v1/sessions/{session}/terminal");
        let mut first = connect(addr, &token, &path);
        let mut second = connect(addr, &token, &path);
        runtime.write(terminal, b"shared ").unwrap();
        let mut seen = [read_bytes(&mut first, 7), read_bytes(&mut second, 7)];

        // Both type; their keystrokes are written in arrival order, and both see both.
        first
            .send(Message::Binary(b"one ".to_vec().into()))
            .unwrap();
        wait_for("the first input", || {
            output(&runtime, terminal).ends_with(b"one ")
        });
        second
            .send(Message::Binary(b"two".to_vec().into()))
            .unwrap();
        seen[0].extend(read_bytes(&mut first, 7));
        seen[1].extend(read_bytes(&mut second, 7));
        assert_eq!(output(&runtime, terminal), b"shared one two");
        assert_eq!(seen[0], b"shared one two");
        assert_eq!(seen[1], b"shared one two");

        // Each attachment settles the exit on its own, after the last output.
        runtime.write(terminal, b"!").unwrap();
        runtime.kill(terminal).unwrap();
        for socket in [&mut first, &mut second] {
            assert_eq!(read_bytes(socket, 1), b"!");
            assert_eq!(read_text(socket), serde_json::json!({"type": "exit"}));
            assert_eq!(read_close(socket), CloseCode::Normal);
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn the_terminal_needs_a_device_token_and_a_known_session() {
    let s = setup();
    let path = format!("/v1/sessions/{}/terminal", s.session);

    let (status, _) = call(s.app.clone(), &path, None).await;
    assert_eq!(status, 401);

    let (status, body) = call(s.app.clone(), &path, Some(&s.tokens.agent)).await;
    assert_eq!(status, 403);
    assert_eq!(body["code"], "forbidden");

    for unknown in [
        format!("/v1/sessions/{}/terminal", SessionId::new()),
        "/v1/sessions/not-an-id/terminal".to_owned(),
    ] {
        let (status, body) = call(s.app.clone(), &unknown, Some(&s.tokens.device)).await;
        assert_eq!(status, 404, "{unknown}");
        assert_eq!(body["code"], "not_found");
    }

    // A known session, but no upgrade, or a bad size.
    for (query, message) in [("", "upgrade"), ("?cols=80", "rows")] {
        let (status, body) = call(
            s.app.clone(),
            &format!("{path}{query}"),
            Some(&s.tokens.device),
        )
        .await;
        assert_eq!(status, 400, "{query}");
        assert!(
            body["message"].as_str().unwrap().contains(message),
            "{body}"
        );
    }
}

// The runner's side of the contract.

#[tokio::test(flavor = "multi_thread")]
async fn a_runtime_that_hangs_answers_unavailable_in_bounded_time() {
    let s = setup();
    s.runtime.hang.store(true, Ordering::SeqCst);
    let started = Instant::now();
    let path = format!("/v1/sessions/{}/terminal", s.session);
    let (status, body) = call(s.app.clone(), &path, Some(&s.tokens.device)).await;
    assert_eq!(status, 503, "{body}");
    assert_eq!(body["code"], "unavailable");
    assert!(started.elapsed() < Duration::from_secs(3));
    s.runtime.hang.store(false, Ordering::SeqCst);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_terminal_that_disappears_mid_stream_has_exited() {
    let s = setup();
    let addr = serve(s.app.clone()).await;
    let token = s.tokens.device.clone();
    let (runtime, terminal, session) = (s.runtime.clone(), s.terminal, s.session);
    tokio::task::spawn_blocking(move || {
        let mut socket = connect(addr, &token, &format!("/v1/sessions/{session}/terminal"));
        runtime.write(terminal, b"last").unwrap();
        assert_eq!(read_bytes(&mut socket, 4), b"last");
        runtime.gone.store(true, Ordering::SeqCst);
        assert_eq!(read_text(&mut socket), serde_json::json!({"type": "exit"}));
        assert_eq!(read_close(&mut socket), CloseCode::Normal);
    })
    .await
    .unwrap();
}

/// A tmux-like runtime whose terminals can be replaced by others with the same targets, as a
/// different server (restarted, or another daemon's) gives out the same window ids.
#[derive(Debug, Default)]
struct Renumbering {
    terminals: Mutex<Vec<TerminalInfo>>,
}

impl Renumbering {
    /// The first terminal is now a different one, at the same target.
    fn replace_first(&self) {
        if let Some(t) = self.terminals.lock().unwrap().first_mut() {
            t.id = TerminalId::new();
        }
    }
    fn find(&self, id: TerminalId) -> Result<TerminalInfo, RuntimeError> {
        self.terminals
            .lock()
            .unwrap()
            .iter()
            .find(|t| t.id == id)
            .cloned()
            .ok_or(RuntimeError::NotFound(id))
    }
}

impl Runtime for Renumbering {
    fn kind(&self) -> RuntimeKind {
        RuntimeKind::Tmux
    }
    fn start(&self, spec: &StartSpec) -> Result<TerminalInfo, RuntimeError> {
        let mut all = self.terminals.lock().unwrap();
        let info = TerminalInfo {
            id: TerminalId::new(),
            name: spec.name.clone(),
            pid: None,
            alive: true,
            native_target: Some(format!("pitcrew:@{}", all.len() + 1)),
        };
        all.push(info.clone());
        Ok(info)
    }
    fn write(&self, id: TerminalId, _: &[u8]) -> Result<(), RuntimeError> {
        self.find(id).map(drop)
    }
    fn send_keys(&self, id: TerminalId, _: &[Key]) -> Result<(), RuntimeError> {
        self.find(id).map(drop)
    }
    fn resize(&self, id: TerminalId, _: u16, _: u16) -> Result<(), RuntimeError> {
        self.find(id).map(drop)
    }
    fn screen(&self, id: TerminalId) -> Result<Screen, RuntimeError> {
        self.find(id).map(|_| Screen::default())
    }
    fn read_output(
        &self,
        id: TerminalId,
        from: u64,
        _: usize,
    ) -> Result<OutputChunk, RuntimeError> {
        self.find(id).map(|_| OutputChunk {
            offset: from,
            data: Vec::new(),
            end: from,
            truncated: false,
        })
    }
    fn info(&self, id: TerminalId) -> Result<TerminalInfo, RuntimeError> {
        self.find(id)
    }
    fn list(&self) -> Result<Vec<TerminalInfo>, RuntimeError> {
        Ok(self.terminals.lock().unwrap().clone())
    }
    fn kill(&self, id: TerminalId) -> Result<(), RuntimeError> {
        self.find(id).map(drop)
    }
}

/// After a restart, a link is kept only for a terminal the runtime still lists by its id. One
/// whose target now belongs to a different terminal (another server reused it) is forgotten, not
/// moved onto that terminal; one whose terminal is gone is forgotten too.
#[test]
fn links_follow_terminal_ids_never_targets() {
    use pitcrew_api::Terminals as _;
    let state = tempfile::tempdir().unwrap();
    let runtime = Arc::new(Renumbering::default());
    let (replaced, kept, lost) = (SessionId::new(), SessionId::new(), SessionId::new());
    let b = {
        let runner = runner(state.path());
        let terminals: RunnerTerminals = runner.terminals_with(runtime.clone(), options()).unwrap();
        let a = runtime.start(&spec()).unwrap().id;
        let b = runtime.start(&spec()).unwrap().id;
        let c = runtime.start(&spec()).unwrap().id;
        terminals.link(replaced, a).unwrap();
        terminals.link(kept, b).unwrap();
        terminals.link(lost, c).unwrap();
        assert!(terminals.attach(replaced).is_ok());
        runner.stop();
        b
    };

    // The first window is now another terminal at the same target; the third is gone.
    runtime.replace_first();
    runtime.terminals.lock().unwrap().truncate(2);
    let stranger = runtime.list().unwrap()[0].clone();
    let runner = runner(state.path());
    let terminals = runner.terminals_with(runtime.clone(), options()).unwrap();
    assert_eq!(terminals.terminal_of(replaced).unwrap(), None);
    assert!(terminals.attach(replaced).is_err());
    assert_eq!(terminals.session_of(stranger.id).unwrap(), None);
    assert_eq!(terminals.terminal_of(kept).unwrap(), Some(b));
    assert_eq!(terminals.session_of(b).unwrap(), Some(kept));
    assert!(terminals.attach(kept).is_ok());
    assert_eq!(terminals.terminal_of(lost).unwrap(), None);
    assert!(terminals.attach(lost).is_err());
    runner.stop();
}
