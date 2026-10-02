//! The commands through Tauri's IPC, on the mock runtime, with the app's real context: its ACL
//! (the capability), its command handler and its cleanup hooks.

#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{FakeDaemon, WORKSPACE_ID, WORKSPACE_NAME, register_local};
use pitcrew_desktop::app::{self, MAIN, WORKSPACES_EVENT};
use pitcrew_desktop::gateway::Gateway;
use pitcrew_desktop::navigate::{self, NAVIGATE_EVENT, NavigateTarget, Navigator};
use pitcrew_desktop::registry::{Registry, WorkspaceState};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::ipc::{CallbackFn, InvokeBody, InvokeResponseBody};
use tauri::test::{INVOKE_KEY, MockRuntime, get_ipc_response, mock_builder};
use tauri::webview::InvokeRequest;
use tauri::{Listener as _, WebviewWindow, WebviewWindowBuilder, WindowEvent};

const TOKEN: &str = "pcd_app-test-token-0123456789";

/// What went to the webview on channels: (channel id, body).
type Channels = Arc<Mutex<Vec<(u32, InvokeResponseBody)>>>;

struct World {
    app: tauri::App<MockRuntime>,
    channels: Channels,
    registry: Arc<Registry>,
    daemon: FakeDaemon,
    _tmp: tempfile::TempDir,
}

fn world() -> World {
    let tmp = tempfile::tempdir().unwrap();
    let daemon = FakeDaemon::start(&tmp.path().join("state"), TOKEN);
    let registry = Arc::new(Registry::in_memory());
    register_local(
        &registry,
        WORKSPACE_ID,
        WORKSPACE_NAME,
        Arc::new(daemon.connector()),
    );
    let channels: Channels = Arc::default();
    let captured = Arc::clone(&channels);
    let builder = mock_builder().channel_interceptor(move |_webview, callback, _index, body| {
        captured.lock().unwrap().push((callback.0, body.clone()));
        true
    });
    let app = app::configure(builder)
        .manage(Gateway::new(Arc::clone(&registry)))
        .manage(Navigator::default())
        .build(pitcrew_desktop::context())
        .unwrap();
    let handle = app.handle().clone();
    registry.on_change(move |list| app::emit_workspaces(&handle, list));
    World {
        app,
        channels,
        registry,
        daemon,
        _tmp: tmp,
    }
}

impl World {
    fn window(&self, label: &str) -> WebviewWindow<MockRuntime> {
        WebviewWindowBuilder::new(&self.app, label, Default::default())
            .build()
            .unwrap()
    }

    /// The messages on channel `id`, as JSON (binary as `{ "binary": [...] }`).
    fn channel(&self, id: u32) -> Vec<Value> {
        self.channels
            .lock()
            .unwrap()
            .iter()
            .filter(|(c, _)| *c == id)
            .map(|(_, body)| match body {
                InvokeResponseBody::Json(text) => serde_json::from_str(text).unwrap(),
                InvokeResponseBody::Raw(bytes) => json!({ "binary": bytes }),
            })
            .collect()
    }

    fn wait_channel(&self, id: u32, n: usize) -> Vec<Value> {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let messages = self.channel(id);
            if messages.len() >= n {
                return messages;
            }
            assert!(Instant::now() < deadline, "channel {id}: {messages:?}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

/// Invokes `cmd` from `window` as the webview would.
fn invoke(window: &WebviewWindow<MockRuntime>, cmd: &str, args: Value) -> Result<Value, Value> {
    get_ipc_response(
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
    )
    .map(|body| body.deserialize::<Value>().unwrap())
}

#[test]
fn the_commands_work_through_the_ipc() {
    let w = world();
    let main = w.window(MAIN);

    let workspaces = invoke(&main, "gateway_workspaces", json!({})).unwrap();
    assert_eq!(
        workspaces,
        json!([{ "id": WORKSPACE_ID, "name": WORKSPACE_NAME, "kind": "local", "state": "ready" }])
    );

    let ok = invoke(
        &main,
        "gateway_request",
        json!({ "req": { "workspace": WORKSPACE_ID, "method": "GET", "path": "/v1/tasks" } }),
    )
    .unwrap();
    assert_eq!(ok["status"], 200);
    assert_eq!(ok["contentType"], "application/json");
    assert!(ok["body"].as_str().unwrap().contains("Write the gateway"));

    let missing = invoke(
        &main,
        "gateway_request",
        json!({ "req": { "workspace": WORKSPACE_ID, "method": "GET", "path": "/v1/nothing" } }),
    )
    .unwrap();
    assert_eq!(missing["status"], 404);

    // Failures reject with a GatewayError.
    let e = invoke(
        &main,
        "gateway_request",
        json!({ "req": { "workspace": WORKSPACE_ID, "method": "GET", "path": "/v1/../x" } }),
    )
    .unwrap_err();
    assert_eq!(e["code"], "invalid");
    let e = invoke(
        &main,
        "gateway_request",
        json!({ "req": { "workspace": 1 } }),
    )
    .unwrap_err();
    assert_eq!(e["code"], "invalid");
    let e = invoke(&main, "gateway_request", json!({})).unwrap_err();
    assert_eq!(e["code"], "invalid");
    let e = invoke(
        &main,
        "gateway_request",
        json!({ "req": { "workspace": "nope", "method": "GET", "path": "/v1/tasks" } }),
    )
    .unwrap_err();
    assert_eq!(
        e,
        json!({ "code": "unknown_workspace", "message": "no workspace \"nope\"" })
    );

    // A stream socket: text frames as { type: "text" }, close last.
    let opened = invoke(
        &main,
        "gateway_socket_open",
        json!({ "workspace": WORKSPACE_ID, "path": "/v1/stream?script=order", "events": "__CHANNEL__:41" }),
    )
    .unwrap();
    assert!(opened["socket"].as_u64().unwrap() > 0);
    let messages = w.wait_channel(41, 4);
    assert_eq!(
        messages,
        vec![
            json!({ "type": "text", "data": "a" }),
            json!({ "binary": [1, 2, 3] }),
            json!({ "type": "text", "data": "b" }),
            json!({ "type": "close", "code": 1000, "reason": "bye" }),
        ]
    );

    // A terminal: text and binary in, echoed back; then closed.
    let opened = invoke(
        &main,
        "gateway_socket_open",
        json!({ "workspace": WORKSPACE_ID, "path": "/v1/sessions/01JS/terminal?cols=80&rows=24", "events": "__CHANNEL__:42" }),
    )
    .unwrap();
    let socket = opened["socket"].clone();
    assert_eq!(
        invoke(
            &main,
            "gateway_socket_send",
            json!({ "socket": socket, "text": "ls\r" })
        )
        .unwrap(),
        Value::Null
    );
    invoke(
        &main,
        "gateway_socket_send",
        json!({ "socket": socket, "binary": [27, 91, 65] }),
    )
    .unwrap();
    let e = invoke(&main, "gateway_socket_send", json!({ "socket": socket })).unwrap_err();
    assert_eq!(e["code"], "invalid");
    let e = invoke(
        &main,
        "gateway_socket_send",
        json!({ "socket": socket, "text": "a", "binary": [1] }),
    )
    .unwrap_err();
    assert_eq!(e["code"], "invalid");
    assert_eq!(
        w.wait_channel(42, 2),
        vec![
            json!({ "type": "text", "data": "ls\r" }),
            json!({ "binary": [27, 91, 65] })
        ]
    );
    invoke(
        &main,
        "gateway_socket_close",
        json!({ "socket": socket, "code": 4001, "reason": "bye" }),
    )
    .unwrap();
    assert_eq!(
        w.wait_channel(42, 3)[2],
        json!({ "type": "close", "code": 4001, "reason": "bye" })
    );
    // Idempotent.
    invoke(&main, "gateway_socket_close", json!({ "socket": socket })).unwrap();
    let e = invoke(
        &main,
        "gateway_socket_send",
        json!({ "socket": socket, "text": "x" }),
    )
    .unwrap_err();
    assert_eq!(e["code"], "invalid");

    // Upgrade failures.
    let e = invoke(
        &main,
        "gateway_socket_open",
        json!({ "workspace": WORKSPACE_ID, "path": "/v1/sessions/down/terminal", "events": "__CHANNEL__:43" }),
    )
    .unwrap_err();
    assert_eq!(e["code"], "unreachable");
    let e = invoke(
        &main,
        "gateway_socket_open",
        json!({ "workspace": WORKSPACE_ID, "path": "/v1/stream", "events": "not a channel" }),
    )
    .unwrap_err();
    assert_eq!(e["code"], "invalid");
}

#[test]
fn only_the_main_window_may_call_the_gateway_and_nothing_else() {
    let w = world();
    let main = w.window(MAIN);
    let other = w.window("other");
    // Another window has no capability at all.
    assert!(invoke(&other, "gateway_workspaces", json!({})).is_err());
    assert!(
        invoke(
            &other,
            "gateway_request",
            json!({ "req": { "workspace": WORKSPACE_ID, "method": "GET", "path": "/v1/tasks" } })
        )
        .is_err()
    );
    // The main window: the gateway, and listening to events; no other core command.
    assert!(invoke(&main, "gateway_workspaces", json!({})).is_ok());
    for (cmd, args) in [
        (
            "plugin:event|emit",
            json!({ "event": "gateway://workspaces", "payload": [] }),
        ),
        ("plugin:window|close", json!({ "label": MAIN })),
        ("plugin:webview|create_webview_window", json!({})),
        ("plugin:path|resolve_directory", json!({ "directory": 1 })),
        ("plugin:app|version", json!({})),
    ] {
        let denied = invoke(&main, cmd, args);
        assert!(denied.is_err(), "{cmd} was allowed: {denied:?}");
    }
    assert!(w.daemon.seen.lock().unwrap().authorizations.len() <= 1);
}

/// `gateway_local_host`: this computer's host name, cleaned, for the main window only.
#[test]
fn the_local_host_name_is_for_the_main_window_only() {
    let w = world();
    let main = w.window(MAIN);
    let other = w.window("other");
    let answer = invoke(&main, "gateway_local_host", json!({})).unwrap();
    let expected =
        pitcrew_desktop::host::clean_host(rustix::system::uname().nodename().to_str().unwrap());
    assert_eq!(answer, json!({ "name": expected }));
    let name = answer["name"].as_str().unwrap();
    assert!(!name.is_empty() && name.chars().count() <= 60, "{name}");
    // Another window: Tauri's ACL refuses it before it runs.
    let refused = invoke(&other, "gateway_local_host", json!({})).unwrap_err();
    let message = refused.as_str().unwrap_or_else(|| {
        panic!("gateway_local_host from another window was not refused by the ACL: {refused}")
    });
    assert!(
        message.contains("gateway_local_host") && message.contains("not allowed"),
        "{message}"
    );
}

#[test]
fn the_workspace_list_is_emitted_when_it_changes() {
    let w = world();
    let events: Arc<Mutex<Vec<Value>>> = Arc::default();
    let seen = Arc::clone(&events);
    w.app.listen_any(WORKSPACES_EVENT, move |event| {
        seen.lock()
            .unwrap()
            .push(serde_json::from_str(event.payload()).unwrap());
    });
    w.registry.set_state(
        WORKSPACE_ID,
        WorkspaceState::Unreachable,
        Some("pitcrewd stopped".into()),
    );
    w.registry
        .set_state(WORKSPACE_ID, WorkspaceState::Ready, None);
    let deadline = Instant::now() + Duration::from_secs(10);
    while events.lock().unwrap().len() < 2 {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    let events = events.lock().unwrap();
    assert_eq!(
        events[0],
        json!([{ "id": WORKSPACE_ID, "name": WORKSPACE_NAME, "kind": "local", "state": "unreachable", "detail": "pitcrewd stopped" }])
    );
    assert_eq!(events[1][0]["state"], "ready");
}

#[test]
fn deep_links_navigate_the_main_window_once_its_page_listens() {
    const TASK: &str = "01JB000000000000000TASK001";
    let w = world();
    let events: Arc<Mutex<Vec<Value>>> = Arc::default();
    let seen = Arc::clone(&events);
    w.app.listen_any(NAVIGATE_EVENT, move |event| {
        seen.lock()
            .unwrap()
            .push(serde_json::from_str(event.payload()).unwrap());
    });
    let main = w.window(MAIN);
    let other = w.window("other");
    let handle = w.app.handle().clone();
    let navigator = tauri::Manager::state::<Navigator>(&w.app);
    let settle = || std::thread::sleep(Duration::from_millis(100));

    // A link that launched the app: held until the page asks for the workspaces.
    navigate::open_links(
        &handle,
        [format!("pitcrew://w/{WORKSPACE_ID}/task/tsk_{TASK}")],
    );
    settle();
    assert!(events.lock().unwrap().is_empty());
    assert_eq!(
        navigator.pending(),
        Some(NavigateTarget::task(WORKSPACE_ID, TASK))
    );
    // Another window cannot release it (it has no capability at all).
    assert!(invoke(&other, "gateway_workspaces", json!({})).is_err());
    assert!(navigator.pending().is_some());
    invoke(&main, "gateway_workspaces", json!({})).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while events.lock().unwrap().is_empty() {
        assert!(Instant::now() < deadline, "no navigation");
        std::thread::sleep(Duration::from_millis(10));
    }
    // The payload is the contract's NavigateTarget.
    assert_eq!(
        events.lock().unwrap()[0],
        json!({ "workspace": WORKSPACE_ID, "kind": "task", "id": TASK })
    );

    // Now the page listens: links go at once. Anything else is dropped.
    navigate::open_links(
        &handle,
        [
            "--some-flag".to_owned(),
            format!("pitcrew://w/{WORKSPACE_ID}/inbox?answer=0"),
            format!("pitcrew://w/{WORKSPACE_ID}/task/{TASK}/answer"),
            "pitcrew://w/../etc/passwd".to_owned(),
            format!("pitcrew://w/{WORKSPACE_ID}/inbox"),
        ],
    );
    navigate::open_links(&handle, ["https://example.com/".to_owned()]);
    navigate::open_links(&handle, [format!("pitcrew://w/{WORKSPACE_ID}/inbox/")]);
    settle();
    assert_eq!(
        *events.lock().unwrap(),
        [
            json!({ "workspace": WORKSPACE_ID, "kind": "task", "id": TASK }),
            json!({ "workspace": WORKSPACE_ID, "kind": "inbox" }),
        ]
    );

    // A reload (what `on_page_load` does when the main page starts loading): held again until
    // the new page asks.
    navigator.page_started();
    navigator.navigate(&handle, NavigateTarget::inbox(WORKSPACE_ID));
    settle();
    assert_eq!(events.lock().unwrap().len(), 2);
    invoke(&main, "gateway_workspaces", json!({})).unwrap();
    settle();
    assert_eq!(events.lock().unwrap().len(), 3);
    assert_eq!(navigator.pending(), None);
}

#[test]
fn closing_the_window_closes_its_sockets() {
    let w = world();
    let main = w.window(MAIN);
    for channel in [51, 52] {
        invoke(
            &main,
            "gateway_socket_open",
            json!({ "workspace": WORKSPACE_ID, "path": "/v1/stream?script=hold", "events": format!("__CHANNEL__:{channel}") }),
        )
        .unwrap();
    }
    let gateway = tauri::Manager::state::<Gateway>(&w.app);
    assert_eq!(gateway.open_sockets(), 2);

    // What Tauri calls when the window is destroyed.
    app::on_window_event(&main.as_ref().window(), &WindowEvent::Destroyed);
    w.daemon.wait_for("both sockets closed with 1001", |s| {
        s.closes
            .iter()
            .filter(|c| **c == ("hold".into(), Some(1001)))
            .count()
            == 2
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    while gateway.open_sockets() > 0 {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    // The gone page is sent nothing more.
    assert!(w.channel(51).is_empty() && w.channel(52).is_empty());
}
