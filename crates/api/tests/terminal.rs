//! `GET /v1/sessions/{id}/terminal` through the real router, over development TCP, with a
//! WebSocket client and `FakeRuntime` underneath.

#![allow(clippy::unwrap_used)]

mod common;

use common::{Fixture, call, get_request};
use pitcrew_api::{
    Bound, Listen, RouterParts, RuntimeTerminals, TerminalConfig, local_host_info, terminal,
};
use pitcrew_auth::TokenStore;
use pitcrew_interfaces::fake::FakeRuntime;
use pitcrew_interfaces::runtime::{
    OutputChunk, Runtime, RuntimeError, RuntimeKind, Screen, StartSpec, TerminalInfo,
};
use pitcrew_protocol::ids::{SessionId, TerminalId};
use pitcrew_protocol::runner::Key;
use std::net::{SocketAddr, TcpStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tungstenite::client::IntoClientRequest as _;
use tungstenite::http::HeaderValue;
use tungstenite::protocol::frame::coding::CloseCode;
use tungstenite::{Message, WebSocket};

/// `FakeRuntime`, plus a replay buffer that has lost its first `dropped` bytes, and a record of
/// resizes.
#[derive(Debug, Default)]
struct TestRuntime {
    inner: FakeRuntime,
    dropped: AtomicU64,
    resizes: Mutex<Vec<(u16, u16)>>,
}

impl Runtime for TestRuntime {
    fn kind(&self) -> RuntimeKind {
        self.inner.kind()
    }
    fn start(&self, spec: &StartSpec) -> Result<TerminalInfo, RuntimeError> {
        self.inner.start(spec)
    }
    fn write(&self, id: TerminalId, bytes: &[u8]) -> Result<(), RuntimeError> {
        self.inner.write(id, bytes)
    }
    fn send_keys(&self, id: TerminalId, keys: &[Key]) -> Result<(), RuntimeError> {
        self.inner.send_keys(id, keys)
    }
    fn resize(&self, id: TerminalId, cols: u16, rows: u16) -> Result<(), RuntimeError> {
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
        let dropped = self.dropped.load(Ordering::SeqCst);
        let mut chunk = self.inner.read_output(id, from.max(dropped), max)?;
        chunk.truncated = from < dropped;
        Ok(chunk)
    }
    fn info(&self, id: TerminalId) -> Result<TerminalInfo, RuntimeError> {
        self.inner.info(id)
    }
    fn list(&self) -> Result<Vec<TerminalInfo>, RuntimeError> {
        self.inner.list()
    }
    fn kill(&self, id: TerminalId) -> Result<(), RuntimeError> {
        self.inner.kill(id)
    }
}

struct Setup {
    fixture: Fixture,
    runtime: Arc<TestRuntime>,
    terminal: TerminalId,
    session: SessionId,
    app: axum::Router,
}

fn setup() -> Setup {
    let fixture = Fixture::new();
    let runtime = Arc::new(TestRuntime::default());
    let terminal = runtime
        .start(&StartSpec {
            program: "agent".into(),
            args: vec![],
            cwd: "/work".into(),
            env: vec![],
            name: "t".into(),
            cols: 80,
            rows: 24,
        })
        .unwrap()
        .id;
    let session = SessionId::new();
    let terminals = Arc::new(RuntimeTerminals::new(runtime.clone()));
    terminals.link(session, terminal);
    let tokens: Arc<dyn TokenStore> = fixture.tokens.clone();
    let config = TerminalConfig {
        poll_every: Duration::from_millis(2),
        max_frame: 8,
        ..TerminalConfig::default()
    };
    let app = pitcrew_api::router(
        local_host_info("0.0.0-test", vec![], vec![]),
        tokens,
        RouterParts::new().device(terminal::routes(terminals, config)),
    );
    Setup {
        fixture,
        runtime,
        terminal,
        session,
        app,
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

#[tokio::test(flavor = "multi_thread")]
async fn replay_live_and_resume_by_offset_lose_nothing() {
    let s = setup();
    let addr = serve(s.app.clone()).await;
    let token = s.fixture.device_token.clone();
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
    let token = s.fixture.device_token.clone();
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
    let token = s.fixture.device_token.clone();
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
    let token = s.fixture.device_token.clone();
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

#[tokio::test]
async fn the_terminal_needs_a_device_token_and_a_known_session() {
    let s = setup();
    let path = format!("/v1/sessions/{}/terminal", s.session);

    let (status, _) = call(s.app.clone(), get_request(&path, None)).await;
    assert_eq!(status, 401);

    let (status, body) = call(
        s.app.clone(),
        get_request(&path, Some(&s.fixture.agent_token)),
    )
    .await;
    assert_eq!(status, 403);
    assert_eq!(body["code"], "forbidden");

    for unknown in [
        format!("/v1/sessions/{}/terminal", SessionId::new()),
        "/v1/sessions/not-an-id/terminal".to_owned(),
    ] {
        let (status, body) = call(
            s.app.clone(),
            get_request(&unknown, Some(&s.fixture.device_token)),
        )
        .await;
        assert_eq!(status, 404, "{unknown}");
        assert_eq!(body["code"], "not_found");
    }

    // A known session, but no upgrade, or a bad size.
    for (query, message) in [("", "upgrade"), ("?cols=80", "rows")] {
        let (status, body) = call(
            s.app.clone(),
            get_request(&format!("{path}{query}"), Some(&s.fixture.device_token)),
        )
        .await;
        assert_eq!(status, 400, "{query}");
        assert!(
            body["message"].as_str().unwrap().contains(message),
            "{body}"
        );
    }
}
