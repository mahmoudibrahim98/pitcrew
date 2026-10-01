//! `pitcrewd serve` end to end: the real binary on a temp state directory and a free port.

#![allow(clippy::unwrap_used)]

mod common;

#[cfg(unix)]
use common::Frame;
use common::{Daemon, Ws, id, request, run};
use serde_json::{Value, json};
use std::ffi::OsStr;
use std::path::Path;
use std::time::Duration;

const WAIT: Duration = Duration::from_secs(10);

fn state_dir() -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    // A fresh subdirectory, which the daemon creates private.
    let state = tmp.path().join("state");
    (tmp, state)
}

fn keys(tasks: &Value) -> Vec<String> {
    tasks
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["key"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn version_names_the_protocol_range() {
    let out = run(&[OsStr::new("--version")]);
    assert!(out.status.success());
    let line = String::from_utf8(out.stdout).unwrap();
    assert!(
        line.starts_with(&format!("pitcrewd {} ", env!("CARGO_PKG_VERSION"))),
        "{line}"
    );
    // Launchers find the bare version as a word of the first line.
    assert_eq!(
        line.lines().next().unwrap().split_whitespace().nth(1),
        Some(env!("CARGO_PKG_VERSION"))
    );
    assert!(
        line.contains(&format!(
            "protocol {}, oldest accepted {}",
            pitcrew_protocol::PROTOCOL_VERSION,
            pitcrew_protocol::PROTOCOL_MIN
        )),
        "{line}"
    );
}

/// `--version` needs no state directory and creates nothing.
#[cfg(unix)]
#[test]
fn version_touches_no_state() {
    let home = tempfile::tempdir().unwrap();
    let out = std::process::Command::new(common::PITCREWD)
        .arg("--version")
        .env("HOME", home.path())
        .env_remove("XDG_DATA_HOME")
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(std::fs::read_dir(home.path()).unwrap().count(), 0);
}

/// `--listen unix:<path>` binds exactly that socket, in a private directory, and every stop
/// signal (SIGTERM, and SIGHUP from a closed tmux pane or SSH session) removes it.
#[cfg(unix)]
#[test]
fn unix_listen_binds_exactly_that_socket_and_a_stop_removes_it() {
    use std::os::unix::fs::{FileTypeExt as _, PermissionsExt as _};
    for signal in ["TERM", "HUP"] {
        let (tmp, state) = state_dir();
        let socket = tmp.path().join("home/.pitcrew/run/pitcrewd.sock");
        let listen = format!("unix:{}", socket.display());
        let mut daemon = Daemon::start_on(&state, &listen, &["--demo"]);
        assert_eq!(Path::new(&daemon.at), socket);
        let kind = std::fs::symlink_metadata(&socket).unwrap().file_type();
        assert!(kind.is_socket());
        let dir = std::fs::metadata(socket.parent().unwrap()).unwrap();
        assert_eq!(dir.permissions().mode() & 0o777, 0o700);
        assert_eq!(unix_get(&socket, "/v1/host/info"), 200);

        let status = daemon.signal(signal);
        assert!(
            status.success(),
            "SIG{signal}: {status}\n{}",
            daemon.stderr()
        );
        assert!(!socket.exists(), "SIG{signal} left the socket behind");
        let logs = daemon.stderr();
        assert!(logs.contains("stopped"), "{logs}");
        // The back office stopped before the store closed.
        let office = logs.find("the back office stopped").unwrap_or(usize::MAX);
        let store = logs.find("store closed").unwrap_or(0);
        assert!(office < store, "SIG{signal}:\n{logs}");
    }

    // Only `<dir>/pitcrewd.sock`, the name pitcrew-api binds.
    let (tmp, state) = state_dir();
    let other = format!("unix:{}", tmp.path().join("other.sock").display());
    let refused = Daemon::try_start_on(&state, &other, &[]).unwrap_err();
    assert!(
        refused.stderr.contains("pitcrewd.sock"),
        "{}",
        refused.stderr
    );
}

/// A GET over a unix socket; returns the status.
#[cfg(unix)]
fn unix_get(socket: &Path, path: &str) -> u16 {
    use std::io::{Read as _, Write as _};
    let mut stream = std::os::unix::net::UnixStream::connect(socket).unwrap();
    stream.set_read_timeout(Some(WAIT)).unwrap();
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut raw = String::new();
    stream.read_to_string(&mut raw).unwrap();
    raw.split(' ').nth(1).unwrap().parse().unwrap()
}

#[test]
fn token_show_path_prints_the_path_never_the_token() {
    let (_tmp, state) = state_dir();
    let show = || {
        run(&[
            OsStr::new("--state-dir"),
            state.as_os_str(),
            OsStr::new("token"),
            OsStr::new("show-path"),
        ])
    };
    let before = show();
    assert!(!before.status.success(), "no token before the first start");
    assert!(before.stdout.is_empty());

    let daemon = Daemon::start(&state, &["--demo"]);
    let after = show();
    assert!(after.status.success());
    let printed = String::from_utf8(after.stdout).unwrap();
    assert_eq!(Path::new(printed.trim()), state.join("device.token"));
    let token = daemon.device_token();
    assert!(token.starts_with("pcd_"));
    assert!(!printed.contains(&token[4..]));
}

#[test]
fn demo_serves_the_work_model_with_real_tokens() {
    let (_tmp, state) = state_dir();
    let daemon = Daemon::start(&state, &["--demo"]);
    eprintln!("ready in {:?}", daemon.ready_in);
    let device = daemon.device_token();
    let agent = daemon.agent_token();
    assert!(agent.starts_with("pca_"));

    // Host info needs no token.
    let info = daemon.get("/v1/host/info", None);
    assert_eq!(info.status, 200);
    let info = info.json();
    assert_eq!(info["name"], "pitcrewd");
    assert_eq!(info["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(info["protocol"], pitcrew_protocol::PROTOCOL_VERSION);
    assert_eq!(info["protocol_min"], pitcrew_protocol::PROTOCOL_MIN);
    assert_eq!(info["roles"], json!(["hub", "runner"]));
    // The demo watches no home.
    assert_eq!(info["capabilities"], json!([]));

    // No token, a malformed one, and the mock hub's dev token are all refused.
    for token in [None, Some("nope"), Some("dev-device-token")] {
        let reply = daemon.get("/v1/tasks", token);
        assert_eq!(reply.status, 401, "{token:?}");
        assert_eq!(reply.code(), "unauthorized");
        assert_eq!(reply.header("www-authenticate"), Some("Bearer"));
    }

    // The device token is the demo's person and sees the demo's tasks.
    let tasks = daemon.get("/v1/tasks", Some(&device));
    assert_eq!(tasks.status, 200);
    assert_eq!(keys(&tasks.json()).len(), 10);
    let me = daemon.get("/v1/me", Some(&device)).json();
    assert_eq!(me["handle"], "@sam");
    let todo = daemon.get("/v1/tasks?status=todo&status=backlog", Some(&device));
    assert_eq!(keys(&todo.json()), ["PAP-2", "PAP-5", "PAP-6", "TL-2"]);
    let task = daemon.get("/v1/tasks/PAP-4", Some(&device)).json();
    assert_eq!(task["key"], "PAP-4");
    for path in [
        "/v1/machines",
        "/v1/members",
        "/v1/personas",
        "/v1/teams",
        "/v1/projects",
        "/v1/workstreams",
        "/v1/briefs",
        "/v1/asks",
    ] {
        let reply = daemon.get(path, Some(&device));
        assert_eq!(reply.status, 200, "{path}");
        assert!(!reply.json().as_array().unwrap().is_empty(), "{path}");
    }

    // The demo agent token is @writer: agent routes yes, device routes no.
    let me = daemon.get("/v1/me", Some(&agent)).json();
    assert_eq!(me["handle"], "@writer");
    assert_eq!(me["id"], id::WRITER);
    let machines = daemon.get("/v1/machines", Some(&agent));
    assert_eq!(machines.status, 403);
    assert_eq!(machines.code(), "forbidden");
    // After the back office's pass over the seed, nothing else appends before the move.
    let before_move = daemon.settle(&device);
    let moved = daemon.post(
        "/v1/tasks/PAP-2/move",
        Some(&agent),
        &json!({ "to": "in_progress" }),
    );
    assert_eq!(moved.status, 200, "{}", moved.body);

    // Activity pages the log, newest last; the agent's move is the event after what was there.
    // (The back office may append after it: the demo's asks are days old by the wall clock.)
    let page = daemon
        .get(
            &format!("/v1/events?limit=1&before={}", before_move + 2),
            Some(&device),
        )
        .json();
    assert_eq!(page["to_rev"], before_move + 1);
    let event = &page["events"][0];
    assert_eq!(event["author"], id::WRITER);
    assert_eq!(event["on_behalf_of"], id::SAM);
    assert_eq!(event["body"]["type"], "task_moved");

    // Hooks: accepted from agents and answered at once.
    let hook = daemon.post(
        "/v1/hooks/claude/Stop",
        Some(&agent),
        &json!({ "session_id": "abc", "hook_event_name": "Stop" }),
    );
    assert_eq!(hook.status, 202);
    let bad = daemon.post("/v1/hooks/emacs/Stop", Some(&agent), &json!({}));
    assert_eq!(bad.status, 400);

    // Terminals: a session of this machine has none (no runtime yet), one on another machine is
    // out of reach, and an unknown one is not found.
    let local = daemon.get(
        &format!("/v1/sessions/{}/terminal", id::SES1),
        Some(&device),
    );
    assert_eq!(local.status, 404, "{}", local.body);
    assert_eq!(local.code(), "not_found");
    let remote = daemon.get(
        &format!("/v1/sessions/{}/terminal", id::SES2),
        Some(&device),
    );
    assert_eq!(remote.status, 503, "{}", remote.body);
    assert_eq!(remote.code(), "unavailable");
    let unknown = daemon.get(
        &format!("/v1/sessions/{}/terminal", id::SES_UNKNOWN),
        Some(&device),
    );
    assert_eq!(unknown.status, 404);
    assert_eq!(unknown.code(), "not_found");
    let agent_terminal = daemon.get(&format!("/v1/sessions/{}/terminal", id::SES1), Some(&agent));
    assert_eq!(agent_terminal.status, 403);

    // Transcripts: the demo watches no home, so this machine's sessions have an empty one.
    let transcript = daemon.get(
        &format!("/v1/sessions/{}/transcript", id::SES1),
        Some(&device),
    );
    assert_eq!(transcript.status, 200, "{}", transcript.body);
    assert_eq!(
        transcript.json(),
        json!({ "items": [], "from": 0, "to": 0, "at_start": true })
    );
    for (session, status) in [(id::SES2, 503), (id::SES_UNKNOWN, 404)] {
        let reply = daemon.get(&format!("/v1/sessions/{session}/transcript"), Some(&device));
        assert_eq!(reply.status, status, "{session}: {}", reply.body);
    }
    let agent_transcript = daemon.get(
        &format!("/v1/sessions/{}/transcript", id::SES1),
        Some(&agent),
    );
    assert_eq!(agent_transcript.status, 403);

    // The workspace, named, and the revision the work model reflects.
    let workspace = daemon.get("/v1/workspace", Some(&device));
    assert_eq!(workspace.status, 200, "{}", workspace.body);
    let workspace = workspace.json();
    assert_eq!(workspace["workspace"]["name"], "Demo Lab");
    assert!(workspace["rev"].as_u64().unwrap() > 0);

    // Sessions, and one of them.
    let sessions = daemon.get("/v1/sessions", Some(&device));
    assert_eq!(sessions.status, 200, "{}", sessions.body);
    assert!(!sessions.json().as_array().unwrap().is_empty());
    let session = daemon.get(&format!("/v1/sessions/{}", id::SES1), Some(&device));
    assert_eq!(session.status, 200, "{}", session.body);
    assert_eq!(session.json()["id"], id::SES1);

    // The runner does not start dispatches yet, so a dispatch is unavailable and records nothing:
    // no dispatch, no session, and no assignment of the unassigned PAP-5. (Only the back office
    // may still be appending what it makes of the move above.)
    let before = daemon.latest_rev(&device);
    let dispatch = daemon.post(
        "/v1/tasks/PAP-5/dispatch",
        Some(&device),
        &json!({ "agent": id::WRITER }),
    );
    assert_eq!(dispatch.status, 503, "{}", dispatch.body);
    assert_eq!(dispatch.code(), "unavailable");
    let page = daemon.get("/v1/events?limit=500", Some(&device)).json();
    let from = page["from_rev"].as_u64().unwrap();
    let appended: Vec<&Value> = page["events"]
        .as_array()
        .unwrap()
        .iter()
        .skip(usize::try_from(before + 1 - from).unwrap())
        .filter(|e| e["author"] != id::OFFICE)
        .collect();
    assert!(
        appended.is_empty(),
        "a dispatch appended events: {appended:?}"
    );
    let pap5 = daemon.get("/v1/tasks/PAP-5", Some(&device)).json();
    assert!(pap5["assignee"].is_null(), "{pap5}");
    let by_agent = daemon.post(
        "/v1/tasks/PAP-5/dispatch",
        Some(&agent),
        &json!({ "agent": id::WRITER }),
    );
    assert_eq!(by_agent.status, 403);

    // Unknown routes are JSON 404s.
    let nowhere = daemon.get("/v1/nowhere", Some(&device));
    assert_eq!(nowhere.status, 404);
    assert_eq!(nowhere.code(), "not_found");

    // Development TCP: CORS for local origins only, and the DNS-rebinding guard.
    let preflight = |origin: &str| {
        request(
            daemon.port,
            "OPTIONS",
            "/v1/tasks",
            None,
            None,
            &[
                ("Origin", origin),
                ("Access-Control-Request-Method", "GET"),
                ("Access-Control-Request-Headers", "authorization"),
            ],
        )
    };
    let ok = preflight("http://127.0.0.1:5199");
    assert_eq!(ok.status, 204);
    assert_eq!(
        ok.header("access-control-allow-origin"),
        Some("http://127.0.0.1:5199")
    );
    assert!(
        ok.header("access-control-allow-headers")
            .unwrap()
            .to_ascii_lowercase()
            .contains("authorization")
    );
    let refused = preflight("https://evil.example");
    assert_eq!(refused.status, 403);
    assert_eq!(refused.header("access-control-allow-origin"), None);
    assert_eq!(raw_host_request(daemon.port, "rebind.example"), 403);
    assert_eq!(raw_host_request(daemon.port, "localhost"), 200);

    // Nothing the daemon logged, even at debug, holds a token.
    let logs = daemon.stderr();
    assert!(logs.contains("ready"), "{logs}");
    for token in [&device, &agent] {
        assert!(!logs.contains(&token[4..]), "a token leaked into the logs");
    }
}

/// A GET whose only `Host` header is `host`; returns the status.
fn raw_host_request(port: u16, host: &str) -> u16 {
    use std::io::{Read as _, Write as _};
    let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream.set_read_timeout(Some(WAIT)).unwrap();
    write!(
        stream,
        "GET /v1/host/info HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut raw = String::new();
    stream.read_to_string(&mut raw).unwrap();
    raw.split(' ').nth(1).unwrap().parse().unwrap()
}

#[test]
fn a_move_through_the_api_appears_on_the_stream() {
    let (_tmp, state) = state_dir();
    let daemon = Daemon::start(&state, &["--demo"]);
    let device = daemon.device_token();

    // Agents use HTTP; the stream is for device tokens.
    let refused = Ws::connect(daemon.port, "/v1/stream", &daemon.agent_token()).unwrap_err();
    assert_eq!(refused.status, 403);

    // After the back office's pass over the seed, nothing else appends before the move.
    let settled = daemon.settle(&device);
    let mut stream = Ws::connect(daemon.port, "/v1/stream", &device).unwrap();
    let hello = stream.next_json(WAIT);
    assert_eq!(hello["type"], "hello");
    let rev = hello["rev"].as_u64().unwrap();
    assert_eq!(rev, settled);
    let log = hello["log"].as_str().unwrap().to_owned();
    assert!(rev > 0);
    assert_eq!(log.len(), 26, "the log id is a ULID");

    let moved = daemon.post(
        "/v1/tasks/PAP-2/move",
        Some(&device),
        &json!({ "to": "in_progress" }),
    );
    assert_eq!(moved.status, 200, "{}", moved.body);

    // The move comes first. The back office may append after it (the demo's asks are days old by
    // the wall clock), in the same frame or later ones.
    let frame = stream.next_json(WAIT);
    assert_eq!(frame["type"], "events", "{frame}");
    assert_eq!(frame["from_rev"], rev + 1);
    let to_rev = frame["to_rev"].as_u64().unwrap();
    let events = frame["events"].as_array().unwrap();
    assert_eq!(events.len() as u64, to_rev - rev);
    let event = &events[0];
    assert_eq!(event["body"]["type"], "task_moved");
    assert_eq!(event["body"]["data"]["task"], id::PAP2);
    assert_eq!(event["body"]["data"]["to"], "in_progress");
    assert_eq!(event["author"], id::SAM);
    for later in &events[1..] {
        assert_eq!(later["author"], id::OFFICE, "{later}");
    }

    // A client that reconnects with what it had gets exactly what it missed: every revision
    // after `since` up to the hello's, in order, the move first.
    let mut again = Ws::connect(daemon.port, &format!("/v1/stream?since={rev}"), &device).unwrap();
    let hello = again.next_json(WAIT);
    let now = hello["rev"].as_u64().unwrap();
    assert!(now > rev);
    assert_eq!(hello["log"], log.as_str());
    let mut next = rev + 1;
    let mut first = None;
    while next <= now {
        let missed = again.next_json(WAIT);
        assert_eq!(missed["type"], "events", "{missed}");
        assert_eq!(missed["from_rev"], next);
        first.get_or_insert_with(|| missed["events"][0].clone());
        next = missed["to_rev"].as_u64().unwrap() + 1;
    }
    let first = first.unwrap();
    assert_eq!(first["body"]["data"]["task"], id::PAP2);
    assert_eq!(first["author"], id::SAM);
}

#[test]
fn a_second_daemon_on_the_same_state_dir_is_refused() {
    let (_tmp, state) = state_dir();
    let first = Daemon::start(&state, &["--demo"]);
    let second = Daemon::try_start(&state, &[]).unwrap_err();
    assert!(!second.status.success());
    assert!(
        second.stderr.contains("already running"),
        "{}",
        second.stderr
    );
    // The first one is unharmed.
    assert_eq!(first.get("/v1/host/info", None).status, 200);
}

#[cfg(unix)]
#[test]
fn sigterm_stops_cleanly_and_a_restart_keeps_everything() {
    let (_tmp, state) = state_dir();
    let mut daemon = Daemon::start(&state, &["--demo"]);
    let device = daemon.device_token();
    let reopened = daemon.post(
        "/v1/tasks/PAP-7/move",
        Some(&device),
        &json!({ "to": "todo" }),
    );
    assert_eq!(reopened.status, 200, "{}", reopened.body);
    // A stream is open while it stops.
    let mut stream = Ws::connect(daemon.port, "/v1/stream", &device).unwrap();
    assert_eq!(stream.next_json(WAIT)["type"], "hello");

    let status = daemon.terminate();
    assert!(status.success(), "{status}:\n{}", daemon.stderr());
    let logs = daemon.stderr();
    assert!(logs.contains("store closed"), "{logs}");
    // The back office stopped first, and saved where it got to.
    let office = logs.find("the back office stopped").unwrap_or(usize::MAX);
    assert!(office < logs.find("store closed").unwrap(), "{logs}");
    assert!(state.join("office.json").is_file());
    // The store closed cleanly: its WAL was checkpointed into hub.db and removed.
    assert!(state.join("hub.db").is_file());
    for leftover in ["hub.db-wal", "hub.db-shm"] {
        assert!(!state.join(leftover).exists(), "{leftover} is left");
    }
    // The stream ended.
    loop {
        match stream.next(WAIT).unwrap() {
            None | Some(Frame::Close(..)) => break,
            Some(_) => {}
        }
    }
    drop(daemon);

    // --demo refuses the store it seeded.
    let refused = Daemon::try_start(&state, &["--demo"]).unwrap_err();
    assert!(
        refused.stderr.contains("only an empty store"),
        "{}",
        refused.stderr
    );

    // Without it, everything is as it was: the same token, the same log, the move.
    let mut daemon = Daemon::start(&state, &[]);
    assert_eq!(daemon.device_token(), device);
    let task = daemon.get("/v1/tasks/PAP-7", Some(&device));
    assert_eq!(task.status, 200, "{}", task.body);
    assert_eq!(task.json()["status"], "todo");
    assert_eq!(task.json()["id"], id::PAP7);
    let workspace = daemon.get("/v1/workspace", Some(&device)).json();
    assert_eq!(workspace["workspace"]["name"], "Demo Lab");
    assert!(daemon.terminate().success());
}

/// `pitcrew-api`'s `Bound::serve` tells open WebSockets that the hub is shutting down.
#[cfg(unix)]
#[test]
fn sigterm_closes_streams_with_1001() {
    let (_tmp, state) = state_dir();
    let mut daemon = Daemon::start(&state, &["--demo"]);
    let mut stream = Ws::connect(daemon.port, "/v1/stream", &daemon.device_token()).unwrap();
    assert_eq!(stream.next_json(WAIT)["type"], "hello");
    let status = daemon.terminate();
    assert!(status.success(), "{status}");
    let close = loop {
        match stream.next(WAIT).unwrap() {
            Some(Frame::Close(code, _)) => break code,
            Some(Frame::Ping | Frame::Pong | Frame::Text(_)) => {}
            other => panic!("expected a Close frame, got {other:?}"),
        }
    };
    assert_eq!(close, Some(1001));
}
