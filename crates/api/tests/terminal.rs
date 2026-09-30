//! `GET /v1/sessions/{id}/terminal` through the real router, over development TCP, with a
//! WebSocket client and `FakeRuntime` underneath.

#![allow(clippy::unwrap_used)]

mod common;

use common::{Fixture, call, get_request};
use pitcrew_api::{
    Attachment, Bound, EventSource, Listen, MemorySource, RouterParts, RuntimeTerminals,
    StreamConfig, TerminalConfig, TerminalError, Terminals, local_host_info, stream, terminal,
};
use pitcrew_auth::TokenStore;
use pitcrew_interfaces::fake::FakeRuntime;
use pitcrew_interfaces::runtime::{
    OutputChunk, Runtime, RuntimeError, RuntimeKind, Screen, StartSpec, TerminalInfo,
};
use pitcrew_protocol::ids::{SessionId, TerminalId};
use pitcrew_protocol::runner::Key;
use std::net::{SocketAddr, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::oneshot;
use tungstenite::client::IntoClientRequest as _;
use tungstenite::http::HeaderValue;
use tungstenite::protocol::frame::coding::CloseCode;
use tungstenite::{Message, WebSocket};

/// `FakeRuntime`, plus a replay buffer that has lost its first `dropped` bytes, a record of
/// resizes, and a switch that makes the terminal disappear.
#[derive(Debug, Default)]
struct TestRuntime {
    inner: FakeRuntime,
    dropped: AtomicU64,
    resizes: Mutex<Vec<(u16, u16)>>,
    gone: AtomicBool,
}

impl TestRuntime {
    fn check(&self, id: TerminalId) -> Result<(), RuntimeError> {
        if self.gone.load(Ordering::SeqCst) {
            return Err(RuntimeError::NotFound(id));
        }
        Ok(())
    }
}

impl Runtime for TestRuntime {
    fn kind(&self) -> RuntimeKind {
        self.inner.kind()
    }
    fn start(&self, spec: &StartSpec) -> Result<TerminalInfo, RuntimeError> {
        self.inner.start(spec)
    }
    fn write(&self, id: TerminalId, bytes: &[u8]) -> Result<(), RuntimeError> {
        self.check(id)?;
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

struct Setup {
    fixture: Fixture,
    runtime: Arc<TestRuntime>,
    terminal: TerminalId,
    session: SessionId,
    app: axum::Router,
}

fn quick() -> TerminalConfig {
    TerminalConfig {
        poll_min: Duration::from_millis(2),
        poll_max: Duration::from_millis(20),
        max_frame: 8,
        ..TerminalConfig::default()
    }
}

fn setup() -> Setup {
    setup_with(quick())
}

fn setup_with(config: TerminalConfig) -> Setup {
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
    let app = app(
        &fixture,
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

fn app(fixture: &Fixture, parts: RouterParts) -> axum::Router {
    let tokens: Arc<dyn TokenStore> = fixture.tokens.clone();
    pitcrew_api::router(local_host_info("0.0.0-test", vec![], vec![]), tokens, parts)
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
            Message::Ping(_) => {}
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

/// Reads until a Close frame; returns its code. Output and pings before it are skipped.
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
async fn sizes_are_1_to_1000_on_the_query_and_in_resize() {
    let s = setup();
    let path = format!("/v1/sessions/{}/terminal", s.session);
    for query in [
        "?cols=1001&rows=10",
        "?cols=10&rows=1001",
        "?cols=0&rows=10",
        "?cols=10&rows=0",
        "?cols=70000&rows=10",
    ] {
        let (status, body) = call(
            s.app.clone(),
            get_request(&format!("{path}{query}"), Some(&s.fixture.device_token)),
        )
        .await;
        assert_eq!(status, 400, "{query}");
        assert_eq!(body["code"], "invalid", "{query}");
    }

    let addr = serve(s.app.clone()).await;
    let token = s.fixture.device_token.clone();
    let runtime = s.runtime.clone();
    tokio::task::spawn_blocking(move || {
        let mut socket = connect(addr, &token, &format!("{path}?cols=1000&rows=1000"));
        wait_for("the largest size", || {
            runtime.resizes.lock().unwrap().contains(&(1000, 1000))
        });
        socket
            .send(Message::Text(
                r#"{"type":"resize","cols":1001,"rows":40}"#.into(),
            ))
            .unwrap();
        assert_eq!(read_close(&mut socket), CloseCode::Invalid);
        assert_eq!(*runtime.resizes.lock().unwrap(), vec![(1000, 1000)]);
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn a_get_without_an_upgrade_never_resizes() {
    let s = setup();
    let path = format!("/v1/sessions/{}/terminal?cols=100&rows=30", s.session);
    let (status, body) = call(
        s.app.clone(),
        get_request(&path, Some(&s.fixture.device_token)),
    )
    .await;
    assert_eq!(status, 400);
    assert!(
        body["message"].as_str().unwrap().contains("upgrade"),
        "{body}"
    );
    assert!(s.runtime.resizes.lock().unwrap().is_empty());
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

#[tokio::test(flavor = "multi_thread")]
async fn a_terminal_that_disappears_mid_stream_is_an_exit() {
    let s = setup();
    let addr = serve(s.app.clone()).await;
    let token = s.fixture.device_token.clone();
    let (runtime, terminal, session) = (s.runtime.clone(), s.terminal, s.session);
    tokio::task::spawn_blocking(move || {
        let mut socket = connect(addr, &token, &format!("/v1/sessions/{session}/terminal"));
        runtime.write(terminal, b"before").unwrap();
        assert_eq!(read_bytes(&mut socket, 6), b"before");
        runtime.gone.store(true, Ordering::SeqCst);
        assert_eq!(read_text(&mut socket), serde_json::json!({"type": "exit"}));
        assert_eq!(read_close(&mut socket), CloseCode::Normal);
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn two_clients_share_one_terminal() {
    let s = setup();
    let addr = serve(s.app.clone()).await;
    let token = s.fixture.device_token.clone();
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
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_client_that_stops_reading_is_closed_with_1013() {
    let s = setup_with(TerminalConfig {
        max_frame: 64 * 1024,
        queue_frames: 4,
        send_timeout: Duration::from_millis(200),
        ..quick()
    });
    // Far more than the socket buffers hold, so sending blocks.
    const TOTAL: usize = 32 << 20;
    s.runtime.write(s.terminal, &vec![b'x'; TOTAL]).unwrap();
    let addr = serve(s.app.clone()).await;
    let token = s.fixture.device_token.clone();
    let session = s.session;
    tokio::task::spawn_blocking(move || {
        let mut socket = connect(addr, &token, &format!("/v1/sessions/{session}/terminal"));
        // Not reading while the server gives up on us; then catch up and find the close.
        std::thread::sleep(Duration::from_millis(400));
        let mut received = 0;
        let code = loop {
            match socket.read().unwrap() {
                Message::Binary(data) => received += data.len(),
                Message::Close(Some(frame)) => break frame.code,
                other => panic!("unexpected {other:?}"),
            }
        };
        assert_eq!(code, CloseCode::Again);
        assert!(received < TOTAL, "the whole output fit in the buffers");
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn keepalive_pings_and_closes_a_client_that_stops_answering() {
    let s = setup_with(TerminalConfig {
        ping_every: Duration::from_millis(50),
        pong_timeout: Duration::from_millis(150),
        ..quick()
    });
    let addr = serve(s.app.clone()).await;
    let token = s.fixture.device_token.clone();
    let (runtime, terminal, session) = (s.runtime.clone(), s.terminal, s.session);
    tokio::task::spawn_blocking(move || {
        let path = format!("/v1/sessions/{session}/terminal");

        // A client that keeps reading answers the pings (tungstenite sends the pongs) and stays.
        let mut answering = connect(addr, &token, &path);
        answering
            .get_mut()
            .set_read_timeout(Some(Duration::from_millis(20)))
            .unwrap();
        let mut pings = 0;
        let until = Instant::now() + Duration::from_millis(600);
        while Instant::now() < until {
            match answering.read() {
                Ok(Message::Ping(_)) => pings += 1,
                Ok(other) => panic!("unexpected {other:?}"),
                Err(tungstenite::Error::Io(e))
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) => {}
                Err(e) => panic!("the answering client was dropped: {e}"),
            }
        }
        assert!(pings >= 3, "{pings} pings in 600 ms");
        answering
            .get_mut()
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        runtime.write(terminal, b"alive").unwrap();
        assert_eq!(read_bytes(&mut answering, 5), b"alive");

        // A client that reads nothing sends no pongs, and is closed.
        let mut silent = connect(addr, &token, &path);
        std::thread::sleep(Duration::from_millis(600));
        assert_eq!(read_close(&mut silent), CloseCode::Again);
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_client_close_is_answered() {
    let s = setup();
    let addr = serve(s.app.clone()).await;
    let token = s.fixture.device_token.clone();
    let session = s.session;
    tokio::task::spawn_blocking(move || {
        let mut socket = connect(addr, &token, &format!("/v1/sessions/{session}/terminal"));
        socket.close(None).unwrap();
        // The server's reply, not a reset.
        loop {
            match socket.read() {
                Ok(Message::Close(_)) => break,
                Ok(Message::Ping(_)) => {}
                other => panic!("expected the close reply, got {other:?}"),
            }
        }
        assert!(matches!(
            socket.read(),
            Err(tungstenite::Error::ConnectionClosed)
        ));
    })
    .await
    .unwrap();
}

/// A runtime that stalls: `attach` or `read` block while their switch is on (at most 3 s).
#[derive(Debug, Default)]
struct Stalling {
    attach: AtomicBool,
    read: AtomicBool,
}

impl Stalling {
    fn hang(switch: &AtomicBool) {
        let until = Instant::now() + Duration::from_secs(3);
        while switch.load(Ordering::SeqCst) && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

#[derive(Debug)]
struct StallingTerminals(Arc<Stalling>);

impl Terminals for StallingTerminals {
    fn attach(&self, _session: SessionId) -> Result<Arc<dyn Attachment>, TerminalError> {
        Stalling::hang(&self.0.attach);
        Ok(Arc::new(StallingAttachment(Arc::clone(&self.0))))
    }
}

#[derive(Debug)]
struct StallingAttachment(Arc<Stalling>);

impl Attachment for StallingAttachment {
    fn read(&self, from: u64, _max: usize) -> Result<OutputChunk, TerminalError> {
        Stalling::hang(&self.0.read);
        Ok(OutputChunk {
            offset: from,
            data: vec![],
            end: from,
            truncated: false,
        })
    }
    fn write(&self, _bytes: &[u8]) -> Result<(), TerminalError> {
        Ok(())
    }
    fn resize(&self, _cols: u16, _rows: u16) -> Result<(), TerminalError> {
        Ok(())
    }
    fn exited(&self) -> Result<bool, TerminalError> {
        Ok(false)
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stalled_runtime_answers_503_or_closes_with_1011() {
    let fixture = Fixture::new();
    let stalling = Arc::new(Stalling::default());
    let config = TerminalConfig {
        call_timeout: Duration::from_millis(200),
        ..quick()
    };
    let app = app(
        &fixture,
        RouterParts::new().device(terminal::routes(
            Arc::new(StallingTerminals(Arc::clone(&stalling))),
            config,
        )),
    );
    let path = format!("/v1/sessions/{}/terminal", SessionId::new());

    stalling.attach.store(true, Ordering::SeqCst);
    let started = Instant::now();
    let (status, body) = call(app.clone(), get_request(&path, Some(&fixture.device_token))).await;
    assert_eq!(status, 503, "{body}");
    assert_eq!(body["code"], "unavailable");
    assert!(started.elapsed() < Duration::from_secs(2));
    stalling.attach.store(false, Ordering::SeqCst);

    stalling.read.store(true, Ordering::SeqCst);
    let addr = serve(app).await;
    let token = fixture.device_token.clone();
    let code = tokio::task::spawn_blocking(move || {
        let mut socket = connect(addr, &token, &path);
        read_close(&mut socket)
    })
    .await
    .unwrap();
    assert_eq!(code, CloseCode::Error);
    stalling.read.store(false, Ordering::SeqCst);
}

#[tokio::test(flavor = "multi_thread")]
async fn hub_shutdown_closes_terminals_and_the_stream_with_1001() {
    let s = setup();
    let source: Arc<dyn EventSource> = Arc::new(MemorySource::new("log", 16));
    let terminals = Arc::new(RuntimeTerminals::new(s.runtime.clone()));
    terminals.link(s.session, s.terminal);
    let app = app(
        &s.fixture,
        RouterParts::new()
            .device(terminal::routes(terminals, quick()))
            .device(stream::routes(source, StreamConfig::default())),
    );
    let bound = Bound::bind(&Listen::DevTcp {
        addr: "127.0.0.1:0".parse().unwrap(),
    })
    .await
    .unwrap();
    let addr = bound.tcp_addr().unwrap();
    let (stop, stopped) = oneshot::channel::<()>();
    let server = tokio::spawn(bound.serve(app, async {
        let _ = stopped.await;
    }));

    let token = s.fixture.device_token.clone();
    let session = s.session;
    let (opened, ready) = oneshot::channel();
    let clients = tokio::task::spawn_blocking(move || {
        let mut terminal = connect(addr, &token, &format!("/v1/sessions/{session}/terminal"));
        let mut stream = connect(addr, &token, "/v1/stream");
        // The stream's hello: the session is running.
        assert!(matches!(stream.read().unwrap(), Message::Text(_)));
        opened.send(()).unwrap();
        (read_close(&mut terminal), read_close(&mut stream))
    });
    ready.await.unwrap();
    stop.send(()).unwrap();
    let (terminal, stream) = clients.await.unwrap();
    assert_eq!(terminal, CloseCode::Away);
    assert_eq!(stream, CloseCode::Away);
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap()
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

    // Unknown sessions are 404 even without an upgrade.
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
