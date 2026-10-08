//! Dispatch, end to end: the real binary with a seeded demo, its runner watching temporary agent
//! homes (`--homes`), its terminals in the pitcrew-ptyd cargo built next to it (forced with
//! `--terminal-runtime pty`, on an endpoint of the test's), and stand-in `claude` and `codex`
//! first on its `PATH` (shell scripts: this file runs on Unix; the PTY runtime itself is tested
//! on Windows too, in `tests/pty.rs`).
//!
//! For each CLI a task is dispatched to an agent whose persona runs it, in a project folder of the
//! test's. The stand-in writes its transcript as the CLI does when started with a prompt (Claude
//! under the `--session-id` it was given; Codex a rollout naming its folder and start time), and
//! notes what it was given to reach the hub. The test then checks that:
//! - the session appears once, under the dispatch's id, linked to the task, with its terminal;
//! - the CLI was given an **agent** token's file for the dispatched agent (never the person's
//!   token, and no token in its environment), and where the daemon listens; and that holds though
//!   the daemon itself was started with the person's token in `PITCREW_TOKEN` and another socket in
//!   `PITCREW_SOCKET`, which its terminals pass on: read as the CLI reads them
//!   (`pitcrew_cli::config`), the CLI's variables give the agent's token and the daemon's address;
//! - the agent's own hooks change the session, and its first `working` moves the task to in
//!   progress;
//! - the agent's report (its move to review, what `pitcrew report --review` sends) moves the task
//!   to review and finishes the dispatch as succeeded.
//!
//! **Nothing left behind**: every process the daemon starts carries `PITCREW_TEST_RUN=<mark>`;
//! the test ends the sessions, stops the daemon, and kills anything still marked (`/proc` on
//! Linux, `ps -E` on macOS). pitcrew-ptyd must have been built next to `pitcrewd` (`cargo test
//! --workspace` builds it); if it is not, the test says so and passes, unless `CI` or
//! `PITCREW_REQUIRE_PTYD=1` is set.

#![cfg(unix)]
#![allow(clippy::unwrap_used)]

mod common;

use common::{Daemon, Tmux, id};
use serde_json::{Value, json};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(60);

/// The variable that marks every process a test's daemon starts.
const MARK: &str = "PITCREW_TEST_RUN";
/// @runner, whose persona runs Codex.
const RUNNER: &str = "01JB000000000000000MEM0003";

/// The Claude fixture's own session id, which its lines carry.
const FIXTURE_ID: &str = "2b6f1a8e-4c1d-4f5e-9a37-0c8d1e2f3a4b";

/// What every stand-in does first: note, in `@OUT@/<its id>/`, what it was given to reach the
/// hub: the variables, in a private folder of the test's, and never printed (the token file's
/// text is read from the file the variable names; a token in `PITCREW_TOKEN` would be the one the
/// test started the daemon with).
const NOTE: &str = r#"out="@OUT@/$id"
mkdir -p "$out"
printf '%s' "${PITCREW_TOKEN_FILE:-}" > "$out/token-file"
printf '%s' "${PITCREW_URL:-}" > "$out/url"
if [ -n "${PITCREW_TOKEN:-}" ]; then : > "$out/token-in-env"; fi
env | grep '^PITCREW_' > "$out/env.part"; mv "$out/env.part" "$out/env"
pwd -P > "$out/cwd"
"#;

/// A stand-in for Claude Code given a first prompt: it writes its transcript at once (unless the
/// test holds it back with a `hold` file), for the session id it was given (`--session-id=`), in
/// its Claude home (`CLAUDE_CONFIG_DIR`, which ptyd passes on from the daemon), then waits to be
/// ended.
const FAKE_CLAUDE: &str = r#"#!/bin/sh
id=
for arg in "$@"; do
  case "$arg" in --session-id=*) id=${arg#--session-id=} ;; esac
done
[ -n "$id" ] || { echo "no --session-id" >&2; exit 2; }
@NOTE@
while [ -e '@OUT@/hold' ]; do sleep 0.2; done
dir="${CLAUDE_CONFIG_DIR:?}/projects/-dispatch-work"
mkdir -p "$dir"
sed "s/@NATIVE@/$id/g" '@TEMPLATE@' > "$dir/$id.jsonl.part" && mv "$dir/$id.jsonl.part" "$dir/$id.jsonl"
echo "STAND-IN CLAUDE $id"
trap 'exit 0' INT TERM
while :; do sleep 1; done
"#;

/// A stand-in for Codex given a first prompt: it writes a rollout at once, in its Codex home
/// (`CODEX_HOME`), naming its own new id, its folder and the time, then waits to be ended.
const FAKE_CODEX: &str = r#"#!/bin/sh
id=$(cat /proc/sys/kernel/random/uuid 2>/dev/null || uuidgen | tr 'A-Z' 'a-z')
@NOTE@
now=$(date -u +%Y-%m-%dT%H:%M:%S.000Z)
cwd=$(pwd -P)
dir="${CODEX_HOME:?}/sessions/2026/10/03"
mkdir -p "$dir"
file="$dir/rollout-$id.jsonl"
{
  printf '{"timestamp":"%s","type":"session_meta","payload":{"id":"%s","timestamp":"%s","cwd":"%s","originator":"codex_cli_rs","cli_version":"0.50.0"}}\n' "$now" "$id" "$now" "$cwd"
  printf '{"timestamp":"%s","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"Submit the seeds"}]}}\n' "$now"
} > "$file.part" && mv "$file.part" "$file"
echo "STAND-IN CODEX $id"
trap 'exit 0' INT TERM
while :; do sleep 1; done
"#;

/// One test's setup: homes, project folders, the stand-ins, the ptyd to run, and its mark.
/// Dropped, it kills anything of its own still running.
struct Rig {
    tmp: tempfile::TempDir,
    homes: PathBuf,
    out: PathBuf,
    bin: PathBuf,
    ptyd: PathBuf,
    mark: String,
}

impl Rig {
    /// `None`, after saying why, where pitcrew-ptyd was not built next to `pitcrewd`; a failure
    /// instead under CI or with `PITCREW_REQUIRE_PTYD=1`.
    fn new() -> Option<Self> {
        let ptyd = Path::new(common::PITCREWD).with_file_name(pitcrew_runtime::pty::launch::PTYD);
        if !ptyd.is_file() {
            let required = std::env::var_os("CI").is_some_and(|v| !v.is_empty())
                || std::env::var("PITCREW_REQUIRE_PTYD").is_ok_and(|v| v == "1");
            assert!(
                !required,
                "pitcrew-ptyd is not built at {}: test the workspace, or build it first with \
                 `cargo build -p pitcrew-ptyd`",
                ptyd.display()
            );
            eprintln!(
                "skipped: pitcrew-ptyd is not built at {} (`cargo build -p pitcrew-ptyd`)",
                ptyd.display()
            );
            return None;
        }
        let tmp = tempfile::tempdir().unwrap();
        let homes = tmp.path().join("homes");
        let out = tmp.path().join("out");
        let bin = tmp.path().join("bin");
        for dir in [&homes, &out, &bin] {
            private_folder(dir);
        }
        std::fs::create_dir_all(homes.join(".claude").join("projects")).unwrap();
        std::fs::create_dir_all(homes.join(".codex").join("sessions")).unwrap();
        // The Claude fixture's first five records, as the session the stand-in is given.
        let fixture = pitcrew_fixtures::data_dir().join("transcripts/claude/demo-session.jsonl");
        let lines: String = std::fs::read_to_string(fixture)
            .unwrap()
            .replace(FIXTURE_ID, "@NATIVE@")
            .split_inclusive('\n')
            .take(5)
            .collect();
        let template = bin.join("transcript.jsonl");
        std::fs::write(&template, lines).unwrap();
        let note = NOTE.replace("@OUT@", out.to_str().unwrap());
        for (name, script) in [("claude", FAKE_CLAUDE), ("codex", FAKE_CODEX)] {
            let path = bin.join(name);
            let script = script
                .replace("@NOTE@", &note)
                .replace("@OUT@", out.to_str().unwrap())
                .replace("@TEMPLATE@", template.to_str().unwrap());
            std::fs::write(&path, script).unwrap();
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let mark = format!("dispatch-{}-{nanos}", std::process::id());
        Some(Self {
            tmp,
            homes,
            out,
            bin,
            ptyd,
            mark,
        })
    }

    /// A project folder of the test's, `name`.
    fn folder(&self, name: &str) -> PathBuf {
        let dir = self.tmp.path().join("work").join(name);
        private_folder(&dir);
        dir.canonicalize().unwrap()
    }

    /// A daemon on this rig's state (seeded with the demo when `demo`), watching this rig's homes,
    /// its terminals in this rig's ptyd, with the stand-ins first on its `PATH`.
    fn start(&self, demo: bool) -> Daemon {
        self.start_with(demo, &[])
    }

    /// [`Rig::start`], with `inherited` in the daemon's environment too.
    fn start_with(&self, demo: bool, inherited: &[(&str, OsString)]) -> Daemon {
        let path = std::env::var_os("PATH").unwrap_or_default();
        let path = std::env::join_paths(
            std::iter::once(self.bin.clone()).chain(std::env::split_paths(&path)),
        )
        .unwrap();
        let endpoint = self.tmp.path().join("p");
        private_folder(&endpoint);
        let endpoint = endpoint.join("ptyd");
        let mut args = vec![
            "--terminal-runtime",
            "pty",
            "--ptyd",
            self.ptyd.to_str().unwrap(),
            "--ptyd-endpoint",
            endpoint.to_str().unwrap(),
            "--ptyd-idle-exit-ms",
            "500",
            "--homes",
            self.homes.to_str().unwrap(),
        ];
        if demo {
            args.push("--demo");
        }
        let mut env = vec![
            ("PATH", path),
            (MARK, OsString::from(&self.mark)),
            (
                "CLAUDE_CONFIG_DIR",
                self.homes.join(".claude").into_os_string(),
            ),
            ("CODEX_HOME", self.homes.join(".codex").into_os_string()),
        ];
        env.extend(inherited.iter().cloned());
        Daemon::start_with(&self.tmp.path().join("state"), &args, &env, Tmux::Refused)
    }

    /// What the stand-in started as `native` noted, `what`, once it has.
    fn noted(&self, native: &str, what: &str) -> String {
        let path = self.out.join(native).join(what);
        eventually(&format!("the stand-in notes {what}"), || path.is_file());
        std::fs::read_to_string(path).unwrap()
    }

    /// Whether the stand-in started as `native` had a token in its environment.
    fn token_in_env(&self, native: &str) -> bool {
        self.out.join(native).join("token-in-env").exists()
    }

    /// The `PITCREW_*` variables the stand-in started as `native` had, empty ones included.
    fn pitcrew_env(&self, native: &str) -> std::collections::HashMap<String, String> {
        self.noted(native, "env")
            .lines()
            .filter_map(|line| line.split_once('='))
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect()
    }

    /// This test's processes still running.
    fn running(&self) -> Vec<(u32, String)> {
        marked(&self.mark)
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while !self.running().is_empty() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        for (pid, _) in self.running() {
            let _ = std::process::Command::new("kill")
                .args(["-KILL", &pid.to_string()])
                .status();
        }
    }
}

fn private_folder(dir: &Path) {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::create_dir_all(dir).unwrap();
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).unwrap();
}

/// Live processes other than this one carrying `MARK=<mark>` (Linux: `/proc`; macOS: `ps -E`).
fn marked(mark: &str) -> Vec<(u32, String)> {
    let want = format!("{MARK}={mark}");
    let me = std::process::id();
    #[cfg(target_os = "linux")]
    {
        let Ok(entries) = std::fs::read_dir("/proc") else {
            return Vec::new();
        };
        entries
            .filter_map(|entry| {
                let pid: u32 = entry.ok()?.file_name().to_str()?.parse().ok()?;
                if pid == me {
                    return None;
                }
                // A zombie has no environment.
                let env = std::fs::read(format!("/proc/{pid}/environ")).ok()?;
                if !env.split(|b| *b == 0).any(|v| v == want.as_bytes()) {
                    return None;
                }
                let cmd = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
                Some((pid, String::from_utf8_lossy(&cmd).replace('\0', " ")))
            })
            .collect()
    }
    #[cfg(not(target_os = "linux"))]
    {
        let out = std::process::Command::new("ps")
            .args(["-axE", "-o", "pid=,command="])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default();
        out.lines()
            .filter(|line| line.split_whitespace().any(|word| word == want))
            .filter_map(|line| {
                let (pid, rest) = line.trim().split_once(' ')?;
                let pid: u32 = pid.parse().ok()?;
                (pid != me).then(|| (pid, rest.to_owned()))
            })
            .collect()
    }
}

fn eventually(what: &str, mut holds: impl FnMut() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !holds() {
        assert!(Instant::now() < deadline, "never: {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn ok(reply: &common::Reply, status: u16, what: &str) -> Value {
    assert_eq!(reply.status, status, "{what}: {}", reply.body);
    if reply.body.is_empty() {
        Value::Null
    } else {
        reply.json()
    }
}

/// A project rooted in `folder` on the demo's laptop (the runner's machine), with a task in it;
/// the task.
fn task_in(daemon: &Daemon, device: &str, key: &str, folder: &Path) -> Value {
    let project = ok(
        &daemon.post(
            "/v1/projects",
            Some(device),
            &json!({
                "key": key,
                "name": format!("Dispatch {key}"),
                "root": { "machine": id::LAPTOP, "path": folder.to_str().unwrap() },
            }),
        ),
        201,
        "project",
    );
    ok(
        &daemon.post(
            "/v1/tasks",
            Some(device),
            &json!({ "project": project["id"], "title": "Submit the seeds" }),
        ),
        201,
        "task",
    )
}

fn task(daemon: &Daemon, device: &str, id: &str) -> Value {
    ok(
        &daemon.get(&format!("/v1/tasks/{id}"), Some(device)),
        200,
        "task",
    )
}

fn session(daemon: &Daemon, device: &str, id: &str) -> Value {
    ok(
        &daemon.get(&format!("/v1/sessions/{id}"), Some(device)),
        200,
        "session",
    )
}

/// The dispatch's events, oldest first.
fn dispatch_events(daemon: &Daemon, device: &str, task: &str) -> Vec<Value> {
    daemon
        .events_matching(&format!("&task={task}"), device)
        .into_iter()
        .filter(|e| {
            e["body"]["type"]
                .as_str()
                .is_some_and(|t| t.starts_with("dispatch_"))
        })
        .collect()
}

/// A hook the agent's CLI sends: the path and the body, for the session whose CLI id is given.
type Hook = (&'static str, fn(&str) -> Value);

/// Dispatches `agent` on a new task in `folder` and follows it to review, as described in the
/// [module docs](self). `idle` is a hook of the CLI's that makes its session idle; `working`, one
/// that makes it work, if the CLI has one.
fn dispatch_and_follow(
    rig: &Rig,
    daemon: &Daemon,
    key: &str,
    agent: &str,
    folder: &Path,
    idle: Hook,
    working: Option<Hook>,
) {
    let device = daemon.device_token();
    let t = task_in(daemon, &device, key, folder);
    let task_id = t["id"].as_str().unwrap().to_owned();
    assert_eq!(t["status"], "todo");

    let dispatch = ok(
        &daemon.post(
            &format!("/v1/tasks/{task_id}/dispatch"),
            Some(&device),
            &json!({ "agent": agent }),
        ),
        202,
        "dispatch",
    );
    let named = dispatch["session"].as_str().unwrap().to_owned();
    let stored = session(daemon, &device, &named);
    assert_eq!(stored["state"], "starting");
    assert_eq!(stored["agent"], agent);

    // The runner reports the CLI's transcript under the dispatch's session: once, linked to the
    // task, with its terminal.
    eventually("the runner reports the dispatched session", || {
        session(daemon, &device, &named)["native_id"]
            .as_str()
            .is_some_and(|n| !n.is_empty())
    });
    let s = session(daemon, &device, &named);
    let native = s["native_id"].as_str().unwrap().to_owned();
    assert_eq!(s["agent"], agent);
    assert_eq!(s["task"], task_id.as_str());
    assert_eq!(s["link_basis"], "dispatch");
    assert_eq!(s["machine"], id::LAPTOP);
    assert!(s["terminal"].is_string(), "{s}");
    assert_eq!(rig.noted(&native, "cwd").trim(), folder.to_str().unwrap());
    let all = ok(&daemon.get("/v1/sessions", Some(&device)), 200, "sessions");
    let same: Vec<&Value> = all
        .as_array()
        .unwrap()
        .iter()
        .filter(|x| x["native_id"] == native.as_str())
        .collect();
    assert_eq!(same.len(), 1, "one session for the CLI: {same:?}");
    assert_eq!(same[0]["id"], named.as_str());

    // The CLI was given an agent token's file for the dispatched agent, never the person's, and
    // where the daemon listens; no token in its environment.
    assert!(!rig.token_in_env(&native));
    assert_eq!(rig.noted(&native, "url"), daemon.at);
    // Though the daemon was started with the person's token and another socket (passed on by its
    // terminals), the CLI reads the agent's token file and the daemon's address: the two are set
    // empty for it, which the CLI reads as unset.
    let vars = rig.pitcrew_env(&native);
    for name in ["PITCREW_TOKEN", "PITCREW_SOCKET", "PITCREW_PIPE"] {
        assert_eq!(vars.get(name).map(String::as_str), Some(""), "{name}");
    }
    let lookup = |name: &str| vars.get(name).map(OsString::from);
    let read = pitcrew_cli::config::token_from_env(&lookup)
        .unwrap_or_else(|e| panic!("the CLI finds no token: {:?}", e.kind));
    match pitcrew_cli::config::Endpoint::from_env(&lookup) {
        Ok(pitcrew_cli::config::Endpoint::Tcp { host, .. }) => {
            assert_eq!(format!("http://{host}"), daemon.at);
        }
        other => panic!("the CLI reaches another endpoint: {other:?}"),
    }
    let file = PathBuf::from(rig.noted(&native, "token-file"));
    assert_eq!(
        file,
        daemon.state.join("agents").join(format!("{agent}.token"))
    );
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&file).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "{}", file.display());
    }
    let token = common::read_token(&file);
    assert!(token != device, "never the person's token");
    assert!(
        read == token,
        "the CLI authenticates with the agent's token file, not an inherited token"
    );
    let me = ok(&daemon.get("/v1/me", Some(&token)), 200, "me");
    assert_eq!(me["id"], agent);
    assert_eq!(me["owner"], id::SAM);
    // An agent's token: it may not dispatch, which only a person may.
    ok(
        &daemon.post(
            &format!("/v1/tasks/{task_id}/dispatch"),
            Some(&token),
            &json!({ "agent": agent }),
        ),
        403,
        "an agent dispatching",
    );

    // Its first working moves the task to in progress: the transcript's first prompt, or the
    // CLI's hook. The agent's own hooks change the session.
    let hook = |(path, body): Hook| {
        let reply = daemon.post(path, Some(&token), &body(&native));
        assert_eq!(reply.status, 202, "{path}: {}", reply.body);
    };
    let worked = session(daemon, &device, &named)["state"] == "working";
    hook(idle);
    eventually("the agent's hook makes its session idle", || {
        session(daemon, &device, &named)["state"] == "idle"
    });
    if worked {
        assert_eq!(task(daemon, &device, &task_id)["status"], "in_progress");
    } else {
        assert_eq!(task(daemon, &device, &task_id)["status"], "todo");
    }
    if let Some(working) = working {
        hook(working);
        eventually("the agent's hook makes its session work", || {
            session(daemon, &device, &named)["state"] == "working"
        });
    }
    eventually("the task moves to in progress", || {
        task(daemon, &device, &task_id)["status"] == "in_progress"
    });

    // The agent reports it done (`pitcrew report <task> --review`): review, and the dispatch
    // succeeded.
    let moved = ok(
        &daemon.post(
            &format!("/v1/tasks/{task_id}/move"),
            Some(&token),
            &json!({ "to": "review" }),
        ),
        200,
        "the agent's report",
    );
    assert_eq!(moved["status"], "review");
    let events = dispatch_events(daemon, &device, &task_id);
    let types: Vec<&str> = events
        .iter()
        .map(|e| e["body"]["type"].as_str().unwrap())
        .collect();
    assert_eq!(
        types,
        ["dispatch_started", "dispatch_finished"],
        "{events:?}"
    );
    assert_eq!(events[1]["body"]["data"]["outcome"], "succeeded");
    assert_eq!(events[1]["author"], agent);
    assert_eq!(events[1]["on_behalf_of"], id::SAM);

    // Ended by the person: the session ends; the dispatch stays succeeded.
    ok(
        &daemon.post(
            &format!("/v1/sessions/{named}/end"),
            Some(&device),
            &json!({ "mode": "kill" }),
        ),
        204,
        "end",
    );
    eventually("the session ends", || {
        session(daemon, &device, &named)["state"] == "ended"
    });
    assert_eq!(dispatch_events(daemon, &device, &task_id).len(), 2);
}

/// Claude and Codex dispatched end to end: see the [module docs](self).
#[test]
fn a_dispatched_agent_runs_as_its_session_and_moves_its_task() {
    let Some(rig) = Rig::new() else {
        return;
    };
    // The demo, set up; then the daemon again, started from a shell that exports the person's
    // token and another daemon's socket, which its terminals pass on to every CLI.
    let mut daemon = rig.start(true);
    let device = daemon.device_token();
    daemon.stop();
    let elsewhere = rig.tmp.path().join("elsewhere");
    private_folder(&elsewhere);
    let mut daemon = rig.start_with(
        false,
        &[
            ("PITCREW_TOKEN", OsString::from(&device)),
            ("PITCREW_SOCKET", elsewhere.into_os_string()),
        ],
    );
    let info = daemon.get("/v1/host/info", None).json();
    assert_eq!(
        info["capabilities"],
        json!(["pty", "watch", "scan"]),
        "{info}"
    );

    // Claude (@writer): its `Stop` and `UserPromptSubmit` hooks.
    dispatch_and_follow(
        &rig,
        &daemon,
        "CLD",
        id::WRITER,
        &rig.folder("claude"),
        (
            "/v1/hooks/claude/Stop",
            |native| json!({ "session_id": native, "hook_event_name": "Stop" }),
        ),
        Some((
            "/v1/hooks/claude/UserPromptSubmit",
            |native| json!({ "session_id": native, "hook_event_name": "UserPromptSubmit" }),
        )),
    );
    // Codex (@runner): its rollout's first prompt is its first working; its notify hook says a
    // turn is complete.
    dispatch_and_follow(
        &rig,
        &daemon,
        "CDX",
        RUNNER,
        &rig.folder("codex"),
        (
            "/v1/hooks/codex/notify",
            |native| json!({ "type": "agent-turn-complete", "thread-id": native }),
        ),
        None,
    );

    daemon.stop();
    eventually("nothing of the test's runs", || rig.running().is_empty());
}

/// A crash between a dispatch and its start, and one after the CLI started: at the next start, the
/// session whose CLI never started is abandoned (its dispatch fails, it ends), and the one whose
/// CLI runs is kept, and reported under its id once its transcript appears.
#[test]
fn dispatches_a_crash_left_are_reconciled_at_start() {
    use pitcrew_protocol::events::EventBody;
    use pitcrew_protocol::ids::{DispatchId, SessionId};
    use pitcrew_protocol::model::{Dispatch, Engine, LinkBasis, Session, SessionState};

    let Some(rig) = Rig::new() else {
        return;
    };
    let mut daemon = rig.start(true);
    let device = daemon.device_token();
    let folder = rig.folder("held");

    // A dispatch whose CLI starts, its transcript held back...
    std::fs::write(rig.out.join("hold"), "").unwrap();
    let t = task_in(&daemon, &device, "HLD", &folder);
    let task_id = t["id"].as_str().unwrap().to_owned();
    let dispatch = ok(
        &daemon.post(
            &format!("/v1/tasks/{task_id}/dispatch"),
            Some(&device),
            &json!({ "agent": id::WRITER }),
        ),
        202,
        "dispatch",
    );
    let running = dispatch["session"].as_str().unwrap().to_owned();
    let mut natives = Vec::new();
    eventually("the stand-in starts", || {
        natives = std::fs::read_dir(&rig.out)
            .unwrap()
            .filter_map(|e| {
                let e = e.ok()?;
                e.file_type().ok()?.is_dir().then(|| e.file_name())
            })
            .collect();
        !natives.is_empty()
    });
    let native = natives[0].to_str().unwrap().to_owned();
    // ...then the daemon crashes. Its terminal runs on in ptyd.
    daemon.signal("KILL");

    // A crash between the append and the start left this: an open dispatch and a `starting`
    // session, with no CLI.
    let (lost, lost_session) = (DispatchId::new(), SessionId::new());
    let now = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap();
    common::append_to_store(
        &daemon.state,
        id::SAM,
        None,
        vec![
            EventBody::DispatchStarted {
                dispatch: Dispatch {
                    id: lost,
                    task: id::PAP2.parse().unwrap(),
                    agent: id::REVIEWER.parse().unwrap(),
                    session: Some(lost_session),
                    brief: "Check the submission".into(),
                    started: now,
                    ended: None,
                    outcome: None,
                    summary: None,
                },
            },
            EventBody::SessionDiscovered {
                session: Session {
                    id: lost_session,
                    engine: Engine::Claude,
                    native_id: String::new(),
                    machine: id::LAPTOP.parse().unwrap(),
                    cwd: folder.to_str().unwrap().to_owned(),
                    branch: None,
                    title: None,
                    agent: Some(id::REVIEWER.parse().unwrap()),
                    workstream: None,
                    task: Some(id::PAP2.parse().unwrap()),
                    link_basis: Some(LinkBasis::Dispatch),
                    state: SessionState::Starting,
                    status_line: None,
                    started: now,
                    last_activity: now,
                    terminal: None,
                    parent: None,
                    recorded: None,
                },
            },
        ],
    );

    let mut daemon = rig.start(false);
    let lost_id = lost_session.0.to_string();
    eventually("the session whose CLI never started ends", || {
        session(&daemon, &device, &lost_id)["state"] == "ended"
    });
    let failed: Vec<Value> = dispatch_events(&daemon, &device, id::PAP2)
        .into_iter()
        .filter(|e| e["body"]["type"] == "dispatch_finished")
        .collect();
    assert_eq!(failed.len(), 1, "{failed:?}");
    assert_eq!(failed[0]["body"]["data"]["dispatch"], lost.0.to_string());
    assert_eq!(failed[0]["body"]["data"]["outcome"], "failed");
    assert!(
        failed[0]["body"]["data"]["summary"]
            .as_str()
            .is_some_and(|s| s.contains("did not start")),
        "{failed:?}"
    );

    // The one whose CLI runs is kept, and is its transcript's once that appears.
    assert_eq!(session(&daemon, &device, &running)["state"], "starting");
    std::fs::remove_file(rig.out.join("hold")).unwrap();
    eventually(
        "the running CLI is reported under the dispatch's session",
        || session(&daemon, &device, &running)["native_id"] == native.as_str(),
    );
    assert_eq!(
        session(&daemon, &device, &running)["link_basis"],
        "dispatch"
    );
    // Ended without a report: the dispatch stopped work.
    ok(
        &daemon.post(
            &format!("/v1/sessions/{running}/end"),
            Some(&device),
            &json!({ "mode": "kill" }),
        ),
        204,
        "end",
    );
    eventually("the dispatch is over", || {
        dispatch_events(&daemon, &device, &task_id).len() == 2
    });
    let over = dispatch_events(&daemon, &device, &task_id);
    assert_eq!(over[1]["body"]["data"]["outcome"], "canceled", "{over:?}");

    daemon.stop();
    eventually("nothing of the test's runs", || rig.running().is_empty());
}
