//! The first run in the real daemon (api-v1.md, "The first run"): a fresh `pitcrewd serve`, set
//! up once by `POST /v1/setup` or `pitcrewd init`, then working as a hub that started with a
//! person does, without a restart: its name kept, the back office acting, the runner watching.
//! Every daemon watches temporary homes (`--homes`), never a real one. Also `pitcrewd connect`,
//! the stdio bridge for remote helpers.

#![allow(clippy::unwrap_used)]

mod common;

use common::{Daemon, Reply, request};
use pitcrew_protocol::events::{Event, EventBody};
use pitcrew_protocol::ids::{DispatchId, MemberId, TaskId, WorkspaceId};
use pitcrew_protocol::model::{Dispatch, DispatchOutcome};
use serde_json::{Value, json};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::Barrier;
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(30);

/// What the daemon logs once a setup has taken (not the warnings that say it has not yet).
const SET_UP: &str = "pitcrewd::setup: the workspace is set up";

/// The Claude fixture's own session id, which its lines carry.
const FIXTURE_ID: &str = "2b6f1a8e-4c1d-4f5e-9a37-0c8d1e2f3a4b";
/// The id the tests give it in their homes.
const NATIVE: &str = "7c2d4e6f-1a3b-4c5d-8e9f-0a1b2c3d4e5f";

fn state_dir() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    (tmp, state)
}

/// Temporary agent homes holding the Claude fixture's transcript, as session [`NATIVE`].
fn homes_with_a_transcript(tmp: &Path) -> PathBuf {
    let homes = tmp.join("homes");
    let dir = homes.join(".claude/projects/-home-lee-work-thesis");
    std::fs::create_dir_all(&dir).unwrap();
    let path = pitcrew_fixtures::data_dir().join("transcripts/claude/demo-session.jsonl");
    let text = std::fs::read_to_string(path)
        .unwrap()
        .replace(FIXTURE_ID, NATIVE);
    std::fs::write(dir.join(format!("{NATIVE}.jsonl")), text).unwrap();
    homes
}

/// A fresh daemon (no `--demo`) on `state`, watching `homes`.
fn fresh(state: &Path, homes: &Path) -> Daemon {
    Daemon::start(state, &["--homes", homes.to_str().unwrap()])
}

fn setup_body(workspace: &str, name: &str, handle: &str, machine: &str) -> Value {
    json!({
        "workspace_name": workspace,
        "person": { "name": name, "handle": handle },
        "machine_name": machine,
    })
}

/// Polls `f` until it gives something, at most [`WAIT`].
fn eventually<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + WAIT;
    loop {
        if let Some(found) = f() {
            return found;
        }
        assert!(Instant::now() < deadline, "never: {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn ok(reply: &Reply, status: u16) -> Value {
    assert_eq!(reply.status, status, "{}", reply.body);
    reply.json()
}

fn host_info(daemon: &Daemon) -> Value {
    ok(&daemon.get("/v1/host/info", None), 200)
}

/// `HEAD /v1/host/info`'s `Content-Length` (it has no body), and the length `GET`'s body has now.
fn host_info_head_and_get(daemon: &Daemon) -> (usize, usize) {
    let head = request(daemon.port, "HEAD", "/v1/host/info", None, None, &[]);
    assert_eq!(head.status, 200);
    assert!(head.body.is_empty(), "{}", head.body);
    assert_eq!(head.header("content-type"), Some("application/json"));
    let length = head.header("content-length").unwrap().parse().unwrap();
    let get = daemon.get("/v1/host/info", None);
    assert_eq!(get.status, 200);
    (length, get.body.len())
}

fn members_called(daemon: &Daemon, token: &str, handle: &str) -> Vec<Value> {
    ok(&daemon.get("/v1/members", Some(token)), 200)
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["handle"] == handle)
        .cloned()
        .collect()
}

/// Appends `bodies` to the store in `state` from this process, as the runner link will report a
/// dispatch (as `tests/office.rs` does). The daemon looks at them with its next append.
fn append_to_store(state: &Path, workspace: &str, author: &str, bodies: Vec<EventBody>) {
    let store =
        pitcrew_store::Store::open(state.join("hub.db"), pitcrew_store::StoreOptions::default())
            .unwrap();
    let workspace: WorkspaceId = workspace.parse().unwrap();
    let author: MemberId = author.parse().unwrap();
    let events: Vec<Event> = bodies
        .into_iter()
        .map(|body| Event::now(workspace, author, body))
        .collect();
    store.append(&events).unwrap();
}

/// The back office's first rule, on this workspace: a task in progress whose dispatch finishes
/// moves to review, as `@office`.
fn the_office_moves_a_finished_dispatch(
    daemon: &Daemon,
    token: &str,
    workspace: &str,
    person: &str,
    office: &str,
) {
    let project = ok(
        &daemon.post(
            "/v1/projects",
            Some(token),
            &json!({ "key": "LAB", "name": "Lab work" }),
        ),
        201,
    );
    let task = ok(
        &daemon.post(
            "/v1/tasks",
            Some(token),
            &json!({
                "project": project["id"],
                "title": "Run the ablation",
                "status": "in_progress",
            }),
        ),
        201,
    );
    let task_id: TaskId = task["id"].as_str().unwrap().parse().unwrap();
    let dispatch = DispatchId::new();
    append_to_store(
        &daemon.state,
        workspace,
        person,
        vec![
            EventBody::DispatchStarted {
                dispatch: Dispatch {
                    id: dispatch,
                    task: task_id,
                    agent: MemberId::new(),
                    session: None,
                    brief: "Run the ablation".into(),
                    started: 1_790_800_000_000,
                    ended: None,
                    outcome: None,
                    summary: None,
                },
            },
            EventBody::DispatchFinished {
                dispatch,
                outcome: DispatchOutcome::Succeeded,
                summary: Some("Done; ready for review.".into()),
            },
        ],
    );
    // An append through the API: the daemon looks at everything up to it.
    let path = format!("/v1/tasks/{}/comments", task["id"].as_str().unwrap());
    ok(
        &daemon.post(
            &path,
            Some(token),
            &json!({ "text": "Over to review.", "mentions": [] }),
        ),
        201,
    );
    let task_path = format!("/v1/tasks/{}", task["id"].as_str().unwrap());
    eventually("the task moves to review", || {
        (ok(&daemon.get(&task_path, Some(token)), 200)["status"] == "review").then_some(())
    });
    let moves: Vec<Value> = daemon
        .events_matching(&format!("&task={}", task["id"].as_str().unwrap()), token)
        .into_iter()
        .filter(|e| e["body"]["type"] == "task_moved")
        .collect();
    assert_eq!(moves.len(), 1, "{moves:?}");
    assert_eq!(moves[0]["author"], office);
    assert_eq!(moves[0]["on_behalf_of"], person);
    assert_eq!(moves[0]["body"]["data"]["to"], "review");
}

/// Before setup: `setup_needed`, no person, no runner, no office.
#[test]
fn a_fresh_hub_waits_for_its_setup() {
    let (tmp, state) = state_dir();
    let homes = homes_with_a_transcript(tmp.path());
    let daemon = fresh(&state, &homes);
    let device = daemon.device_token();

    let workspace = ok(&daemon.get("/v1/workspace", Some(&device)), 200);
    assert_eq!(workspace["setup_needed"], true);
    assert_eq!(workspace["workspace"]["name"], "Workspace");
    assert_eq!(daemon.get("/v1/me", Some(&device)).status, 404);
    let info = host_info(&daemon);
    assert_eq!(info["roles"], json!(["hub"]));
    assert_eq!(info["capabilities"], json!([]));
    assert!(members_called(&daemon, &device, "@office").is_empty());
    let logs = daemon.stderr();
    assert!(logs.contains("the back office is off"), "{logs}");
    assert!(!logs.contains("the back office acts as"), "{logs}");
    assert!(logs.contains("the runner is off"), "{logs}");
    // Nothing is watched, though a transcript waits in the home.
    assert_eq!(
        ok(&daemon.get("/v1/sessions", Some(&device)), 200),
        json!([])
    );
    assert!(!state.join("workspace.json").exists());
    assert!(!state.join("office.json").exists());

    // `@office` is the back office's, though it has no member yet: a setup asking for it is a
    // `409`, and changes nothing.
    let reserved = daemon.post(
        "/v1/setup",
        Some(&device),
        &setup_body("Lab", "Lee", "@office", "PC"),
    );
    assert_eq!(reserved.status, 409, "{}", reserved.body);
    assert_eq!(reserved.code(), "conflict");
    let workspace = ok(&daemon.get("/v1/workspace", Some(&device)), 200);
    assert_eq!(workspace["setup_needed"], true);
    assert!(!daemon.stderr().contains(SET_UP));
}

/// `POST /v1/setup`, then without a restart: the name (kept in `workspace.json`), `@office` acting
/// for the person, the runner watching the homes on the new machine; then a restart that keeps the
/// name and starts both at once.
#[test]
fn setup_starts_the_office_and_the_runner_and_a_restart_keeps_them() {
    let (tmp, state) = state_dir();
    let homes = homes_with_a_transcript(tmp.path());
    let mut daemon = fresh(&state, &homes);
    let device = daemon.device_token();
    // HEAD of host info answers as GET does, without the body.
    let (head, get) = host_info_head_and_get(&daemon);
    assert_eq!(head, get);
    let fresh_length = get;

    // The names are trimmed, and stored trimmed.
    let done = ok(
        &daemon.post(
            "/v1/setup",
            Some(&device),
            &setup_body(" Thesis lab ", "Lee Park\t", "@lee", "\u{3000}Lab desktop"),
        ),
        200,
    );
    assert_eq!(done["workspace"]["name"], "Thesis lab");
    assert_eq!(done["me"]["name"], "Lee Park");
    assert_eq!(done["machine"]["name"], "Lab desktop");
    let workspace_id = done["workspace"]["id"].as_str().unwrap().to_owned();
    let person = done["me"]["id"].as_str().unwrap().to_owned();
    let machine = done["machine"]["id"].as_str().unwrap().to_owned();

    let workspace = ok(&daemon.get("/v1/workspace", Some(&device)), 200);
    assert_eq!(workspace["workspace"]["name"], "Thesis lab");
    assert!(workspace.get("setup_needed").is_none(), "{workspace}");
    let me = ok(&daemon.get("/v1/me", Some(&device)), 200);
    assert_eq!(me["handle"], "@lee");
    // The name is kept: workspace.json, written right after.
    let saved: Value = eventually("workspace.json", || {
        let text = std::fs::read_to_string(state.join("workspace.json")).ok()?;
        serde_json::from_str(&text).ok()
    });
    assert_eq!(saved, json!({ "id": workspace_id, "name": "Thesis lab" }));

    // @office, an agent of the person, and acting.
    let office = eventually("@office", || {
        members_called(&daemon, &device, "@office").pop()
    });
    assert_eq!(office["kind"], "agent");
    assert_eq!(office["owner"], person.as_str());
    daemon.wait_for_log("the back office acts as @office", WAIT);

    // The runner, on the new machine, watching the homes: the transcript is a session.
    let info = eventually("the runner's role", || {
        let info = host_info(&daemon);
        (info["roles"] == json!(["hub", "runner"])).then_some(info)
    });
    assert_eq!(info["capabilities"], json!(["watch"]));
    // HEAD follows too: the length of the answer with the runner, never the router's fixed one.
    let (head, get) = host_info_head_and_get(&daemon);
    assert_eq!(head, get);
    assert!(head > fresh_length, "{head} <= {fresh_length}");
    let session = eventually("the transcript's session", || {
        ok(&daemon.get("/v1/sessions", Some(&device)), 200)
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["native_id"] == NATIVE)
            .cloned()
    });
    assert_eq!(session["machine"], machine.as_str());
    assert_eq!(session["engine"], "claude");

    let office_id = office["id"].as_str().unwrap().to_owned();
    the_office_moves_a_finished_dispatch(&daemon, &device, &workspace_id, &person, &office_id);

    // Once only.
    let again = daemon.post(
        "/v1/setup",
        Some(&device),
        &setup_body("Other", "Kim", "@kim", "Other PC"),
    );
    assert_eq!(again.status, 409, "{}", again.body);
    assert_eq!(again.code(), "conflict");
    assert!(
        !daemon.stderr().contains(&device[4..]),
        "the token leaked into the logs"
    );

    // A restart keeps the name, and starts the office and the runner at once.
    daemon.stop();
    let daemon = fresh(&state, &homes);
    let workspace = ok(&daemon.get("/v1/workspace", Some(&device)), 200);
    assert_eq!(workspace["workspace"]["name"], "Thesis lab");
    assert!(workspace.get("setup_needed").is_none(), "{workspace}");
    let info = host_info(&daemon);
    assert_eq!(info["roles"], json!(["hub", "runner"]));
    assert_eq!(info["capabilities"], json!(["watch"]));
    daemon.wait_for_log("the back office acts as @office", WAIT);
    let offices = members_called(&daemon, &device, "@office");
    assert_eq!(offices.len(), 1, "{offices:?}");
    assert_eq!(offices[0]["id"], office_id.as_str());
    assert!(!daemon.stderr().contains(SET_UP));
    let sessions = ok(&daemon.get("/v1/sessions", Some(&device)), 200);
    let found: Vec<&Value> = sessions
        .as_array()
        .unwrap()
        .iter()
        .filter(|s| s["native_id"] == NATIVE)
        .collect();
    assert_eq!(found.len(), 1, "{sessions}");
    assert_eq!(found[0]["id"], session["id"]);
}

/// Two setups at once: exactly one person; every later one is a `409`.
#[test]
fn setup_runs_once_even_when_two_race() {
    let (tmp, state) = state_dir();
    let homes = tmp.path().join("homes");
    let daemon = fresh(&state, &homes);
    let device = daemon.device_token();
    let barrier = Barrier::new(2);
    let mut statuses: Vec<u16> = std::thread::scope(|scope| {
        let racers: Vec<_> = [("Lee", "@lee"), ("Kim", "@kim")]
            .into_iter()
            .map(|(name, handle)| {
                let (barrier, device, port) = (&barrier, device.as_str(), daemon.port);
                scope.spawn(move || {
                    let body = setup_body("Lab", name, handle, "PC");
                    barrier.wait();
                    request(port, "POST", "/v1/setup", Some(device), Some(&body), &[]).status
                })
            })
            .collect();
        racers.into_iter().map(|r| r.join().unwrap()).collect()
    });
    statuses.sort_unstable();
    assert_eq!(statuses, [200, 409]);
    let people: Vec<Value> = ok(&daemon.get("/v1/members", Some(&device)), 200)
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["kind"] == "human")
        .cloned()
        .collect();
    assert_eq!(people.len(), 1, "{people:?}");
    let again = daemon.post(
        "/v1/setup",
        Some(&device),
        &setup_body("Lab", "Ash", "@ash", "PC"),
    );
    assert_eq!(again.status, 409, "{}", again.body);
    // The office and the runner started once.
    daemon.wait_for_log("the back office acts as @office", WAIT);
    let logs = daemon.stderr();
    assert_eq!(logs.matches(SET_UP).count(), 1, "{logs}");
    assert_eq!(members_called(&daemon, &device, "@office").len(), 1);
}

/// `pitcrewd --state-dir <state> init …`, with a home of its own.
fn init(state: &Path, args: &[&str]) -> Output {
    let mut all: Vec<&OsStr> = vec![OsStr::new("--state-dir"), state.as_os_str()];
    all.push(OsStr::new("init"));
    all.extend(args.iter().map(OsStr::new));
    common::run(&all)
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// `pitcrewd init` sets up the daemon running on its state directory, once; without a daemon it
/// says to start one; the API's `400` and `409` are its messages. It never prints the token.
#[test]
fn init_sets_up_the_running_daemon_once() {
    let (tmp, state) = state_dir();
    let names = [
        "--workspace",
        "Thesis lab",
        "--name",
        "Lee Park",
        "--handle",
        "lee",
        "--machine",
        "Lab desktop",
    ];

    // No daemon has run here: a clear error, and nothing is created.
    let out = init(&state, &names);
    assert_eq!(out.status.code(), Some(5), "{}", text(&out.stderr));
    let said = text(&out.stderr);
    assert!(said.contains("no pitcrewd is running on"), "{said}");
    assert!(said.contains("serve"), "{said}");
    assert!(out.stdout.is_empty());
    assert!(!state.exists());

    // The private transport on Unix (the socket in the state directory); development TCP
    // elsewhere, since the private pipe is this user's own and a real daemon may hold it.
    let homes = tmp.path().join("homes");
    let homes_arg = homes.to_str().unwrap().to_owned();
    let mut daemon = if cfg!(unix) {
        Daemon::start_on(&state, "private", &["--homes", &homes_arg])
    } else {
        Daemon::start(&state, &["--homes", &homes_arg])
    };
    let listen = format!("tcp:127.0.0.1:{}", daemon.port);
    let with_listen = |extra: &[&str]| -> Vec<String> {
        let mut args: Vec<String> = extra.iter().map(|s| (*s).to_owned()).collect();
        if !cfg!(unix) {
            args.extend(["--listen".to_owned(), listen.clone()]);
        }
        args
    };
    let run = |extra: &[&str]| {
        let args = with_listen(extra);
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        init(&state, &args)
    };
    let token = daemon.device_token();

    let out = run(&names);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        text(&out.stdout),
        "Set up the workspace \"Thesis lab\": you are @lee (Lee Park) on \"Lab desktop\".\n"
    );
    for printed in [&out.stdout, &out.stderr] {
        assert!(
            !text(printed).contains(&token[4..]),
            "the token was printed"
        );
    }
    daemon.wait_for_log("the back office acts as @office", WAIT);

    // Again: the API's 409, with its message.
    let out = run(&names);
    assert_eq!(out.status.code(), Some(4), "{}", text(&out.stderr));
    let said = text(&out.stderr);
    assert!(said.contains("already has a person"), "{said}");
    // A malformed handle: the API's 400, which comes before the 409.
    let mut bad = names;
    bad[5] = "@Lee";
    let out = run(&bad);
    assert_eq!(out.status.code(), Some(2), "{}", text(&out.stderr));
    let said = text(&out.stderr);
    assert!(said.contains("person.handle must be"), "{said}");
    assert!(!said.contains(&token[4..]));

    // Its daemon stopped: the same clear error.
    daemon.stop();
    let out = run(&names);
    assert_eq!(out.status.code(), Some(5), "{}", text(&out.stderr));
    assert!(text(&out.stderr).contains("no pitcrewd is running on"));
}

/// `pitcrewd connect` with nothing to connect to: the bridge's own usage error (exit 2), and
/// nothing on stdout.
#[test]
fn connect_without_a_socket_is_a_usage_error() {
    let out = common::run(&[OsStr::new("connect")]);
    assert_eq!(out.status.code(), Some(2), "{}", text(&out.stderr));
    assert!(out.stdout.is_empty());
    assert!(text(&out.stderr).contains("usage: pitcrewd connect"));
    let out = common::run(&[OsStr::new("connect"), OsStr::new("--verbose")]);
    assert_eq!(out.status.code(), Some(2), "{}", text(&out.stderr));
}

/// `pitcrewd connect --socket <dir>/pitcrewd.sock` carries an HTTP request to a running daemon
/// through stdin and stdout, after its ready mark; it uses no state directory.
#[cfg(unix)]
#[test]
fn connect_carries_a_request_to_the_daemon() {
    use std::io::Write as _;
    use std::process::{Command, Stdio};

    let (tmp, state) = state_dir();
    let socket = tmp.path().join("sock").join("pitcrewd.sock");
    let daemon = Daemon::start_on(&state, &format!("unix:{}", socket.display()), &["--demo"]);
    let device = daemon.device_token();
    let home = tmp.path().join("bridge-home");
    std::fs::create_dir(&home).unwrap();

    let bridge = |stdin: &[u8], extra: &[&str]| -> Output {
        let mut command = Command::new(common::PITCREWD);
        command
            .arg("connect")
            .arg("--socket")
            .arg(&socket)
            .args(extra);
        pitcrew_fixtures::homes::private_home(&mut command, &home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = common::spawn(&mut command).unwrap();
        // Stdin stays open while the answer comes, as an HTTP client's does: the daemon closes
        // the connection once it has answered (`Connection: close`), which ends the bridge.
        let mut input = child.stdin.take().unwrap();
        input.write_all(stdin).unwrap();
        let (done, output) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = done.send(child.wait_with_output());
        });
        let output = output
            .recv_timeout(WAIT)
            .expect("the bridge ended")
            .unwrap();
        drop(input);
        output
    };

    let out = bridge(
        b"GET /v1/host/info HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
        &["--nonce", "0a1b"],
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    let mark = b"\0pitcrew-bridge 1 ready 0a1b\n";
    assert!(out.stdout.starts_with(mark), "{:?}", text(&out.stdout));
    let response = text(&out.stdout[mark.len()..]);
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(response.contains("\"name\":\"pitcrewd\""), "{response}");

    // An authenticated request goes through as well.
    let request = format!(
        "GET /v1/workspace HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {device}\r\n\
         Connection: close\r\n\r\n"
    );
    let out = bridge(request.as_bytes(), &[]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(out.stdout.starts_with(b"\0pitcrew-bridge 1 ready\n"));
    let response = text(&out.stdout);
    assert!(response.contains("HTTP/1.1 200"), "{response}");
    assert!(response.contains("\"name\":\"Demo Lab\""), "{response}");

    // No state directory, nor anything else, in its home.
    let made: Vec<_> = std::fs::read_dir(&home).unwrap().collect();
    assert!(made.is_empty(), "{made:?}");

    // No daemon there: exit 4.
    let gone = tmp.path().join("nothing").join("pitcrewd.sock");
    let out = common::run(&[
        OsStr::new("connect"),
        OsStr::new("--socket"),
        gone.as_os_str(),
    ]);
    assert_eq!(out.status.code(), Some(4), "{}", text(&out.stderr));
    assert!(out.stdout.is_empty());
}
