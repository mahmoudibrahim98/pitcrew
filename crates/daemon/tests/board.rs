//! A board draft, end to end: the real binary with a seeded demo, its terminals in the
//! pitcrew-ptyd cargo built next to it, and a stand-in `claude` first on its `PATH` that answers
//! the draft the way the prompt asks, **through the real `pitcrew` CLI** cargo built next to
//! `pitcrewd` (`pitcrew board submit <draft>`, with the agent's token file the daemon gave it).
//!
//! The test previews a workstream's draft, starts it with the preview's digest, and checks that:
//! - the stand-in was started as the drafting agent with the prompt the preview measured, naming
//!   its draft and the workstream's session, with an agent token's file and no token in its
//!   environment;
//! - its proposal arrives (`pitcrew` exits 0), a second one is refused (`pitcrew` exits 4), and
//!   **no task exists** until the person reviews it;
//! - the review creates the accepted task only, labelled `drafted`, and links its evidence session.
//!
//! Unix only (the stand-in is a shell script). pitcrew-ptyd and pitcrew must have been built next
//! to `pitcrewd` (`cargo test --workspace` builds both); if they are not, the test says so and
//! passes, unless `CI` or `PITCREW_REQUIRE_PTYD=1` is set. Everything it starts carries
//! `PITCREW_TEST_RUN=<mark>` and is killed at the end.

#![cfg(unix)]
#![allow(clippy::unwrap_used)]

mod common;

use common::{Daemon, Tmux, id};
use serde_json::{Value, json};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(60);
const MARK: &str = "PITCREW_TEST_RUN";
/// The demo's session on the gpu box, linked to nothing: the draft's evidence.
const SES5: &str = "01JB000000000000000SES0005";

/// A stand-in for Claude Code started with a draft's prompt: it notes what it was given, then
/// submits a proposal twice with the real `pitcrew` (`$PITCREW_TEST_CLI`), citing the first
/// session the summary names, and waits to be ended.
const FAKE_CLAUDE: &str = r#"#!/bin/sh
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
draft=$(printf '%s\n' "$prompt" | sed -n 's/^pitcrew board submit \(drf_[0-9A-Z]*\).*/\1/p' | head -n 1)
evidence=$(printf '%s\n' "$prompt" | sed -n 's/^Session \([0-9A-Z]\{26\}\)$/\1/p' | head -n 1)
proposal="{\"tasks\": [{\"title\": \"Finish the synthetic paper\", \"status\": \"in_progress\", \"evidence\": [\"$evidence\"]}, {\"title\": \"A synthetic idea\", \"status\": \"backlog\"}], \"note\": \"Drafted by the stand-in.\"}"
printf '%s' "$proposal" | "$PITCREW_TEST_CLI" board submit "$draft" > "$out/submit.out" 2> "$out/submit.err"
echo $? > "$out/submit.part" && mv "$out/submit.part" "$out/submit.code"
printf '%s' "$proposal" | "$PITCREW_TEST_CLI" board submit "$draft" > /dev/null 2> "$out/again.err"
echo $? > "$out/again.part" && mv "$out/again.part" "$out/again.code"
trap 'exit 0' INT TERM
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

#[test]
fn a_stand_in_agent_drafts_a_board_through_pitcrew_and_only_accepted_tasks_are_made() {
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
    let script = FAKE_CLAUDE.replace("@OUT@", out.to_str().unwrap());
    std::fs::write(bin.join("claude"), script).unwrap();
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(bin.join("claude"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
    }
    let paper = tmp.path().join("work").join("paper");
    private_folder(&paper);
    let paper = paper.canonicalize().unwrap();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let mark = format!("board-{}-{nanos}", std::process::id());
    let _cleanup = Cleanup(mark.clone());

    let path = std::env::join_paths(std::iter::once(bin.clone()).chain(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    )))
    .unwrap();
    let endpoint = tmp.path().join("p");
    private_folder(&endpoint);
    let endpoint = endpoint.join("ptyd");
    let mut daemon = Daemon::start_with(
        &tmp.path().join("state"),
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

    // A workstream in the test's folder on the runner's machine, with one session of history.
    let project = ok(
        &daemon.post(
            "/v1/projects",
            Some(&device),
            &json!({"key": "DRB", "name": "Drafted paper",
                    "root": {"machine": id::LAPTOP, "path": paper.to_str().unwrap()}}),
        ),
        201,
        "project",
    );
    let workstream = ok(
        &daemon.post(
            "/v1/workstreams",
            Some(&device),
            &json!({"project": project["id"], "name": "Paper",
                    "locations": [{"machine": id::LAPTOP, "path": paper.to_str().unwrap()}]}),
        ),
        201,
        "workstream",
    );
    let ws = workstream["id"].as_str().unwrap().to_owned();
    ok(
        &daemon.post(
            &format!("/v1/sessions/{SES5}/link"),
            Some(&device),
            &json!({"workstream": ws}),
        ),
        200,
        "link",
    );
    let tasks_of = |daemon: &Daemon| -> Vec<Value> {
        ok(
            &daemon.get(&format!("/v1/tasks?workstream={ws}"), Some(&device)),
            200,
            "tasks",
        )
        .as_array()
        .unwrap()
        .clone()
    };

    // Preview, then start with what it showed.
    let shown = ok(
        &daemon.get(&format!("/v1/workstreams/{ws}/board-draft"), Some(&device)),
        200,
        "preview",
    );
    assert_eq!(shown["cost"]["sessions"], 1);
    assert!(shown["summary"].as_str().unwrap().contains(SES5));
    let draft = ok(
        &daemon.post(
            &format!("/v1/workstreams/{ws}/board-drafts"),
            Some(&device),
            &json!({"agent": id::WRITER, "digest": shown["digest"]}),
        ),
        202,
        "start",
    );
    let draft_id = draft["id"].as_str().unwrap().to_owned();
    assert_eq!(draft["state"], "running");

    // The stand-in answers through pitcrew; the proposal arrives, and creates nothing.
    let current = || {
        ok(
            &daemon.get(&format!("/v1/board-drafts/{draft_id}"), Some(&device)),
            200,
            "draft",
        )
    };
    eventually("the proposal arrives", || current()["state"] == "proposed");
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
    assert_eq!(
        read("submit.code").trim(),
        "0",
        "pitcrew: {}",
        read("submit.err")
    );
    assert_eq!(
        read("submit.out"),
        format!(
            "Proposed 2 tasks for drf_{draft_id}. Nothing is created until a person reviews it.\n"
        )
    );
    assert_eq!(
        read("again.code").trim(),
        "4",
        "a second proposal is a conflict"
    );
    assert!(read("again.err").contains("already has a proposal"));
    let prompt = read("prompt");
    assert!(prompt.contains(&format!("pitcrew board submit drf_{draft_id}")));
    assert!(prompt.contains(&format!("Session {SES5}")));
    assert_eq!(
        prompt.len() as u64,
        shown["cost"]["prompt_bytes"].as_u64().unwrap()
    );
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
        token_file.ends_with(&format!("{}.token", id::WRITER)),
        "the CLI was not given the drafting agent's own token file"
    );

    let proposed = current();
    assert_eq!(proposed["proposal"]["tasks"][0]["evidence"], json!([SES5]));
    assert!(tasks_of(&daemon).is_empty(), "a proposal creates no task");

    // The person accepts the first; the second creates nothing.
    let reviewed = ok(
        &daemon.post(
            &format!("/v1/board-drafts/{draft_id}/review"),
            Some(&device),
            &json!({"accept": [0]}),
        ),
        200,
        "review",
    );
    assert_eq!(reviewed["draft"]["rejected"], json!([1]));
    let made = tasks_of(&daemon);
    assert_eq!(made.len(), 1);
    assert_eq!(made[0]["title"], "Finish the synthetic paper");
    assert_eq!(made[0]["status"], "in_progress");
    assert_eq!(made[0]["labels"], json!(["drafted"]));
    assert_eq!(made[0]["key"], "DRB-1");
    let ses5 = ok(
        &daemon.get(&format!("/v1/sessions/{SES5}"), Some(&device)),
        200,
        "session",
    );
    assert_eq!(ses5["task"], made[0]["id"]);

    // The drafting session ends with the test.
    let session = draft["session"].as_str().unwrap();
    let ended = daemon.post(
        &format!("/v1/sessions/{session}/end"),
        Some(&device),
        &json!({"mode": "kill"}),
    );
    assert!(matches!(ended.status, 204 | 409), "{}", ended.body);
    daemon.stop();
}
