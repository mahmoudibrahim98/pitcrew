//! No token reaches the webview: every command, socket message, event, error and log line (at
//! trace level, Tauri's and tungstenite's included) is searched for a known token, against a fake
//! daemon that even puts the token in its response headers, and fake `pitcrewd`s that print it
//! on stderr.
//!
//! Its own test binary, because it installs the process-wide log subscriber.

#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{FakeDaemon, WORKSPACE_ID, WORKSPACE_NAME};
use pitcrew_desktop::app::{self, MAIN, WORKSPACES_EVENT};
use pitcrew_desktop::daemon::endpoint::Endpoint;
use pitcrew_desktop::daemon::supervisor::{DaemonState, Options, Supervisor};
use pitcrew_desktop::daemon::{LocalConnector, follow};
use pitcrew_desktop::gateway::Gateway;
use pitcrew_desktop::logging;
use pitcrew_desktop::registry::{
    Connection, Registry, WorkspaceKind, WorkspaceRecord, WorkspaceState,
};
use serde_json::{Value, json};
use std::io::Write;
use std::os::unix::fs::PermissionsExt as _;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::ipc::{CallbackFn, InvokeBody, InvokeResponseBody};
use tauri::test::{INVOKE_KEY, MockRuntime, get_ipc_response, mock_builder};
use tauri::webview::InvokeRequest;
use tauri::{Listener as _, WebviewWindow, WebviewWindowBuilder};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::MakeWriter;

const TOKEN: &str = "pcd_KNOWN-token-for-the-leak-test-7f3a9c";
const DOWN: &str = "01JD0000000000000000000000";

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for Capture {
    type Writer = Capture;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

fn invoke(window: &WebviewWindow<MockRuntime>, cmd: &str, args: Value) -> String {
    let result = get_ipc_response(
        window,
        InvokeRequest {
            cmd: cmd.into(),
            callback: CallbackFn(0),
            error: CallbackFn(1),
            url: "tauri://localhost".parse().unwrap(),
            body: InvokeBody::Json(args),
            headers: Default::default(),
            invoke_key: INVOKE_KEY.to_string(),
        },
    );
    match result {
        Ok(body) => format!("ok {}", body.deserialize::<Value>().unwrap()),
        Err(e) => format!("err {e}"),
    }
}

#[test]
fn no_token_reaches_the_webview_or_the_logs() {
    let logs = Capture::default();
    tracing::subscriber::set_global_default(logging::subscriber(
        EnvFilter::new("trace"),
        logs.clone(),
    ))
    .unwrap();
    logging::bridge_log();

    let tmp = tempfile::tempdir().unwrap();
    let daemon = FakeDaemon::start(&tmp.path().join("state"), TOKEN);
    let registry = Arc::new(Registry::in_memory());
    let local = WorkspaceRecord {
        id: WORKSPACE_ID.into(),
        name: WORKSPACE_NAME.into(),
        kind: WorkspaceKind::Local,
        connection: Connection::Local,
    };
    registry
        .insert(
            local,
            Some(Arc::new(daemon.connector())),
            WorkspaceState::Ready,
        )
        .unwrap();
    // A workspace whose token file is there but whose daemon is not.
    let down_dir = tmp.path().join("down");
    std::fs::create_dir_all(down_dir.join("run")).unwrap();
    std::fs::copy(daemon.token_file(), down_dir.join("device.token")).unwrap();
    registry
        .insert(
            WorkspaceRecord {
                id: DOWN.into(),
                name: "Down".into(),
                kind: WorkspaceKind::Remote,
                connection: Connection::Local,
            },
            Some(Arc::new(LocalConnector::with_token_path(
                Endpoint::Unix {
                    dir: down_dir.join("run"),
                },
                down_dir.join("device.token"),
            ))),
            WorkspaceState::Ready,
        )
        .unwrap();

    let channels: Arc<Mutex<Vec<InvokeResponseBody>>> = Arc::default();
    let captured = Arc::clone(&channels);
    let builder = mock_builder().channel_interceptor(move |_w, _callback, _i, body| {
        captured.lock().unwrap().push(body.clone());
        true
    });
    let app = app::configure(builder)
        .manage(Gateway::new(Arc::clone(&registry)))
        .build(pitcrew_desktop::context())
        .unwrap();
    let handle = app.handle().clone();
    registry.on_change(move |list| app::emit_workspaces(&handle, list));
    let events: Arc<Mutex<Vec<String>>> = Arc::default();
    let seen = Arc::clone(&events);
    app.listen_any(WORKSPACES_EVENT, move |e| {
        seen.lock().unwrap().push(e.payload().to_owned());
    });
    let main = WebviewWindowBuilder::new(&app, MAIN, Default::default())
        .build()
        .unwrap();

    let mut results = Vec::new();
    let mut call = |cmd: &str, args: Value| results.push(invoke(&main, cmd, args));
    let request = |method: &str, path: &str, body: Option<&str>| json!({ "req": { "workspace": WORKSPACE_ID, "method": method, "path": path, "body": body } });

    // Every command, with successes and every kind of failure.
    call("gateway_workspaces", json!({}));
    call(
        "gateway_request",
        request("GET", "/v1/tasks?project=secret-query-value", None),
    );
    call("gateway_request", request("GET", "/v1/workspace", None));
    call("gateway_request", request("GET", "/v1/nothing", None));
    call(
        "gateway_request",
        request("POST", "/v1/echo", Some(r#"{"title":"x"}"#)),
    );
    call("gateway_request", request("GET", "/v1/../x", None));
    call("gateway_request", request("HEAD", "/v1/tasks", None));
    call(
        "gateway_request",
        request("POST", "/v1/echo", Some(&"x".repeat(1024 * 1024 + 1))),
    );
    call(
        "gateway_request",
        request("GET", "/v1/big?bytes=33554433", None),
    );
    call(
        "gateway_request",
        json!({ "req": { "workspace": "nope", "method": "GET", "path": "/v1/tasks" } }),
    );
    call(
        "gateway_request",
        json!({ "req": { "workspace": DOWN, "method": "GET", "path": "/v1/tasks" } }),
    );
    call("gateway_request", json!({ "req": 42 }));
    let open = |path: &str, channel: u32| json!({ "workspace": WORKSPACE_ID, "path": path, "events": format!("__CHANNEL__:{channel}") });
    call("gateway_socket_open", open("/v1/stream?script=order", 1));
    call("gateway_socket_open", open("/v1/stream?script=drop", 2));
    call("gateway_socket_open", open("/v1/stream?script=ping", 3));
    call("gateway_socket_open", open("/v1/stream?script=big", 4));
    call("gateway_socket_open", open("/v1/sessions/down/terminal", 5));
    call("gateway_socket_open", open("/v1/sessions/nope/terminal", 6));
    call("gateway_socket_open", open("/v1/tasks", 7));
    call(
        "gateway_socket_open",
        json!({ "workspace": DOWN, "path": "/v1/stream", "events": "__CHANNEL__:8" }),
    );
    let terminal = invoke(
        &main,
        "gateway_socket_open",
        open("/v1/sessions/01JS/terminal", 9),
    );
    let socket: Value = serde_json::from_str(terminal.trim_start_matches("ok ")).unwrap();
    let socket = socket["socket"].clone();
    results.push(terminal);
    for args in [
        json!({ "socket": socket, "text": "echo hi\r" }),
        json!({ "socket": socket, "binary": [27, 91, 65] }),
        json!({ "socket": socket, "binary": vec![0u8; 1024 * 1024 + 1] }),
        json!({ "socket": socket, "text": "after the close" }),
        json!({ "socket": 4_000_000, "text": "unknown socket" }),
    ] {
        results.push(invoke(&main, "gateway_socket_send", args));
    }
    for args in [
        json!({ "socket": socket, "code": 1006 }),
        json!({ "socket": socket }),
    ] {
        results.push(invoke(&main, "gateway_socket_close", args));
    }
    let stream = invoke(
        &main,
        "gateway_socket_open",
        open("/v1/stream?script=hold", 10),
    );
    let stream_socket: Value = serde_json::from_str(stream.trim_start_matches("ok ")).unwrap();
    results.push(stream);
    results.push(invoke(
        &main,
        "gateway_socket_send",
        json!({ "socket": stream_socket["socket"], "text": "x".repeat(5000) }),
    ));

    // State changes go out as events.
    registry.set_state(
        WORKSPACE_ID,
        WorkspaceState::Unreachable,
        Some("pitcrewd stopped".into()),
    );
    registry.set_state(WORKSPACE_ID, WorkspaceState::Ready, None);
    registry.set_state(DOWN, WorkspaceState::NeedsPairing, Some("pair it".into()));
    results.push(invoke(
        &main,
        "gateway_request",
        json!({ "req": { "workspace": DOWN, "method": "GET", "path": "/v1/tasks" } }),
    ));
    results.push(invoke(&main, "gateway_workspaces", json!({})));

    // The local daemon's supervisor, with fake `pitcrewd`s that print the token on stderr (and in
    // the ready line): what it puts in the workspace's detail (the UI sees it, in the list and the
    // event) and what it logs must not hold it.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let says = format!("bad token {TOKEN}; Authorization: Bearer {TOKEN}; pitcrew.bearer.{TOKEN}");
    let mut details = Vec::new();
    for (name, serve, show_path) in [
        (
            "dies-before-ready",
            format!("echo '{says}' >&2; exit 1"),
            "echo \"$DIR/device.token\"".to_owned(),
        ),
        (
            "crashes-after-ready",
            format!("echo 'pitcrewd listening on {TOKEN}'; echo '{says}' >&2; sleep 0.1; exit 3"),
            "echo \"$DIR/device.token\"".to_owned(),
        ),
        (
            "show-path-fails",
            "trap 'exit 0' TERM; echo 'pitcrewd listening on x'; while true; do sleep 0.05; done"
                .to_owned(),
            format!("echo '{says}' >&2; exit 2"),
        ),
    ] {
        let dir = tmp.path().join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let program = dir.join("pitcrewd");
        std::fs::write(
            &program,
            format!(
                "#!/bin/sh\nDIR='{}'\ncase \"$*\" in\n  *\"token show-path\"*) {show_path} ;;\n  *serve*) {serve} ;;\nesac\n",
                dir.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        let endpoint = Endpoint::Unix {
            dir: dir.join("state").join("run"),
        };
        let mut options = Options::new(Ok(program), Some(dir.join("state")), endpoint.clone());
        options.first_backoff = Duration::from_millis(50);
        options.max_failures = 2;
        let detail = rt.block_on(async {
            let supervisor = Supervisor::start(options, &tokio::runtime::Handle::current());
            let following = tokio::spawn(follow(
                supervisor.state(),
                Arc::new(LocalConnector::new(endpoint)),
                Arc::clone(&registry),
            ));
            let mut state = supervisor.state();
            let detail = tokio::time::timeout(Duration::from_secs(30), async {
                loop {
                    if let DaemonState::Unreachable { detail } = state.borrow_and_update().clone() {
                        return detail;
                    }
                    state.changed().await.unwrap();
                }
            })
            .await
            .unwrap();
            supervisor.shutdown().await;
            following.abort();
            detail
        });
        details.push(detail);
    }
    assert!(details[0].contains("exit status: 1"), "{details:?}");
    assert!(details[1].contains("exit status: 3"), "{details:?}");
    assert!(details[2].contains("exit status: 2"), "{details:?}");
    results.extend(details.iter().cloned());
    results.push(invoke(&main, "gateway_workspaces", json!({})));

    // A record from an unsilenced target goes through the log bridge, so the absence of the
    // silenced crates' lines below is the silencing, not a broken bridge.
    log::trace!(target: "pitcrew_canary", "canary {}", 42);

    // Let the sockets finish and the last log lines land.
    let gateway = tauri::Manager::state::<Gateway>(&app);
    let deadline = Instant::now() + Duration::from_secs(20);
    while gateway.open_sockets() > 0 {
        assert!(Instant::now() < deadline, "sockets still open");
        std::thread::sleep(Duration::from_millis(20));
    }
    std::thread::sleep(Duration::from_millis(200));

    // The token was in play: the daemon received it.
    {
        let seen = daemon.seen.lock().unwrap();
        assert!(seen.authorizations.iter().any(|a| a.contains(TOKEN)));
        assert!(seen.subprotocols.iter().any(|p| p.contains(TOKEN)));
    }

    let channels: Vec<String> = channels
        .lock()
        .unwrap()
        .iter()
        .map(|body| match body {
            InvokeResponseBody::Json(text) => text.clone(),
            InvokeResponseBody::Raw(bytes) => String::from_utf8_lossy(bytes).into_owned(),
        })
        .collect();
    let events = events.lock().unwrap().clone();
    let logs = String::from_utf8_lossy(&logs.0.lock().unwrap()).into_owned();

    assert!(results.len() >= 30, "{results:#?}");
    assert!(
        results.iter().any(|r| r.starts_with("err ")),
        "{results:#?}"
    );
    assert!(channels.len() >= 8, "{channels:#?}");
    assert!(events.len() >= 3, "{events:#?}");
    assert!(
        logs.contains(r#"route="/v1/tasks""#),
        "the gateway's log lines are captured: {}",
        logs.chars().take(3000).collect::<String>()
    );
    assert!(logs.contains("canary 42"), "the log bridge works");
    assert!(
        logs.contains("pitcrewd stopped before it was ready"),
        "the supervisor's lines are captured"
    );
    assert!(
        logs.contains("pcd_…"),
        "the daemon's stderr was logged, redacted"
    );

    let secret_core = &TOKEN["pcd_".len()..];
    for (what, texts) in [
        ("command result or error", &results),
        ("channel message", &channels),
        ("event", &events),
    ] {
        for text in texts {
            assert!(
                !text.contains(secret_core),
                "a {what} holds the token: {text}"
            );
            assert!(
                !text.contains("pitcrew.bearer."),
                "a {what} holds the subprotocol: {text}"
            );
        }
    }
    for line in logs.lines() {
        assert!(
            !line.contains(secret_core),
            "a log line holds the token: {line}"
        );
        // The daemon's own lines are logged redacted: `pitcrew.bearer.…`, never a value.
        assert!(
            line.match_indices("pitcrew.bearer.")
                .all(|(i, m)| line[i + m.len()..].starts_with('…')),
            "a log line holds the subprotocol: {line}"
        );
        assert!(
            !line.contains("secret-query-value"),
            "a log line holds a query: {line}"
        );
    }
}
