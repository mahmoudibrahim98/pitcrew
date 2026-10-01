//! The gateway against a fake daemon on a unix socket: requests, the limits, the error mapping,
//! and sockets (order, 1006, 1009, 1013, Pings, closing, cleanup).

#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{FakeDaemon, Recorder, WORKSPACE_ID, WORKSPACE_NAME, close_of, closes_in};
use pitcrew_desktop::daemon::LocalConnector;
use pitcrew_desktop::daemon::endpoint::Endpoint;
use pitcrew_desktop::gateway::{
    Delivery, ErrorCode, Gateway, GatewayError, GatewayRequest, GatewayResponse, Payload,
};
use pitcrew_desktop::registry::{
    Connection, Registry, WorkspaceKind, WorkspaceRecord, WorkspaceState,
};
use std::sync::Arc;
use std::time::{Duration, Instant};

const TOKEN: &str = "pcd_gateway-test-token-0123456789";
/// A workspace whose daemon is down.
const DOWN: &str = "01JD0000000000000000000000";
const MIB: usize = 1024 * 1024;

/// Dropped in this order: the gateway (its sockets close), the runtime, the daemon.
struct World {
    gateway: Gateway,
    rt: tokio::runtime::Runtime,
    daemon: FakeDaemon,
    registry: Arc<Registry>,
    _tmp: tempfile::TempDir,
}

fn world() -> World {
    let tmp = tempfile::tempdir().unwrap();
    let daemon = FakeDaemon::start(&tmp.path().join("state"), TOKEN);
    let registry = Arc::new(Registry::in_memory());
    registry
        .insert(
            record(WORKSPACE_ID, WORKSPACE_NAME),
            Some(Arc::new(daemon.connector())),
            WorkspaceState::Ready,
        )
        .unwrap();
    let down = tmp.path().join("down");
    std::fs::create_dir_all(down.join("run")).unwrap();
    registry
        .insert(
            record(DOWN, "Down"),
            Some(Arc::new(LocalConnector::with_token_path(
                Endpoint::Unix {
                    dir: down.join("run"),
                },
                down.join("device.token"),
            ))),
            WorkspaceState::Ready,
        )
        .unwrap();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    World {
        gateway: Gateway::new(Arc::clone(&registry)),
        rt,
        daemon,
        registry,
        _tmp: tmp,
    }
}

fn record(id: &str, name: &str) -> WorkspaceRecord {
    WorkspaceRecord {
        id: id.into(),
        name: name.into(),
        kind: WorkspaceKind::Local,
        connection: Connection::Local,
    }
}

impl World {
    fn request(
        &self,
        workspace: &str,
        method: &str,
        path: &str,
        body: Option<String>,
    ) -> Result<GatewayResponse, GatewayError> {
        self.rt.block_on(self.gateway.request(GatewayRequest {
            workspace: workspace.into(),
            method: method.into(),
            path: path.into(),
            body,
        }))
    }

    fn get(&self, path: &str) -> Result<GatewayResponse, GatewayError> {
        self.request(WORKSPACE_ID, "GET", path, None)
    }

    fn open(&self, owner: &str, path: &str, sink: Arc<Recorder>) -> Result<u32, GatewayError> {
        self.rt
            .block_on(self.gateway.socket_open(owner, WORKSPACE_ID, path, sink))
    }

    fn send(&self, owner: &str, socket: u32, payload: Payload) -> Result<(), GatewayError> {
        self.rt
            .block_on(self.gateway.socket_send(owner, socket, payload))
    }

    fn wait_until_closed(&self) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while self.gateway.open_sockets() > 0 {
            assert!(Instant::now() < deadline, "sockets still open");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

#[test]
fn requests_come_back_whatever_their_status() {
    let w = world();
    let ok = w.get("/v1/tasks").unwrap();
    assert_eq!(ok.status, 200);
    assert_eq!(ok.content_type.as_deref(), Some("application/json"));
    assert!(ok.body.contains("Write the gateway"), "{}", ok.body);

    // A daemon error is a response, with its ApiError body, as over HTTP.
    let missing = w.get("/v1/nothing").unwrap();
    assert_eq!(missing.status, 404);
    assert!(missing.body.contains("\"not_found\""), "{}", missing.body);

    // The gateway sends Content-Type and Host itself.
    let posted = w
        .request(
            WORKSPACE_ID,
            "POST",
            "/v1/echo",
            Some(r#"{"title":"x"}"#.into()),
        )
        .unwrap();
    assert_eq!(posted.status, 201);
    let echoed: serde_json::Value = serde_json::from_str(&posted.body).unwrap();
    assert_eq!(echoed["contentType"], "application/json");
    assert_eq!(echoed["host"], "localhost");
    assert_eq!(echoed["body"], r#"{"title":"x"}"#);

    // The query goes on as it is.
    let query = w.get("/v1/query?b=2&a=1&a=%2Fhome%2Fsam").unwrap();
    assert_eq!(query.body, "b=2&a=1&a=%2Fhome%2Fsam");

    let empty = w.get("/v1/plain").unwrap();
    assert_eq!((empty.status, empty.body.as_str()), (204, ""));

    // The daemon's other headers (two of them hold the token) never come back.
    let json = serde_json::to_value(&ok).unwrap();
    let mut keys: Vec<_> = json.as_object().unwrap().keys().cloned().collect();
    keys.sort();
    assert_eq!(keys, ["body", "contentType", "status"]);
    assert!(!json.to_string().contains(TOKEN));

    // The daemon got the bearer token.
    assert!(
        w.daemon
            .seen
            .lock()
            .unwrap()
            .authorizations
            .contains(&format!("Bearer {TOKEN}"))
    );
}

#[test]
fn the_size_limits() {
    let w = world();
    // A request body of 1 MiB goes; one byte more does not.
    let sent = w
        .request(WORKSPACE_ID, "POST", "/v1/echo", Some("x".repeat(MIB)))
        .unwrap();
    assert_eq!(sent.status, 201);
    let too_big = w
        .request(WORKSPACE_ID, "POST", "/v1/echo", Some("x".repeat(MIB + 1)))
        .unwrap_err();
    assert_eq!(too_big.code, ErrorCode::TooLarge);

    // A response body of 32 MiB comes back; one byte more does not.
    let big = w.get(&format!("/v1/big?bytes={}", 32 * MIB)).unwrap();
    assert_eq!(big.body.len(), 32 * MIB);
    let too_big = w
        .get(&format!("/v1/big?bytes={}", 32 * MIB + 1))
        .unwrap_err();
    assert_eq!(too_big.code, ErrorCode::TooLarge);
}

#[test]
fn bad_paths_and_methods_are_refused_before_anything_is_sent() {
    let w = world();
    for path in [
        "/v2/tasks",
        "tasks",
        "/v1/../admin",
        "/v1/%2e%2e/admin",
        "/v1//tasks",
        "/v1/tasks\\x",
        "/v1/tasks#x",
        "/v1/tasks\r\nX-Injected: 1",
        "/v1/tasks\u{0}",
    ] {
        let e = w.get(path).unwrap_err();
        assert_eq!(e.code, ErrorCode::Invalid, "{path:?}");
    }
    for method in ["HEAD", "OPTIONS", "get", "CONNECT"] {
        let e = w
            .request(WORKSPACE_ID, method, "/v1/tasks", None)
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::Invalid, "{method}");
    }
    for path in ["/v1/tasks", "/v1/stream/x", "/v1/sessions/../terminal"] {
        let e = w.open("main", path, Recorder::keeping_up()).unwrap_err();
        assert_eq!(e.code, ErrorCode::Invalid, "{path}");
    }
    assert!(w.daemon.seen.lock().unwrap().authorizations.is_empty());
    assert!(w.daemon.seen.lock().unwrap().subprotocols.is_empty());
}

#[test]
fn errors_map_to_the_contract() {
    let w = world();
    // Unknown workspace.
    let e = w
        .request("01JX0000000000000000000000", "GET", "/v1/tasks", None)
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::UnknownWorkspace);
    let e =
        w.rt.block_on(
            w.gateway
                .socket_open("main", "nope", "/v1/stream", Recorder::keeping_up()),
        )
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::UnknownWorkspace);

    // Daemon down: never a made-up HTTP status.
    let e = w.request(DOWN, "GET", "/v1/tasks", None).unwrap_err();
    assert_eq!(e.code, ErrorCode::Unreachable, "{e:?}");
    let e =
        w.rt.block_on(
            w.gateway
                .socket_open("main", DOWN, "/v1/stream", Recorder::keeping_up()),
        )
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Unreachable);

    // 503 on upgrade is unreachable, with the status in the message.
    let e = w
        .open(
            "main",
            "/v1/sessions/down/terminal?cols=80&rows=24",
            Recorder::keeping_up(),
        )
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Unreachable);
    assert!(e.message.contains("HTTP 503"), "{}", e.message);
    assert!(e.message.contains("unreachable"), "{}", e.message);
    // Another refusal keeps its status too.
    let e = w
        .open("main", "/v1/sessions/nope/terminal", Recorder::keeping_up())
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Invalid);
    assert!(e.message.contains("HTTP 404"), "{}", e.message);

    // A workspace that needs pairing.
    w.registry.set_state(
        DOWN,
        WorkspaceState::NeedsPairing,
        Some("pair it first".into()),
    );
    let e = w.request(DOWN, "GET", "/v1/tasks", None).unwrap_err();
    assert_eq!(e.code, ErrorCode::NeedsPairing);
    assert_eq!(e.message, "pair it first");
}

#[test]
fn socket_messages_arrive_in_order_and_close_comes_last() {
    let w = world();
    let sink = Recorder::keeping_up();
    let socket = w
        .open("main", "/v1/stream?script=order", sink.clone())
        .unwrap();
    assert!(socket > 0);
    let messages = sink.wait_closed();
    assert_eq!(
        messages,
        vec![
            Delivery::Text("a".into()),
            Delivery::Binary(vec![1, 2, 3]),
            Delivery::Text("b".into()),
            Delivery::Close {
                code: 1000,
                reason: "bye".into()
            },
        ]
    );
    w.wait_until_closed();
    // The token went as a subprotocol, after pitcrew.v1.
    assert_eq!(
        w.daemon.seen.lock().unwrap().subprotocols,
        vec![format!("pitcrew.v1, pitcrew.bearer.{TOKEN}")]
    );
    // Sending on a closed socket is invalid.
    let e = w
        .send("main", socket, Payload::Text("late".into()))
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Invalid);
}

#[test]
fn a_broken_connection_closes_with_1006() {
    let w = world();
    let sink = Recorder::keeping_up();
    w.open("main", "/v1/stream?script=drop", sink.clone())
        .unwrap();
    let messages = sink.wait_closed();
    assert_eq!(messages[0], Delivery::Text("x".into()));
    assert_eq!(close_of(&messages), (1006, String::new()));
    assert_eq!(closes_in(&messages), 1);
}

#[test]
fn an_oversize_message_closes_with_1009() {
    let w = world();
    // The stream takes 4 KiB from the client.
    let sink = Recorder::keeping_up();
    let socket = w
        .open("main", "/v1/stream?script=hold", sink.clone())
        .unwrap();
    w.send("main", socket, Payload::Text("x".repeat(4096)))
        .unwrap();
    let e = w
        .send("main", socket, Payload::Text("x".repeat(4097)))
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::TooLarge);
    assert_eq!(close_of(&sink.wait_closed()).0, 1009);
    w.daemon.wait_for("1009 on the stream", |s| {
        s.closes.contains(&("hold".into(), Some(1009)))
    });
    let e = w
        .send("main", socket, Payload::Text("x".into()))
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Invalid);

    // A terminal takes 1 MiB.
    let sink = Recorder::keeping_up();
    let socket = w
        .open(
            "main",
            "/v1/sessions/01JS/terminal?cols=80&rows=24",
            sink.clone(),
        )
        .unwrap();
    w.send("main", socket, Payload::Binary(vec![9; MIB]))
        .unwrap();
    let echoed = sink.wait_for(1);
    assert_eq!(echoed[0], Delivery::Binary(vec![9; MIB]));
    let e = w
        .send("main", socket, Payload::Binary(vec![9; MIB + 1]))
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::TooLarge);
    assert_eq!(close_of(&sink.wait_closed()).0, 1009);
    w.daemon.wait_for("1009 on the terminal", |s| {
        s.closes.contains(&("echo".into(), Some(1009)))
    });

    // A daemon frame over the gateway's 8 MiB closes with 1009 too.
    let sink = Recorder::keeping_up();
    w.open("main", "/v1/stream?script=big", sink.clone())
        .unwrap();
    let messages = sink.wait_closed();
    assert_eq!(messages.len(), 1, "nothing but the close");
    assert_eq!(close_of(&messages).0, 1009);
    w.daemon.wait_for("1009 from the gateway", |s| {
        s.closes.contains(&("big".into(), Some(1009)))
    });
}

#[test]
fn a_webview_that_does_not_keep_up_is_closed_with_1013() {
    let w = world();
    let sink = Recorder::stalled();
    w.open("main", "/v1/stream?script=flood", sink.clone())
        .unwrap();
    let messages = sink.wait_closed();
    let (code, _) = close_of(&messages);
    assert_eq!(code, 1013);
    assert_eq!(closes_in(&messages), 1);
    let delivered: usize = messages
        .iter()
        .map(|m| match m {
            Delivery::Binary(b) => b.len(),
            _ => 0,
        })
        .sum();
    assert_eq!(delivered, 8 * MIB, "the budget, and not a byte more");
    w.daemon.wait_for("1013 from the gateway", |s| {
        s.closes.contains(&("flood".into(), Some(1013)))
    });
}

#[test]
fn a_webview_that_keeps_up_gets_everything() {
    let w = world();
    let sink = Recorder::keeping_up();
    w.open("main", "/v1/stream?script=flood", sink.clone())
        .unwrap();
    let messages = sink.wait_closed();
    assert_eq!(close_of(&messages), (1000, "done".into()));
    let delivered: usize = messages
        .iter()
        .map(|m| match m {
            Delivery::Binary(b) => b.len(),
            _ => 0,
        })
        .sum();
    assert_eq!(delivered, 16 * MIB);
}

#[test]
fn the_gateway_answers_pings_and_the_webview_never_sees_them() {
    let w = world();
    let sink = Recorder::keeping_up();
    w.open("main", "/v1/stream?script=ping", sink.clone())
        .unwrap();
    let messages = sink.wait_closed();
    assert_eq!(
        messages,
        vec![
            Delivery::Text("pong:are-you-there".into()),
            Delivery::Close {
                code: 1000,
                reason: String::new()
            },
        ]
    );
    assert_eq!(
        w.daemon.seen.lock().unwrap().pongs,
        vec![b"are-you-there".to_vec()]
    );
}

#[test]
fn closing_a_socket() {
    let w = world();
    let sink = Recorder::keeping_up();
    let socket = w
        .open("main", "/v1/sessions/01JS/terminal", sink.clone())
        .unwrap();
    w.send(
        "main",
        socket,
        Payload::Text(r#"{"type":"resize","cols":120,"rows":40}"#.into()),
    )
    .unwrap();
    assert_eq!(
        sink.wait_for(1)[0],
        Delivery::Text(r#"{"type":"resize","cols":120,"rows":40}"#.into())
    );

    for (code, reason) in [
        (Some(1006), None),
        (Some(1001), None),
        (Some(2999), None),
        (Some(5000), None),
        (None, Some("r".repeat(124))),
    ] {
        let e = w
            .gateway
            .socket_close("main", socket, code, reason)
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::Invalid, "{code:?}");
    }
    w.gateway
        .socket_close("main", socket, Some(4000), Some("done here".into()))
        .unwrap();
    let messages = sink.wait_closed();
    assert_eq!(close_of(&messages), (4000, "done here".into()));
    w.daemon
        .wait_for("4000", |s| s.closes.contains(&("echo".into(), Some(4000))));
    // Idempotent; and sending afterwards is invalid.
    w.gateway.socket_close("main", socket, None, None).unwrap();
    w.gateway.socket_close("main", 999, None, None).unwrap();
    assert_eq!(
        w.send("main", socket, Payload::Text("x".into()))
            .unwrap_err()
            .code,
        ErrorCode::Invalid
    );

    // 1000 is the default.
    let sink = Recorder::keeping_up();
    let socket = w
        .open("main", "/v1/stream?script=hold", sink.clone())
        .unwrap();
    w.gateway.socket_close("main", socket, None, None).unwrap();
    assert_eq!(close_of(&sink.wait_closed()), (1000, String::new()));
    w.daemon
        .wait_for("1000", |s| s.closes.contains(&("hold".into(), Some(1000))));
    w.wait_until_closed();
}

#[test]
fn sockets_belong_to_their_page_and_close_with_it() {
    let w = world();
    let main_sinks = [Recorder::keeping_up(), Recorder::keeping_up()];
    let mains: Vec<u32> = main_sinks
        .iter()
        .map(|sink| {
            w.open("main", "/v1/stream?script=hold", sink.clone())
                .unwrap()
        })
        .collect();
    let other_sink = Recorder::keeping_up();
    let other = w
        .open("other", "/v1/stream?script=hold", other_sink.clone())
        .unwrap();
    assert_eq!(w.gateway.open_sockets(), 3);

    // Another page cannot use or close them.
    let e = w
        .send("other", mains[0], Payload::Text("x".into()))
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Invalid);
    w.gateway
        .socket_close("other", mains[0], None, None)
        .unwrap();
    assert_eq!(w.gateway.open_sockets(), 3);

    // The window closes: its sockets close with 1001, and nothing is sent to the gone page.
    w.gateway.forget("main");
    w.daemon.wait_for("two 1001", |s| {
        s.closes
            .iter()
            .filter(|c| **c == ("hold".into(), Some(1001)))
            .count()
            == 2
    });
    let deadline = Instant::now() + Duration::from_secs(20);
    while w.gateway.open_sockets() != 1 {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    for sink in &main_sinks {
        assert_eq!(closes_in(&sink.snapshot()), 0);
    }

    // The other page reloads: its socket closes too.
    w.gateway.page_started("other");
    w.daemon.wait_for("three 1001", |s| {
        s.closes
            .iter()
            .filter(|c| **c == ("hold".into(), Some(1001)))
            .count()
            == 3
    });
    w.wait_until_closed();
    let e = w
        .send("other", other, Payload::Text("x".into()))
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Invalid);
}
