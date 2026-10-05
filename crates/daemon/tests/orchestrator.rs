//! The Orchestrator, end to end: the real binary with a seeded demo, its terminals in the
//! pitcrew-ptyd cargo built next to it, and a stand-in `claude` first on its `PATH` that answers
//! the way the prompt asks: it reads the work **through the real `pitcrew` CLI** cargo built next
//! to `pitcrewd`, with the token file the daemon gave it, tries two writes, and writes its answer
//! in a transcript as Claude Code does, citing what it read.
//!
//! The test asks "What did my agents do today?" and checks that:
//! - the stand-in ran in the person's scratch folder with a **reader** token's file (no token in
//!   its environment); every read verb answered it, and both writes were refused (`pitcrew` exits
//!   3);
//! - the answer arrives from the transcript, with **working links**: each reference names a
//!   session and a task the hub serves; its suggestion is a suggestion (nothing moved);
//! - the reader token is refused (`403`) on **every write route of the contract**, every
//!   WebSocket, and the device-only reads, and answers the reads marked **read**;
//! - a follow-up is typed into the live session and answered there; clearing forgets the
//!   conversation and ends its session.
//!
//! Unix only (the stand-in is a shell script). pitcrew-ptyd and pitcrew must have been built next
//! to `pitcrewd` (`cargo test --workspace` builds both); if they are not, the test says so and
//! passes, unless `CI` or `PITCREW_REQUIRE_PTYD=1` is set. Everything it starts carries
//! `PITCREW_TEST_RUN=<mark>` and is killed at the end.

#![cfg(unix)]
#![allow(clippy::unwrap_used)]

mod common;

use common::{Daemon, Tmux, id, request};
use serde_json::{Value, json};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(60);
const MARK: &str = "PITCREW_TEST_RUN";
const QUESTION: &str = "What did my agents do today?";
const FOLLOW_UP: &str = "And which tasks moved?";

/// A stand-in for Claude Code started with the Orchestrator's prompt. It notes what it was given,
/// runs `pitcrew`'s read verbs and two writes, then writes its transcript (under the
/// `--session-id` it was given): the question, one tool run, and an answer citing the first
/// session `pitcrew session list` shows that is not its own, and PAP-1, with a suggestion. Each
/// line typed into its terminal afterwards is a follow-up, answered in the transcript too.
const FAKE_CLAUDE: &str = r#"#!/bin/sh
trap 'exit 0' INT TERM
id=
for arg in "$@"; do
  case "$arg" in --session-id=*) id=${arg#--session-id=} ;; esac
done
[ -n "$id" ] || { echo "no --session-id" >&2; exit 2; }
for prompt in "$@"; do :; done
out="@OUT@/$id"
mkdir -p "$out"
printf '%s' "$prompt" > "$out/prompt"
printf '%s' "${PITCREW_TOKEN_FILE:-}" > "$out/token-file"
if [ -n "${PITCREW_TOKEN:-}" ]; then : > "$out/token-in-env"; fi
pwd -P > "$out/cwd"
note() { name=$1; shift; "$PITCREW_TEST_CLI" "$@" > "$out/$name.out" 2> "$out/$name.err"; echo $? > "$out/$name.code"; }
note sessions session list
note today session list --since today --json
note show session show ses_@SES1@
note blocks recap blocks --limit 5
note days recap days --workstream wst_@SUBMISSION@
note activity activity --limit 5
note search search method
note tasks task list
note move task move PAP-1 done
note comment comment PAP-1 hello
first=$(awk '$1 ~ /^ses_/ && $NF != "Orchestrator" { print $1; exit }' "$out/sessions.out")
dir="${CLAUDE_CONFIG_DIR:?}/projects/-orchestrator"
mkdir -p "$dir"
file="$dir/$id.jsonl"
cwd=$(pwd -P)
now() { date -u +%Y-%m-%dT%H:%M:%S.000Z; }
meta() { printf '"sessionId":"%s","cwd":"%s","uuid":"%s","timestamp":"%s"' "$id" "$cwd" "$1" "$(now)"; }
user() { printf '{"type":"user",%s,"message":{"role":"user","content":"%s"}}\n' "$(meta "u$2")" "$1" >> "$file"; }
said() { printf '{"type":"assistant",%s,"message":{"role":"assistant","model":"stand-in","content":[{"type":"text","text":"%s"}],"stop_reason":"end_turn"}}\n' "$(meta "a$2")" "$1" >> "$file"; }
user '@QUESTION@' 1
printf '{"type":"assistant",%s,"message":{"role":"assistant","model":"stand-in","content":[{"type":"tool_use","id":"toolu_1","name":"Bash","input":{"command":"pitcrew session list"}}],"stop_reason":"tool_use"}}\n' "$(meta a1)" >> "$file"
printf '{"type":"user",%s,"message":{"role":"user","content":[{"tool_use_id":"toolu_1","type":"tool_result","content":"listed"}]}}\n' "$(meta u2)" >> "$file"
said "Today one session worked:\\n- $first moved PAP-1 on.\\n\\nSuggestion: move PAP-1 to review" 3
n=4
while IFS= read -r typed; do
  user "$typed" "$n"
  said "Follow-up answer: PAP-1 is in progress." "$((n + 1))"
  n=$((n + 2))
done
while :; do sleep 1; done
"#;

/// The binaries this test needs next to `pitcrewd`, or `None` (after saying why) when one is not
/// built; a failure instead under CI or with `PITCREW_REQUIRE_PTYD=1`.
fn built() -> Option<(PathBuf, PathBuf)> {
    let ptyd = Path::new(common::PITCREWD).with_file_name(pitcrew_runtime::pty::launch::PTYD);
    let cli = Path::new(common::PITCREWD).with_file_name("pitcrew");
    for path in [&ptyd, &cli] {
        if !path.is_file() {
            let required = std::env::var_os("CI").is_some_and(|v| !v.is_empty())
                || std::env::var("PITCREW_REQUIRE_PTYD").is_ok_and(|v| v == "1");
            assert!(
                !required,
                "{} is not built: test the workspace, or build it first with `cargo build -p \
                 pitcrew-ptyd -p pitcrew-cli`",
                path.display()
            );
            eprintln!("skipped: {} is not built", path.display());
            return None;
        }
    }
    Some((ptyd, cli))
}

fn private_folder(dir: &Path) {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::create_dir_all(dir).unwrap();
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).unwrap();
}

/// Live processes other than this one carrying `MARK=<mark>` (Linux: `/proc`; macOS: `ps -E`).
fn marked(mark: &str) -> Vec<u32> {
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
                let env = std::fs::read(format!("/proc/{pid}/environ")).ok()?;
                (pid != me && env.split(|b| *b == 0).any(|v| v == want.as_bytes())).then_some(pid)
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
            .filter_map(|line| line.trim().split_once(' ')?.0.parse::<u32>().ok())
            .filter(|pid| *pid != me)
            .collect()
    }
}

/// Kills what the test started, however it ends.
struct Cleanup(String);

impl Drop for Cleanup {
    fn drop(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while !marked(&self.0).is_empty() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        for pid in marked(&self.0) {
            let _ = std::process::Command::new("kill")
                .args(["-KILL", &pid.to_string()])
                .status();
        }
    }
}

fn eventually(what: &str, mut holds: impl FnMut() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !holds() {
        assert!(Instant::now() < deadline, "never: {what}");
        std::thread::sleep(Duration::from_millis(100));
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

/// Every route of the contract with a method that writes, its parameters filled in (the
/// reader's refusal comes before any route looks at them).
fn contract_writes() -> Vec<(String, String)> {
    let contract = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/build/contracts/api-v1.md"),
    )
    .unwrap();
    let mut routes: Vec<(String, String)> = Vec::new();
    for piece in contract.split('`') {
        let Some((method, path)) = piece.split_once(' ') else {
            continue;
        };
        if !["POST", "PUT", "PATCH", "DELETE"].contains(&method) || !path.starts_with("/v1/") {
            continue;
        }
        let path = path
            .split('?')
            .next()
            .unwrap()
            .replace("{project\\|workstream}", "workstream");
        let filled: Vec<String> = path
            .split('/')
            .map(|segment| match segment {
                "{engine}" => "claude".to_owned(),
                "{event}" => "Stop".to_owned(),
                "{scope}" => "workspace".to_owned(),
                s if s.starts_with('{') => "01J00000000000000000000000".to_owned(),
                s => s.to_owned(),
            })
            .collect();
        let route = (method.to_owned(), filled.join("/"));
        if !routes.contains(&route) {
            routes.push(route);
        }
    }
    routes
}

#[test]
fn a_stand_in_cli_answers_what_my_agents_did_today_with_working_links_and_cannot_write() {
    let Some((ptyd, cli)) = built() else {
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let (homes, out, bin) = (
        tmp.path().join("homes"),
        tmp.path().join("out"),
        tmp.path().join("bin"),
    );
    for dir in [&homes, &out, &bin] {
        private_folder(dir);
    }
    std::fs::create_dir_all(homes.join(".claude").join("projects")).unwrap();
    let script = FAKE_CLAUDE
        .replace("@OUT@", out.to_str().unwrap())
        .replace("@SES1@", id::SES1)
        .replace("@SUBMISSION@", id::SUBMISSION)
        .replace("@QUESTION@", QUESTION);
    std::fs::write(bin.join("claude"), script).unwrap();
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(bin.join("claude"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let mark = format!("orchestrator-{}-{nanos}", std::process::id());
    let _cleanup = Cleanup(mark.clone());
    let path = std::env::join_paths(std::iter::once(bin.clone()).chain(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    )))
    .unwrap();
    let endpoint = tmp.path().join("p");
    private_folder(&endpoint);
    let endpoint = endpoint.join("ptyd");
    let state = tmp.path().join("state");
    let mut daemon = Daemon::start_with(
        &state,
        &[
            "--terminal-runtime",
            "pty",
            "--ptyd",
            ptyd.to_str().unwrap(),
            "--ptyd-endpoint",
            endpoint.to_str().unwrap(),
            "--ptyd-idle-exit-ms",
            "500",
            "--homes",
            homes.to_str().unwrap(),
            "--demo",
        ],
        &[
            ("PATH", path),
            (MARK, OsString::from(&mark)),
            ("PITCREW_TEST_CLI", cli.into_os_string()),
            ("CLAUDE_CONFIG_DIR", homes.join(".claude").into_os_string()),
        ],
        Tmux::Refused,
    );
    let device = daemon.device_token();
    let orchestrator = |daemon: &Daemon| {
        ok(
            &daemon.get("/v1/orchestrator", Some(&device)),
            200,
            "the orchestrator",
        )
    };

    let before = orchestrator(&daemon);
    assert_eq!(
        before["engines"][0],
        json!({"engine": "claude", "installed": true})
    );
    assert_eq!(before["conversations"], json!([]));

    // The question: a session that only reads, in the person's scratch folder.
    let asked = ok(
        &daemon.post(
            "/v1/orchestrator/questions",
            Some(&device),
            &json!({"text": QUESTION}),
        ),
        202,
        "ask",
    );
    let conversation = asked["id"].as_str().unwrap().to_owned();
    let session = asked["turns"][0]["session"].as_str().unwrap().to_owned();
    assert_eq!(asked["turns"][0]["state"], "answering");
    let turn = |daemon: &Daemon, index: usize| -> Value {
        orchestrator(daemon)["conversations"][0]["turns"][index].clone()
    };
    eventually("the answer arrives", || {
        turn(&daemon, 0)["state"] != "answering"
    });
    let answered = turn(&daemon, 0);
    assert_eq!(answered["state"], "answered", "{answered}");

    let native = std::fs::read_dir(&out)
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .find(|p| p.join("prompt").is_file())
        .expect("the stand-in ran");
    let read = |name: &str| {
        let path = native.join(name);
        eventually(&format!("the stand-in notes {name}"), || path.is_file());
        std::fs::read_to_string(path).unwrap()
    };
    // Every read verb answered; both writes were refused.
    for verb in [
        "sessions", "today", "show", "blocks", "days", "activity", "search", "tasks",
    ] {
        assert_eq!(
            read(&format!("{verb}.code")).trim(),
            "0",
            "pitcrew {verb}: {}",
            read(&format!("{verb}.err"))
        );
    }
    for write in ["move", "comment"] {
        assert_eq!(
            read(&format!("{write}.code")).trim(),
            "3",
            "{write} is refused"
        );
        assert!(
            read(&format!("{write}.err")).contains("may only read"),
            "{}",
            read(&format!("{write}.err"))
        );
    }
    let sessions = read("sessions.out");
    assert!(
        sessions.contains(&format!("ses_{}", id::SES1)),
        "{sessions}"
    );
    let today: Value = serde_json::from_str(&read("today.out")).unwrap();
    assert!(
        today.as_array().unwrap().iter().any(|s| s["id"] == session),
        "its own session is today's"
    );
    assert!(read("show.out").starts_with(&format!("ses_{}  Draft method section", id::SES1)));
    let prompt = read("prompt");
    assert!(prompt.ends_with(&format!("The question:\n\n{QUESTION}\n")));
    assert!(prompt.contains("pitcrew session list"));
    assert!(
        !prompt.contains(&device),
        "the device token is never in a prompt"
    );
    assert!(
        !native.join("token-in-env").exists(),
        "no token in the environment"
    );
    let token_file = read("token-file");
    assert!(
        token_file.ends_with(&format!("{}.reader.token", id::OFFICE)),
        "the CLI was not given the back office's reader token file"
    );
    assert_eq!(
        PathBuf::from(read("cwd").trim()),
        state
            .join("scratch")
            .join(format!("orchestrator-{}", id::SAM))
            .canonicalize()
            .unwrap()
    );

    // The answer, with working links, and its suggestion only a suggestion.
    let answer = answered["answer"].as_str().unwrap();
    assert!(answer.contains(&format!("ses_{}", id::SES1)), "{answer}");
    assert!(!answer.contains("Suggestion:"), "{answer}");
    let references = answered["references"].as_array().unwrap();
    assert_eq!(references.len(), 2, "{answered}");
    for reference in references {
        let target = &reference["target"];
        let path = match target["kind"].as_str().unwrap() {
            "session" => format!("/v1/sessions/{}", target["id"].as_str().unwrap()),
            "task" => format!("/v1/tasks/{}", target["key"].as_str().unwrap()),
            other => panic!("an unexpected reference: {other}"),
        };
        ok(&daemon.get(&path, Some(&device)), 200, "a link works");
    }
    assert_eq!(
        references[0]["target"],
        json!({"kind": "session", "id": id::SES1})
    );
    assert_eq!(
        answered["suggestions"],
        json!([{"kind": "move_task", "task": id::PAP1, "key": "PAP-1", "to": "review",
                "label": "Move PAP-1 to review"}])
    );
    assert_eq!(answered["usage"]["tool_runs"], 1);
    let pap1 = ok(&daemon.get("/v1/tasks/PAP-1", Some(&device)), 200, "PAP-1");
    assert_eq!(pap1["status"], "in_progress", "a suggestion moves nothing");

    // The reader token: refused on every write of the contract, on WebSockets and the
    // device-only reads; it makes the reads marked **read**. Read here, never printed.
    let reader = common::read_token(Path::new(&token_file));
    assert!(reader.starts_with("pcr_"), "a reader token");
    let writes = contract_writes();
    assert!(writes.len() > 30, "the contract's writes: {}", writes.len());
    for (method, path) in &writes {
        let reply = request(
            daemon.port,
            method,
            path,
            Some(&reader),
            Some(&json!({})),
            &[],
        );
        assert_eq!(reply.status, 403, "{method} {path}: {}", reply.body);
    }
    for path in [
        "/v1/me/cursors",
        "/v1/safety",
        "/v1/import",
        "/v1/orchestrator",
        "/v1/board-drafts",
        &format!("/v1/sessions/{}/transcript", id::SES1),
        &format!("/v1/workstreams/{}/files?loc=0&path=", id::SUBMISSION),
        &format!("/v1/workstreams/{}/board-draft", id::SUBMISSION),
        "/v1/stream",
        // Integrations, outward writes and machine setup: device-only reads.
        "/v1/integrations",
        "/v1/writes",
        &format!("/v1/machines/{}/check", id::LAPTOP),
        &format!("/v1/machines/{}/agents", id::LAPTOP),
        &format!("/v1/machines/{}/agents/claude/sign-in", id::LAPTOP),
    ] {
        let reply = daemon.get(path, Some(&reader));
        assert_eq!(reply.status, 403, "GET {path}: {}", reply.body);
    }
    for path in ["/v1/stream", &format!("/v1/sessions/{}/terminal", id::SES1)] {
        let reply = request(
            daemon.port,
            "GET",
            path,
            Some(&reader),
            None,
            &[
                ("Upgrade", "websocket"),
                ("Connection", "Upgrade"),
                ("Sec-WebSocket-Version", "13"),
                ("Sec-WebSocket-Key", "dGhlIHNhbXBsZSBub25jZQ=="),
            ],
        );
        assert_eq!(reply.status, 403, "a socket for {path}");
    }
    for path in [
        "/v1/sessions",
        "/v1/projects",
        "/v1/workstreams",
        "/v1/tasks",
        "/v1/events?limit=5",
        "/v1/recaps/blocks?limit=5",
        &format!("/v1/recaps/days?workstream={}", id::SUBMISSION),
        "/v1/me",
    ] {
        ok(&daemon.get(path, Some(&reader)), 200, path);
    }
    let pap1 = ok(&daemon.get("/v1/tasks/PAP-1", Some(&device)), 200, "PAP-1");
    assert_eq!(pap1["status"], "in_progress", "the reader changed nothing");

    // A follow-up is typed into the live session, and answered there.
    let follow = ok(
        &daemon.post(
            "/v1/orchestrator/questions",
            Some(&device),
            &json!({"text": FOLLOW_UP, "conversation": conversation}),
        ),
        202,
        "follow up",
    );
    assert_eq!(follow["turns"][1]["session"], session);
    eventually("the follow-up is answered", || {
        turn(&daemon, 1)["state"] == "answered"
    });
    assert_eq!(
        turn(&daemon, 1)["answer"],
        "Follow-up answer: PAP-1 is in progress."
    );

    // Clearing forgets the conversation and ends its session.
    let cleared = request(
        daemon.port,
        "DELETE",
        "/v1/orchestrator/conversations",
        Some(&device),
        None,
        &[],
    );
    assert_eq!(cleared.status, 204, "{}", cleared.body);
    assert_eq!(orchestrator(&daemon)["conversations"], json!([]));
    eventually("its session ends", || {
        daemon
            .get(&format!("/v1/sessions/{session}"), Some(&device))
            .json()["state"]
            == "ended"
    });
    let saved = std::fs::read_to_string(state.join("orchestrator.json")).unwrap();
    assert!(!saved.contains(QUESTION), "a clear forgets the questions");
    daemon.stop();
}
