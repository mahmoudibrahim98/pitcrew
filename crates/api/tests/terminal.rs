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
/// resizes, a switch that makes the terminal disappear, and one that stalls writes (at most 3 s;
/// `stalled` counts the writes that began stalling).
#[derive(Debug, Default)]
struct TestRuntime {
    inner: FakeRuntime,
    dropped: AtomicU64,
    resizes: Mutex<Vec<(u16, u16)>>,
    gone: AtomicBool,
    stall_writes: AtomicBool,
    stalled: AtomicU64,
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
        if self.stall_writes.load(Ordering::SeqCst) {
            self.stalled.fetch_add(1, Ordering::SeqCst);
            Stalling::hang(&self.stall_writes);
        }
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
    // Wide margins, so a loaded machine cannot make the answering client look silent: it reads
    // every 50 ms and has a whole second for each Pong.
    const PING_EVERY: Duration = Duration::from_millis(100);
    const PONG_TIMEOUT: Duration = Duration::from_secs(1);
    let s = setup_with(TerminalConfig {
        ping_every: PING_EVERY,
        pong_timeout: PONG_TIMEOUT,
        ..quick()
    });
    let addr = serve(s.app.clone()).await;
    let token = s.fixture.device_token.clone();
    let (runtime, terminal, session) = (s.runtime.clone(), s.terminal, s.session);
    tokio::task::spawn_blocking(move || {
        let path = format!("/v1/sessions/{session}/terminal");

        // A client that reads nothing sends no pongs. It stays silent well past the first
        // Ping's deadline (about 1.1 s), so it cannot answer late by accident.
        let silent = {
            let mut silent = connect(addr, &token, &path);
            std::thread::spawn(move || {
                std::thread::sleep(PING_EVERY + PONG_TIMEOUT + Duration::from_millis(1500));
                read_close(&mut silent)
            })
        };

        // A client that keeps reading answers the pings (tungstenite sends the pongs) and stays.
        let mut answering = connect(addr, &token, &path);
        answering
            .get_mut()
            .set_read_timeout(Some(Duration::from_millis(50)))
            .unwrap();
        let pings = answer_pings(&mut answering, PONG_TIMEOUT + Duration::from_millis(500));
        assert!(pings >= 3, "{pings} pings in 1.5 s");
        answering
            .get_mut()
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        runtime.write(terminal, b"alive").unwrap();
        assert_eq!(read_bytes(&mut answering, 5), b"alive");

        assert_eq!(silent.join().unwrap(), CloseCode::Again);
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stalled_write_holds_up_neither_output_nor_pings() {
    // The pump would give up on a full queue after 300 ms (1013); the stalled write is given
    // up after 1.5 s (1011). Output must keep flowing in between.
    let s = setup_with(TerminalConfig {
        queue_frames: 2,
        send_timeout: Duration::from_millis(300),
        call_timeout: Duration::from_millis(1500),
        ..quick()
    });
    let addr = serve(s.app.clone()).await;
    let token = s.fixture.device_token.clone();
    let (runtime, terminal, session) = (s.runtime.clone(), s.terminal, s.session);
    tokio::task::spawn_blocking(move || {
        let mut socket = connect(addr, &token, &format!("/v1/sessions/{session}/terminal"));
        runtime.stall_writes.store(true, Ordering::SeqCst);
        let started = Instant::now();
        socket.send(Message::Binary(b"k".to_vec().into())).unwrap();
        wait_for("the stalled write", || {
            runtime.stalled.load(Ordering::SeqCst) == 1
        });
        let stalled_since = Instant::now();

        // Output while the write hangs: more than the queue holds, for longer than the pump
        // would wait on a full queue. Before, nothing drained the queue during a stall.
        let mut rounds = 0;
        let bound = Duration::from_millis(1200);
        let flowing_for = Duration::from_millis(400);
        while rounds < 5 || stalled_since.elapsed() < flowing_for {
            assert!(
                started.elapsed() < bound,
                "{rounds} rounds before the stall deadline"
            );
            socket
                .get_mut()
                .set_read_timeout(Some(bound.saturating_sub(started.elapsed())))
                .unwrap();
            runtime.inner.write(terminal, b"0123456789").unwrap();
            assert_eq!(read_bytes(&mut socket, 10), b"0123456789");
            rounds += 1;
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            rounds >= 5 && stalled_since.elapsed() >= flowing_for && started.elapsed() < bound,
            "{rounds} rounds in {:?} during the stall",
            started.elapsed()
        );
        socket
            .get_mut()
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();

        // The stalled runtime, not the reader, is what ends the stream.
        assert_eq!(read_close(&mut socket), CloseCode::Error);
        runtime.stall_writes.store(false, Ordering::SeqCst);
    })
    .await
    .unwrap();
}

/// Reads for `how_long`, which answers the server's Pings (tungstenite queues each Pong and sends
/// it with the next read or write); returns how many Pings came. Anything else fails the test.
fn answer_pings(socket: &mut WebSocket<TcpStream>, how_long: Duration) -> usize {
    let mut pings = 0;
    let until = Instant::now() + how_long;
    while Instant::now() < until {
        match socket.read() {
            Ok(Message::Ping(_)) => pings += 1,
            Ok(other) => panic!("unexpected {other:?}"),
            Err(tungstenite::Error::Io(e))
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(e) => panic!("the client was dropped: {e}"),
        }
    }
    pings
}

#[tokio::test(flavor = "multi_thread")]
async fn a_client_answering_pings_stays_while_its_input_backs_up() {
    // A Ping every 100 ms, each Pong due within 300 ms. One write stalls for 2 s (more than six
    // Pong timeouts) while the client keeps typing and answering, so the socket stops being read
    // with its Pongs queued behind keystrokes. The stall is shorter than `call_timeout`: nothing
    // should close the socket.
    const PING_EVERY: Duration = Duration::from_millis(100);
    const PONG_TIMEOUT: Duration = Duration::from_millis(300);
    const STALL: Duration = Duration::from_secs(2);
    let s = setup_with(TerminalConfig {
        ping_every: PING_EVERY,
        pong_timeout: PONG_TIMEOUT,
        call_timeout: Duration::from_secs(10),
        ..quick()
    });
    let addr = serve(s.app.clone()).await;
    let token = s.fixture.device_token.clone();
    let (runtime, terminal, session) = (s.runtime.clone(), s.terminal, s.session);
    tokio::task::spawn_blocking(move || {
        let mut socket = connect(addr, &token, &format!("/v1/sessions/{session}/terminal"));
        let read_timeout = |socket: &mut WebSocket<TcpStream>, wait: Duration| {
            socket.get_mut().set_read_timeout(Some(wait)).unwrap();
        };
        read_timeout(&mut socket, Duration::from_millis(10));
        runtime.stall_writes.store(true, Ordering::SeqCst);
        let mut typed = Vec::new();
        let mut pings = 0;
        let started = Instant::now();
        while started.elapsed() < STALL {
            // A keystroke about every 10 ms: many more than the input queue holds (16).
            let key = b"abcdefghijklmnopqrstuvwxyz"[typed.len() % 26];
            socket.send(Message::Binary(vec![key].into())).unwrap();
            typed.push(key);
            pings += answer_pings(&mut socket, Duration::from_millis(10));
        }
        assert_eq!(
            runtime.stalled.load(Ordering::SeqCst),
            1,
            "one write stalls"
        );
        assert!(
            typed.len() > 40,
            "{} keystrokes during the stall",
            typed.len()
        );
        assert!(pings >= 1, "no Ping during the stall");
        runtime.stall_writes.store(false, Ordering::SeqCst);

        // Every keystroke is written, in order (the fake echoes them), and the socket is open.
        read_timeout(&mut socket, Duration::from_secs(5));
        assert_eq!(read_bytes(&mut socket, typed.len()), typed);
        assert_eq!(output(&runtime, terminal), typed);

        // The deadline runs again: a client that keeps answering stays...
        read_timeout(&mut socket, Duration::from_millis(50));
        let after = answer_pings(&mut socket, PONG_TIMEOUT * 4);
        assert!(after >= 3, "{after} pings after the stall");
        // ...and one that stops answering is closed with 1013.
        std::thread::sleep(PING_EVERY + PONG_TIMEOUT + Duration::from_millis(700));
        read_timeout(&mut socket, Duration::from_secs(5));
        assert_eq!(read_close(&mut socket), CloseCode::Again);
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_message_over_max_inbound_closes_with_1009() {
    assert_eq!(TerminalConfig::default().max_inbound, 1 << 20);
    const LIMIT: usize = 1024;
    let s = setup_with(TerminalConfig {
        max_inbound: LIMIT,
        max_frame: 4096,
        ..quick()
    });
    let addr = serve(s.app.clone()).await;
    let token = s.fixture.device_token.clone();
    let (runtime, terminal, session) = (s.runtime.clone(), s.terminal, s.session);
    tokio::task::spawn_blocking(move || {
        let path = format!("/v1/sessions/{session}/terminal");
        let mut socket = connect(addr, &token, &path);
        // At the limit: written (and echoed by the fake).
        socket
            .send(Message::Binary(vec![b'a'; LIMIT].into()))
            .unwrap();
        assert_eq!(read_bytes(&mut socket, LIMIT), vec![b'a'; LIMIT]);
        // One byte over: 1009, and nothing written.
        socket
            .send(Message::Binary(vec![b'b'; LIMIT + 1].into()))
            .unwrap();
        assert_eq!(read_close(&mut socket), CloseCode::Size);
        assert_eq!(output(&runtime, terminal), vec![b'a'; LIMIT]);

        // The server is fine: a new client resumes where the first stopped.
        let mut again = connect(addr, &token, &format!("{path}?from={LIMIT}"));
        runtime.write(terminal, b"next").unwrap();
        assert_eq!(read_bytes(&mut again, 4), b"next");
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
    tokio::task::spawn_blocking(move || {
        let stream = TcpStream::connect(addr).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut request = format!("ws://{addr}{path}").into_client_request().unwrap();
        request.headers_mut().insert(
            "sec-websocket-protocol",
            HeaderValue::from_str(&format!("pitcrew.v1, pitcrew.bearer.{token}")).unwrap(),
        );
        match tungstenite::client(request, stream) {
            Ok((mut socket, response)) => {
                assert_eq!(response.headers()["sec-websocket-protocol"], "pitcrew.v1");
                assert_eq!(read_close(&mut socket), CloseCode::Error);
            }
            Err(tungstenite::HandshakeError::Failure(tungstenite::Error::Http(response))) => {
                // Calls that time out before the upgrade answer 503; after it they close 1011.
                assert_eq!(response.status(), 503);
                let body: serde_json::Value =
                    serde_json::from_slice(response.body().as_ref().unwrap()).unwrap();
                assert_eq!(body["code"], "unavailable");
            }
            Err(error) => panic!("unexpected handshake failure: {error}"),
        }
    })
    .await
    .unwrap();
    stalling.read.store(false, Ordering::SeqCst);
}

/// Hub shutdown closes terminals and the stream with 1001, and `serve` returns only once those
/// closing handshakes are done. So a daemon whose `main` drops the runtime as soon as `serve`
/// returns (as this test does) still closes every client cleanly.
#[test]
fn hub_shutdown_closes_sockets_with_1001_before_serve_returns() {
    // Each client answers the server's Close this long after reading it.
    const REPLY_AFTER: Duration = Duration::from_millis(300);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
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
    let (stop, stopped) = oneshot::channel::<()>();
    let (addr, server) = runtime.block_on(async {
        let bound = Bound::bind(&Listen::DevTcp {
            addr: "127.0.0.1:0".parse().unwrap(),
        })
        .await
        .unwrap();
        let addr = bound.tcp_addr().unwrap();
        let server = tokio::spawn(bound.serve(app, async {
            let _ = stopped.await;
        }));
        (addr, server)
    });

    let (ready, opened) = std::sync::mpsc::channel();
    let paths = [
        format!("/v1/sessions/{}/terminal", s.session),
        "/v1/stream".to_owned(),
    ];
    let clients: Vec<_> = paths
        .into_iter()
        .map(|path| {
            let (token, ready) = (s.fixture.device_token.clone(), ready.clone());
            std::thread::spawn(move || {
                let mut socket = connect(addr, &token, &path);
                if path == "/v1/stream" {
                    // The hello: the stream's session is running.
                    assert!(matches!(socket.read().unwrap(), Message::Text(_)));
                }
                ready.send(()).unwrap();
                let code = read_close(&mut socket);
                std::thread::sleep(REPLY_AFTER);
                // Sends the Close reply tungstenite queued when it read the server's Close.
                let _ = socket.flush();
                code
            })
        })
        .collect();
    for _ in &clients {
        opened.recv().unwrap();
    }

    let stopping = Instant::now();
    stop.send(()).unwrap();
    runtime.block_on(server).unwrap().unwrap();
    let took = stopping.elapsed();
    // As a daemon's `main` would: nothing more runs once `serve` has returned.
    drop(runtime);
    for client in clients {
        assert_eq!(client.join().unwrap(), CloseCode::Away);
    }
    // `serve` waited for the clients' replies, not only for the shutdown signal.
    assert!(took >= REPLY_AFTER, "serve returned after {took:?}");
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
