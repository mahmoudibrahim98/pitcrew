//! The runner in `pitcrewd serve`, end to end: the real binary, with temporary agent homes filled
//! from `crates/fixtures` (`--homes`), never the machine's own.

#![allow(clippy::unwrap_used)]

mod common;

use common::{Daemon, Ws, home_of, id};
use pitcrew_protocol::events::{Event, EventBody};
use pitcrew_protocol::model::Session;
use serde_json::{Value, json};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(20);

/// The Claude fixture's own session id, which its lines carry.
const FIXTURE_ID: &str = "2b6f1a8e-4c1d-4f5e-9a37-0c8d1e2f3a4b";

fn state_dir() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    (tmp, state)
}

/// The Claude fixture's lines, each with its newline, as the session `native`.
fn fixture_lines(native: &str) -> Vec<Vec<u8>> {
    let path = pitcrew_fixtures::data_dir().join("transcripts/claude/demo-session.jsonl");
    String::from_utf8(std::fs::read(path).unwrap())
        .unwrap()
        .replace(FIXTURE_ID, native)
        .split_inclusive('\n')
        .map(|l| l.as_bytes().to_vec())
        .collect()
}

/// Writes `lines` as Claude's transcript of session `native` in the Claude home `claude`.
fn claude_transcript(claude: &Path, native: &str, lines: &[Vec<u8>]) -> PathBuf {
    let dir = claude
        .join("projects")
        .join("-home-sam-work-diffusion-paper-paper");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{native}.jsonl"));
    std::fs::write(&path, lines.concat()).unwrap();
    path
}

fn append(path: &Path, lines: &[Vec<u8>]) {
    let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
    file.write_all(&lines.concat()).unwrap();
    file.sync_all().unwrap();
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

/// The hub's session whose CLI id is `native`, once there is one.
fn session_named(daemon: &Daemon, token: &str, native: &str) -> Value {
    eventually(&format!("a session {native}"), || {
        let reply = daemon.get("/v1/sessions", Some(token));
        assert_eq!(reply.status, 200, "{}", reply.body);
        reply
            .json()
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["native_id"] == native)
            .cloned()
    })
}

fn session(daemon: &Daemon, token: &str, id: &str) -> Value {
    let reply = daemon.get(&format!("/v1/sessions/{id}"), Some(token));
    assert_eq!(reply.status, 200, "{}", reply.body);
    reply.json()
}

fn transcript(daemon: &Daemon, token: &str, id: &str) -> Value {
    let reply = daemon.get(&format!("/v1/sessions/{id}/transcript"), Some(token));
    assert_eq!(reply.status, 200, "{}", reply.body);
    reply.json()
}

/// `POST /v1/hooks/claude/<event>` for the CLI session `native`, as `token`.
fn hook(daemon: &Daemon, token: &str, event: &str, native: &str, extra: &Value) {
    let mut body = json!({ "session_id": native, "hook_event_name": event });
    body.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    let reply = daemon.post(&format!("/v1/hooks/claude/{event}"), Some(token), &body);
    assert_eq!(reply.status, 202, "{}", reply.body);
}

fn waiting_for(message: &str) -> Value {
    json!({ "notification_type": "permission_prompt", "message": message })
}

/// Gives a session an agent in the hub, as a dispatch does before its CLI starts: a
/// `session_discovered` naming the agent, appended from this process with the work model's
/// projections, so the hub's tables have it at once.
fn give_agent(state: &Path, session: &Value, agent: &str) {
    let store = pitcrew_store::Store::open_with(
        state.join("hub.db"),
        pitcrew_store::StoreOptions::default(),
        pitcrew_hub_work::projections(),
    )
    .unwrap();
    let mut s: Session = serde_json::from_value(session.clone()).unwrap();
    s.agent = Some(agent.parse().unwrap());
    let event = Event::now(
        id::WORKSPACE.parse().unwrap(),
        id::SAM.parse().unwrap(),
        EventBody::SessionDiscovered { session: s },
    );
    store.append(&[event]).unwrap();
}

/// The fixture transcript is a session of this machine, through the API, with its transcript;
/// what is appended to the file reaches the stream live, and the transcript grows with it.
#[test]
fn a_transcript_is_a_session_with_its_transcript_and_live_records() {
    let (tmp, state) = state_dir();
    let homes = tmp.path().join("homes");
    let native = "5e1f0c2a-7b3d-4e8f-9a1b-2c3d4e5f6a7b";
    let lines = fixture_lines(native);
    let path = claude_transcript(&homes.join(".claude"), native, &lines[..5]);
    let daemon = Daemon::start(&state, &["--demo", "--homes", homes.to_str().unwrap()]);
    let device = daemon.device_token();
    let info = daemon.get("/v1/host/info", None).json();
    assert_eq!(info["roles"], json!(["hub", "runner"]));
    assert_eq!(info["capabilities"], json!(["watch"]));
    daemon.wait_for_log("the runner watches these homes", WAIT);

    let found = session_named(&daemon, &device, native);
    let sid = found["id"].as_str().unwrap().to_owned();
    assert_eq!(found["engine"], "claude");
    assert_eq!(found["machine"], id::LAPTOP, "the hub's own machine");
    assert_eq!(found["cwd"], "/home/sam/work/diffusion-paper/paper");
    assert!(found["agent"].is_null(), "{found}");
    assert_eq!(found["state"], "working");

    // Its transcript, from the file: what the first five records hold.
    let written = u64::try_from(lines[..5].concat().len()).unwrap();
    let page = transcript(&daemon, &device, &sid);
    assert_eq!(page["at_start"], true);
    assert_eq!(page["from"], 0);
    let to = page["to"].as_u64().unwrap();
    assert!(0 < to && to <= written, "{page}");
    let items = page["items"].as_array().unwrap().clone();
    assert!(items.len() > 1, "{page}");
    assert_eq!(items[0]["kind"], "user_prompt", "{page}");
    // Paged by one item: the newest record alone, then everything before it.
    let last = daemon
        .get(
            &format!("/v1/sessions/{sid}/transcript?limit=1"),
            Some(&device),
        )
        .json();
    assert_eq!(last["to"], page["to"]);
    assert_eq!(last["at_start"], false);
    let before = last["from"].as_u64().unwrap();
    let older = daemon
        .get(
            &format!("/v1/sessions/{sid}/transcript?before={before}"),
            Some(&device),
        )
        .json();
    assert_eq!(older["at_start"], true);
    let mut joined = older["items"].as_array().unwrap().clone();
    joined.extend(last["items"].as_array().unwrap().iter().cloned());
    assert_eq!(joined, items);
    for bad in ["limit=0", "limit=x", "before=-1", "before=1.5"] {
        let reply = daemon.get(
            &format!("/v1/sessions/{sid}/transcript?{bad}"),
            Some(&device),
        );
        assert_eq!(reply.status, 400, "{bad}: {}", reply.body);
        assert_eq!(reply.code(), "invalid");
    }

    // Appended records reach the stream as they are written.
    let mut stream = Ws::connect(daemon.port, "/v1/stream", &device).unwrap();
    assert_eq!(stream.next_json(WAIT)["type"], "hello");
    append(&path, &lines[5..]);
    let mut seen: Vec<String> = Vec::new();
    let deadline = Instant::now() + WAIT;
    while !seen.iter().any(|t| t == "turn_ended") {
        assert!(
            Instant::now() < deadline,
            "no turn_ended on the stream: {seen:?}"
        );
        let frame = stream.next_json(WAIT);
        if frame["type"] != "events" {
            continue;
        }
        for event in frame["events"].as_array().unwrap() {
            let data = &event["body"]["data"];
            if data["session"] == sid.as_str() {
                seen.push(event["body"]["type"].as_str().unwrap().to_owned());
            }
        }
    }
    assert!(seen.iter().any(|t| t == "file_edited"), "{seen:?}");
    assert!(seen.iter().any(|t| t == "tool_ran"), "{seen:?}");
    assert_eq!(session(&daemon, &device, &sid)["state"], "idle");

    // The transcript grew with the file.
    let page = transcript(&daemon, &device, &sid);
    assert!(page["to"].as_u64().unwrap() > to, "{page}");
    assert!(page["items"].as_array().unwrap().len() > items.len());

    // And the activity log has the session's records, each once.
    let events = daemon.events_matching(&format!("&session={sid}"), &device);
    let edits = events
        .iter()
        .filter(|e| e["body"]["type"] == "file_edited")
        .count();
    assert_eq!(edits, 1, "{events:?}");
}

/// Transcript pages come from the runner: an indexed transcript's pages; an empty page at the
/// start for a session of this machine the runner never indexed (the demo's); and `503` for a
/// transcript that was deleted, saying why.
#[test]
fn transcript_pages_come_from_the_runner() {
    let (tmp, state) = state_dir();
    let homes = tmp.path().join("homes");
    let native = "9a8b7c6d-1111-4222-8333-444455556666";
    let path = claude_transcript(&homes.join(".claude"), native, &fixture_lines(native)[..5]);
    let daemon = Daemon::start(&state, &["--demo", "--homes", homes.to_str().unwrap()]);
    let device = daemon.device_token();
    let sid = session_named(&daemon, &device, native)["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let page = transcript(&daemon, &device, &sid);
    assert_eq!(page["at_start"], true, "{page}");
    assert!(!page["items"].as_array().unwrap().is_empty(), "{page}");

    let demo = transcript(&daemon, &device, id::SES1);
    assert_eq!(
        demo,
        json!({ "items": [], "from": 0, "to": 0, "at_start": true })
    );

    std::fs::remove_file(&path).unwrap();
    let reply = daemon.get(&format!("/v1/sessions/{sid}/transcript"), Some(&device));
    assert_eq!(reply.status, 503, "{}", reply.body);
    assert_eq!(reply.code(), "unavailable");
    assert!(
        reply.body.contains("the transcript is gone"),
        "{}",
        reply.body
    );
    assert!(
        !reply.body.contains(path.to_str().unwrap()),
        "no path: {}",
        reply.body
    );
}

/// A hook changes a session only when its sender may (the runner's ownership rule), through the
/// real hook route and real tokens: the session's own agent, or the person who owns it; never
/// another agent. Each refused hook would have left a state no allowed one does.
#[test]
fn hooks_change_a_session_only_for_its_agent_or_their_person() {
    let (tmp, state) = state_dir();
    let homes = tmp.path().join("homes");
    let claude = homes.join(".claude");
    // Three sessions, all working: one to be @writer's, one @reviewer's, and one with no agent.
    let natives = [
        "aaaaaaaa-1111-4111-8111-aaaaaaaaaaaa",
        "bbbbbbbb-2222-4222-8222-bbbbbbbbbbbb",
        "cccccccc-3333-4333-8333-cccccccccccc",
    ];
    for native in natives {
        claude_transcript(&claude, native, &fixture_lines(native)[..5]);
    }
    let daemon = Daemon::start(&state, &["--demo", "--homes", homes.to_str().unwrap()]);
    let (device, writer) = (daemon.device_token(), daemon.agent_token());
    let [a, b, c] = natives.map(|n| session_named(&daemon, &device, n));
    give_agent(&state, &a, id::WRITER);
    give_agent(&state, &b, id::REVIEWER);
    let id_of = |s: &Value| s["id"].as_str().unwrap().to_owned();
    let (a_id, b_id, c_id) = (id_of(&a), id_of(&b), id_of(&c));
    assert_eq!(session(&daemon, &device, &a_id)["agent"], id::WRITER);
    assert_eq!(session(&daemon, &device, &b_id)["agent"], id::REVIEWER);
    assert!(session(&daemon, &device, &c_id)["agent"].is_null());

    // Refused: @writer on @reviewer's session, and on one without an agent.
    hook(&daemon, &writer, "Stop", natives[1], &json!({}));
    hook(&daemon, &writer, "Stop", natives[2], &json!({}));
    // Allowed, after those (hooks are taken in order): @writer on its own session; @sam, a
    // person, on his agent's session and on one without an agent.
    let bash = "Claude needs your permission to use Bash";
    let edit = "Claude needs your permission to use Edit";
    let read = "Claude needs your permission to use Read";
    hook(
        &daemon,
        &writer,
        "Notification",
        natives[0],
        &waiting_for(bash),
    );
    hook(
        &daemon,
        &device,
        "Notification",
        natives[1],
        &waiting_for(edit),
    );
    hook(
        &daemon,
        &device,
        "Notification",
        natives[2],
        &waiting_for(read),
    );

    for (sid, line) in [(&a_id, bash), (&b_id, edit), (&c_id, read)] {
        eventually(&format!("{sid} waits for {line:?}"), || {
            let s = session(&daemon, &device, sid);
            (s["state"] == "waiting" && s["status_line"] == line).then_some(())
        });
    }
    // The refused Stops never applied: no session became idle.
    for sid in [&a_id, &b_id, &c_id] {
        let events = daemon.events_matching(&format!("&session={sid}"), &device);
        let idle: Vec<&Value> = events
            .iter()
            .filter(|e| {
                e["body"]["type"] == "session_state_changed" && e["body"]["data"]["to"] == "idle"
            })
            .collect();
        assert!(idle.is_empty(), "{sid}: {idle:?}");
    }

    // And the person's own hook on an agent's session they own works too; an agent's on it, no.
    hook(
        &daemon,
        &writer,
        "Notification",
        natives[1],
        &waiting_for(bash),
    );
    hook(&daemon, &device, "UserPromptSubmit", natives[0], &json!({}));
    eventually("@sam's prompt on @writer's session", || {
        (session(&daemon, &device, &a_id)["state"] == "working").then_some(())
    });
    assert_eq!(session(&daemon, &device, &b_id)["status_line"], edit);
}

/// A demo never shows the person's own sessions: with `--demo` and no `--homes`, the runner
/// watches no home, not even this user's (here the daemon's stand-in home, holding a transcript).
/// Without `--demo`, this user's homes are watched by default.
#[test]
fn a_demo_watches_no_home_of_its_own() {
    let (_tmp, state) = state_dir();
    let native = "dddddddd-4444-4444-8444-dddddddddddd";
    claude_transcript(
        &home_of(&state).join(".claude"),
        native,
        &fixture_lines(native),
    );

    let mut daemon = Daemon::start(&state, &["--demo"]);
    let device = daemon.device_token();
    daemon.wait_for_log("the runner watches no home", WAIT);
    let info = daemon.get("/v1/host/info", None).json();
    assert_eq!(info["roles"], json!(["hub", "runner"]));
    // It runs, but watches nothing.
    assert_eq!(info["capabilities"], json!([]));
    // Its first discovery has long finished when the recap index is built and the back office has
    // looked at the seed; give the watcher a while more anyway.
    daemon.settle(&device);
    std::thread::sleep(Duration::from_secs(2));
    let sessions = daemon.get("/v1/sessions", Some(&device)).json();
    assert!(
        sessions
            .as_array()
            .unwrap()
            .iter()
            .all(|s| s["native_id"] != native),
        "{sessions}"
    );
    daemon.stop();
    drop(daemon);

    // Started again without --demo, it watches this user's homes.
    let daemon = Daemon::start(&state, &[]);
    daemon.wait_for_log("the runner watches these homes", WAIT);
    let found = session_named(&daemon, &device, native);
    assert_eq!(found["state"], "idle");
    assert!(
        daemon.stderr().contains(&format!(
            "Claude={}",
            home_of(&state).join(".claude").display()
        )),
        "{}",
        daemon.stderr()
    );
}

/// `--no-runner`: no runner role, hooks are only logged, and no session has a terminal or a
/// transcript here.
#[test]
fn no_runner_watches_nothing_and_serves_no_terminal() {
    let (tmp, state) = state_dir();
    let homes = tmp.path().join("homes");
    let native = "eeeeeeee-5555-4555-8555-eeeeeeeeeeee";
    claude_transcript(&homes.join(".claude"), native, &fixture_lines(native));
    // --homes needs the runner.
    let refused = Daemon::try_start(
        &state,
        &["--demo", "--no-runner", "--homes", homes.to_str().unwrap()],
    )
    .unwrap_err();
    assert!(!refused.status.success());

    let daemon = Daemon::start(&state, &["--demo", "--no-runner"]);
    let device = daemon.device_token();
    daemon.wait_for_log("the runner is off (--no-runner)", WAIT);
    let info = daemon.get("/v1/host/info", None).json();
    assert_eq!(info["roles"], json!(["hub"]));
    assert_eq!(info["capabilities"], json!([]));
    for route in ["terminal", "transcript"] {
        let reply = daemon.get(&format!("/v1/sessions/{}/{route}", id::SES1), Some(&device));
        assert_eq!(reply.status, 503, "{route}: {}", reply.body);
    }
    let reply = daemon.post(
        "/v1/hooks/claude/Stop",
        Some(&device),
        &json!({ "session_id": native }),
    );
    assert_eq!(reply.status, 202);
    daemon.wait_for_log("hook received", WAIT);
}

/// SIGTERM: the runner hands the store what it read and stops before the store closes, as the
/// back office does. A restart keeps the session's id and stores nothing twice.
#[cfg(unix)]
#[test]
fn the_runner_stops_before_the_store_closes_and_a_restart_repeats_nothing() {
    let (tmp, state) = state_dir();
    let homes = tmp.path().join("homes");
    let homes_arg = homes.to_str().unwrap().to_owned();
    let native = "ffffffff-6666-4666-8666-ffffffffffff";
    let lines = fixture_lines(native);
    let path = claude_transcript(&homes.join(".claude"), native, &lines[..5]);

    let mut daemon = Daemon::start(&state, &["--demo", "--homes", &homes_arg]);
    let device = daemon.device_token();
    let sid = session_named(&daemon, &device, native)["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let before = daemon.events_matching(&format!("&session={sid}"), &device);
    let status = daemon.terminate();
    assert!(status.success(), "{status}:\n{}", daemon.stderr());
    let logs = daemon.stderr();
    let at = |what: &str| {
        logs.find(what)
            .unwrap_or_else(|| panic!("no {what:?} in the log:\n{logs}"))
    };
    assert!(at("the runner stopped") < at("store closed"), "{logs}");
    assert!(at("the back office stopped") < at("store closed"), "{logs}");
    drop(daemon);

    // Written while it was stopped: read at the next start.
    append(&path, &lines[5..]);
    let daemon = Daemon::start(&state, &["--homes", &homes_arg]);
    let again = session_named(&daemon, &device, native);
    assert_eq!(again["id"], sid.as_str(), "the same session");
    eventually("the rest of the transcript", || {
        (session(&daemon, &device, &sid)["state"] == "idle").then_some(())
    });
    let after = daemon.events_matching(&format!("&session={sid}"), &device);
    let ids = |events: &[Value]| -> Vec<String> {
        events
            .iter()
            .map(|e| e["id"].as_str().unwrap().to_owned())
            .collect()
    };
    let (before, after) = (ids(&before), ids(&after));
    assert_eq!(
        after[..before.len()],
        before[..],
        "the first start's events"
    );
    let mut unique = after.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), after.len(), "an event stored twice");
    let sessions = daemon.get("/v1/sessions", Some(&device)).json();
    let named = sessions
        .as_array()
        .unwrap()
        .iter()
        .filter(|s| s["native_id"] == native)
        .count();
    assert_eq!(named, 1, "{sessions}");
}

/// Makes `dir` as the daemon would accept it: private (0700) on Unix.
fn private_dir(dir: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        std::fs::DirBuilder::new().mode(0o700).create(dir).unwrap();
    }
    #[cfg(not(unix))]
    std::fs::create_dir(dir).unwrap();
}

/// A runner that cannot start (here its index cannot be made: `runner` in the state directory is
/// a file) leaves the hub serving without it: warned with the reason and the folder, roles
/// `["hub"]`, and the work model as ever.
#[test]
fn a_runner_that_cannot_start_leaves_the_hub_serving() {
    let (tmp, state) = state_dir();
    let homes = tmp.path().join("homes");
    let native = "abababab-7777-4777-8777-abababababab";
    claude_transcript(&homes.join(".claude"), native, &fixture_lines(native));
    private_dir(&state);
    std::fs::write(state.join("runner"), "not a folder").unwrap();

    let daemon = Daemon::start(&state, &["--demo", "--homes", homes.to_str().unwrap()]);
    let device = daemon.device_token();
    daemon.wait_for_log("the runner cannot start", WAIT);
    let logs = daemon.stderr();
    let warning = logs
        .lines()
        .find(|l| l.contains("the runner cannot start"))
        .unwrap();
    assert!(warning.contains("WARN"), "{warning}");
    assert!(
        warning.contains(&state.join("runner").display().to_string()),
        "the folder is named: {warning}"
    );
    let info = daemon.get("/v1/host/info", None).json();
    assert_eq!(info["roles"], json!(["hub"]));
    assert_eq!(info["capabilities"], json!([]));
    let tasks = daemon.get("/v1/tasks", Some(&device));
    assert_eq!(tasks.status, 200, "{}", tasks.body);
    for route in ["terminal", "transcript"] {
        let reply = daemon.get(&format!("/v1/sessions/{}/{route}", id::SES1), Some(&device));
        assert_eq!(reply.status, 503, "{route}: {}", reply.body);
    }
    let sessions = daemon.get("/v1/sessions", Some(&device)).json();
    assert!(
        sessions
            .as_array()
            .unwrap()
            .iter()
            .all(|s| s["native_id"] != native),
        "{sessions}"
    );
}

/// A GET that tolerates the server going away: the bytes it got, if any.
#[cfg(unix)]
fn get_raw(port: u16, path: &str, token: &str) -> Vec<u8> {
    use std::io::Read as _;
    let Ok(mut stream) = std::net::TcpStream::connect(("127.0.0.1", port)) else {
        return Vec::new();
    };
    let head = format!(
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer {token}\r\n\
         Connection: close\r\n\r\n"
    );
    let mut got = Vec::new();
    if stream.write_all(head.as_bytes()).is_ok() {
        let _ = stream.read_to_end(&mut got);
    }
    got
}

/// A stop always finishes in bounded time: here the transcript is swapped for a FIFO that no one
/// writes, so opening it waits forever, both in the runner's watcher (it reads a changed file)
/// and in a transcript request. The daemon still exits, cleanly, and says what it left behind.
#[cfg(unix)]
#[test]
fn a_read_that_never_returns_does_not_keep_the_daemon_from_stopping() {
    let (tmp, state) = state_dir();
    let homes = tmp.path().join("homes");
    let native = "cdcdcdcd-8888-4888-8888-cdcdcdcdcdcd";
    let path = claude_transcript(&homes.join(".claude"), native, &fixture_lines(native)[..5]);
    let mut daemon = Daemon::start(&state, &["--demo", "--homes", homes.to_str().unwrap()]);
    let device = daemon.device_token();
    let sid = session_named(&daemon, &device, native)["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let fifo = tmp.path().join("fifo");
    let made = std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .unwrap();
    assert!(made.success());
    std::fs::rename(&fifo, &path).unwrap();
    let request = {
        let (port, token) = (daemon.port, device.clone());
        let path = format!("/v1/sessions/{sid}/transcript");
        std::thread::spawn(move || get_raw(port, &path, &token))
    };
    // The watcher notices the change within its debounce, and the request is on its way.
    std::thread::sleep(Duration::from_secs(1));

    let stopping = Instant::now();
    daemon.send("TERM");
    let status = daemon.wait_exit(Duration::from_secs(60));
    let took = stopping.elapsed();
    let logs = daemon.stderr();
    assert!(status.success(), "{status}:\n{logs}");
    // Drain (10 s) + the store's release (3 s) + the blocking pool (5 s), and some slack.
    assert!(took < Duration::from_secs(30), "{took:?}:\n{logs}");
    for said in [
        "the runner is still stopping",
        "work on the blocking pool is still running",
        "the store is still open at exit",
        "stopped",
    ] {
        assert!(logs.contains(said), "no {said:?} in:\n{logs}");
    }
    let _ = request.join();
}
