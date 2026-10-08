//! Machine setup end to end (api-v1.md, "Machine setup"): the real binary, temporary homes, and
//! **stand-ins for every tool it may run**, first on its `PATH`:
//!
//! - `claude`, `codex` and `opencode` answer `--version` and their status commands, and the
//!   stand-in `claude`'s login asks for a code and, given the right one, marks itself signed in
//!   (a file in the test's folder, which only the stand-in reads). **No real agent CLI runs.**
//! - `gh` and SLURM's tools answer `--version`.
//! - **Tripwires**: package managers, `sudo`, `curl` and the like, each of which notes that it ran.
//!   The test ends by checking that none did: no check, account or sign-in installs anything.
//!
//! The rest of `PATH` is the system's own folders (`/usr/bin:/bin`), for `sh` and its tools. The
//! sign-in terminal runs in tmux on a private socket of the test's own (skipped, with a message,
//! where tmux 3.2 or newer is not installed, unless `PITCREW_REQUIRE_TMUX=1`).
//!
//! A second person's device token (a member who did not set the hub up) is provisioned in the
//! state directory before the daemon starts, as the conformance suite's is: every route, and a
//! sign-in's terminal, refuses it.

#![cfg(unix)]
#![allow(clippy::unwrap_used)]

mod common;

use common::{Daemon, Frame, Reply, Tmux, Ws, id, request};
use serde_json::{Value, json};
use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const WAIT: Duration = Duration::from_secs(30);

/// What a package manager or a downloader would be called, on any system.
const TRIPWIRES: &[&str] = &[
    "apt", "apt-get", "aptitude", "dpkg", "brew", "port", "dnf", "yum", "rpm", "zypper", "pacman",
    "apk", "snap", "flatpak", "nix-env", "npm", "npx", "pnpm", "yarn", "bun", "pip", "pip3",
    "pipx", "uv", "cargo", "gem", "go", "sudo", "su", "doas", "curl", "wget", "winget", "choco",
    "scoop",
];

/// The stand-in Claude Code: its version, its status (signed in once its login took the code),
/// and its login.
const CLAUDE: &str = r#"#!/bin/sh
marker='@DIR@/claude-signed-in'
case "$1 $2" in
  "--version ") echo "2.1.3 (Claude Code)"; exit 0 ;;
  "auth status")
    if [ -f "$marker" ]; then echo '{"loggedIn":true,"email":"sam@example.com"}'; exit 0; fi
    echo '{"loggedIn":false}'; exit 1 ;;
  "auth login")
    echo "STAND-IN LOGIN: paste the code"
    read -r code
    [ "$code" = "synthetic-code" ] || { echo "WRONG CODE"; exit 1; }
    : > "$marker"
    echo "STAND-IN SIGNED IN"
    exit 0 ;;
esac
echo "unexpected: $*" >&2
exit 9
"#;

const CODEX: &str = r#"#!/bin/sh
case "$1 $2" in
  "--version ") echo "codex-cli 0.50.0"; exit 0 ;;
  "login status") echo "Logged in using an API key - sk-proj-***SYNTHETIC" >&2; exit 0 ;;
esac
exit 9
"#;

/// An older Claude Code, without the `auth` command: it reads `auth status` (or `auth login`) as a
/// prompt for an agent, which with no terminal fails.
const OLD_CLAUDE: &str = r#"#!/bin/sh
case "$1" in
  --version) echo "0.2.9 (Claude Code)"; exit 0 ;;
esac
echo "Error: Raw mode is not supported on the current process.stdin" >&2
exit 1
"#;

const OPENCODE: &str = r#"#!/bin/sh
case "$1 $2" in
  "--version ") echo "1.0.25"; exit 0 ;;
  "auth list") printf '┌  Credentials ~/.local/share/opencode/auth.json\n│\n└  0 credentials\n'; exit 0 ;;
esac
exit 9
"#;

/// The test's folder: stand-ins, tripwires, homes.
struct Rig {
    tmp: tempfile::TempDir,
    bin: PathBuf,
    homes: PathBuf,
    tripped: PathBuf,
}

impl Rig {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tmp.path().join("bin");
        let homes = tmp.path().join("homes");
        for dir in [&bin, &homes] {
            std::fs::create_dir_all(dir).unwrap();
        }
        std::fs::create_dir_all(homes.join(".claude/projects")).unwrap();
        let tripped = tmp.path().join("tripped");
        let dir = tmp.path().to_str().unwrap().to_owned();
        let write = |name: &str, body: &str| {
            let path = bin.join(name);
            std::fs::write(&path, body.replace("@DIR@", &dir)).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        };
        write("claude", CLAUDE);
        write("codex", CODEX);
        write("opencode", OPENCODE);
        write("gh", "#!/bin/sh\necho 'gh version 2.45.0 (2024-03-04)'\n");
        write("sbatch", "#!/bin/sh\necho 'slurm 23.02.7'\n");
        write("squeue", "#!/bin/sh\nexit 0\n");
        write("scancel", "#!/bin/sh\nexit 0\n");
        for name in TRIPWIRES {
            write(
                name,
                &format!(
                    "#!/bin/sh\necho '{name} '\"$*\" >> '{}'\nexit 1\n",
                    tripped.display()
                ),
            );
        }
        Self {
            tmp,
            bin,
            homes,
            tripped,
        }
    }

    fn path(&self) -> OsString {
        let mut path = OsString::from(&self.bin);
        path.push(":/usr/bin:/bin");
        path
    }

    fn state(&self, name: &str) -> PathBuf {
        self.tmp.path().join(name)
    }

    /// A demo daemon on `state` with the stand-ins first on its `PATH`, its terminals on `tmux`.
    fn start(&self, state: &Path, tmux: Tmux<'_>, env: &[(&str, OsString)]) -> Daemon {
        let mut all = vec![("PATH", self.path())];
        all.extend(env.iter().cloned());
        Daemon::start_with(
            state,
            &["--demo", "--homes", self.homes.to_str().unwrap()],
            &all,
            tmux,
        )
    }

    /// The daemon on `state` again, without `--demo` (it refuses the store it seeded).
    fn restart(&self, state: &Path, tmux: Tmux<'_>) -> Daemon {
        Daemon::start_with(
            state,
            &["--homes", self.homes.to_str().unwrap()],
            &[("PATH", self.path())],
            tmux,
        )
    }

    /// Replaces the stand-in `name` with `body`.
    fn stand_in(&self, name: &str, body: &str) {
        let path = self.bin.join(name);
        std::fs::write(&path, body).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// Nothing that installs or downloads ran.
    fn nothing_installed(&self) {
        let tripped = std::fs::read_to_string(&self.tripped).unwrap_or_default();
        assert!(tripped.is_empty(), "a fix or a check ran: {tripped}");
    }
}

/// A second person's device token in `state` (made now, private, before the daemon starts): a
/// member of the hub who did not set it up. Random each run, never printed.
fn second_person(state: &Path) -> String {
    use sha2::{Digest as _, Sha256};
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    std::fs::create_dir_all(state).unwrap();
    std::fs::set_permissions(state, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64
        | 1;
    // 32 bytes, unpadded base64url: 43 characters, the last carrying only 2 bits.
    let mut token = String::from("pcd_");
    for at in 0..43 {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        let index = (seed % 64) as usize;
        token.push(ALPHABET[if at == 42 { index & !3 } else { index }] as char);
    }
    let sha256: String = Sha256::digest(token.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let tokens = json!({ "version": 1, "tokens": [{
        "id": "01J00000000000000000000001",
        "sha256": sha256,
        "caller": { "member": "01JB000000000000000MEM0007", "scope": "device" },
        "created_at": 0,
    }] });
    let path = state.join("tokens.json");
    std::fs::write(&path, tokens.to_string()).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    token
}

fn delete(daemon: &Daemon, path: &str, token: &str) -> Reply {
    request(daemon.port, "DELETE", path, Some(token), None, &[])
}

fn row<'a>(check: &'a Value, id: &str) -> &'a Value {
    check["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == id)
        .unwrap_or_else(|| panic!("no {id} row in {check}"))
}

/// The check and the accounts, on the hub's own machine, read only what the stand-ins print; the
/// routes refuse agents, other machines and malformed asks; and nothing is installed.
#[test]
fn the_check_and_the_accounts_come_from_the_tools_themselves() {
    let rig = Rig::new();
    let second = second_person(&rig.state("state"));
    let daemon = rig.start(&rig.state("state"), Tmux::Refused, &[]);
    let device = daemon.device_token();
    let agent = daemon.agent_token();
    let laptop = id::LAPTOP;

    let reply = daemon.get(&format!("/v1/machines/{laptop}/check"), Some(&device));
    assert_eq!(reply.status, 200, "{}", reply.body);
    let check = reply.json();
    let ids: Vec<&str> = check["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        [
            "cli_claude",
            "cli_codex",
            "cli_opencode",
            "tmux",
            "git",
            "gh",
            "disk",
            "slurm"
        ]
    );
    assert_eq!(
        row(&check, "cli_claude"),
        &json!({
            "id": "cli_claude",
            "status": "ok",
            "detail": "2.1.3 (Claude Code)",
            "version": "2.1.3 (Claude Code)"
        })
    );
    assert_eq!(row(&check, "cli_codex")["detail"], "codex-cli 0.50.0");
    assert_eq!(row(&check, "gh")["status"], "ok");
    assert_eq!(row(&check, "slurm")["detail"], "slurm 23.02.7");
    assert!(
        row(&check, "disk")["detail"]
            .as_str()
            .unwrap()
            .ends_with("free")
            || row(&check, "disk")["status"] == "warn"
    );
    for row in check["rows"].as_array().unwrap() {
        if row["status"] == "ok" {
            assert!(row.get("fix").is_none(), "an ok row has no fix: {row}");
        }
        if let Some(fix) = row.get("fix") {
            assert_eq!(fix, "install_page", "{row}");
        }
    }

    // One row, again.
    let one = daemon.get(
        &format!("/v1/machines/{laptop}/check?row=cli_opencode"),
        Some(&device),
    );
    assert_eq!(one.status, 200, "{}", one.body);
    assert_eq!(one.json()["rows"].as_array().unwrap().len(), 1);
    assert_eq!(one.json()["rows"][0]["detail"], "1.0.25");

    let reply = daemon.get(&format!("/v1/machines/{laptop}/agents"), Some(&device));
    assert_eq!(reply.status, 200, "{}", reply.body);
    assert_eq!(
        reply.json(),
        json!([
            { "engine": "claude", "installed": true, "signed_in": false },
            { "engine": "codex", "installed": true, "signed_in": true, "account": "API key" },
            { "engine": "opencode", "installed": true, "signed_in": false }
        ])
    );
    assert!(
        !reply.body.contains("SYNTHETIC") && !reply.body.contains("sk-"),
        "nothing of a key: {}",
        reply.body
    );
    assert!(
        !daemon.stderr().contains("SYNTHETIC"),
        "no CLI output in the log"
    );

    // Refusals.
    let status = |path: &str, token: &str| daemon.get(path, Some(token)).status;
    assert_eq!(status(&format!("/v1/machines/{laptop}/check"), &agent), 403);
    assert_eq!(
        status(&format!("/v1/machines/{laptop}/agents"), &agent),
        403
    );
    let cluster = daemon.get(
        &format!("/v1/machines/{}/check", id::CLUSTER),
        Some(&device),
    );
    assert_eq!(cluster.status, 409, "{}", cluster.body);
    assert_eq!(cluster.code(), "conflict");
    assert_eq!(
        status(&format!("/v1/machines/{}/agents", id::CLUSTER), &device),
        409
    );
    assert_eq!(
        status("/v1/machines/01J00000000000000000000000/check", &device),
        404
    );
    assert_eq!(status("/v1/machines/not-an-id/agents", &device), 404);
    let bad_row = daemon.get(
        &format!("/v1/machines/{laptop}/check?row=cli-claude"),
        Some(&device),
    );
    assert_eq!(bad_row.status, 400, "{}", bad_row.body);
    assert_eq!(bad_row.code(), "invalid");
    assert_eq!(
        daemon
            .get(&format!("/v1/machines/{laptop}/check"), None)
            .status,
        401
    );

    // Without a terminal runtime (no tmux, no pitcrew-ptyd), no sign-in can open.
    let sign_in = format!("/v1/machines/{laptop}/agents/claude/sign-in");
    let refused = daemon.post(&sign_in, Some(&device), &json!({}));
    assert_eq!(refused.status, 503, "{}", refused.body);
    assert_eq!(refused.code(), "unavailable");
    assert_eq!(daemon.get(&sign_in, Some(&device)).status, 404);
    let unknown = daemon.post(
        &format!("/v1/machines/{laptop}/agents/gemini/sign-in"),
        Some(&device),
        &json!({}),
    );
    assert_eq!(unknown.status, 404, "{}", unknown.body);
    let method = daemon.post(&sign_in, Some(&device), &json!({ "method": "device_code" }));
    assert_eq!(method.status, 400, "{}", method.body);
    let extra = daemon.post(&sign_in, Some(&device), &json!({ "token": "x" }));
    assert_eq!(extra.status, 400, "{}", extra.body);
    assert_eq!(daemon.post(&sign_in, Some(&agent), &json!({})).status, 403);
    assert_eq!(delete(&daemon, &sign_in, &device).status, 404, "none open");
    assert_eq!(delete(&daemon, &sign_in, &agent).status, 403);

    // Only the person who set the hub up: another person's device token is refused on every
    // route, before anything else is looked at (even a machine that is not the hub's own).
    assert_eq!(
        daemon.get("/v1/machines", Some(&second)).status,
        200,
        "the second person's token works elsewhere"
    );
    let routes = [
        ("GET", format!("/v1/machines/{laptop}/check")),
        ("GET", format!("/v1/machines/{laptop}/check?row=cli_claude")),
        ("GET", format!("/v1/machines/{laptop}/agents")),
        ("GET", sign_in.clone()),
        ("POST", sign_in.clone()),
        ("DELETE", sign_in.clone()),
        ("GET", format!("/v1/machines/{}/check", id::CLUSTER)),
        (
            "POST",
            format!("/v1/machines/{laptop}/agents/gemini/sign-in"),
        ),
    ];
    for (method, path) in &routes {
        let body = (*method == "POST").then(|| json!({}));
        let reply = request(daemon.port, method, path, Some(&second), body.as_ref(), &[]);
        assert_eq!(reply.status, 403, "{method} {path}: {}", reply.body);
        assert_eq!(reply.code(), "forbidden");
        assert!(
            !reply.body.contains("sam@example.com") && !reply.body.contains("API key"),
            "nothing of the owner's accounts: {}",
            reply.body
        );
    }

    // An older Claude Code, without `auth`: its login is not started (it would start an agent
    // on the prompt "auth login"), whatever the terminal runtime.
    rig.stand_in("claude", OLD_CLAUDE);
    let old = daemon.post(&sign_in, Some(&device), &json!({}));
    assert_eq!(old.status, 409, "{}", old.body);
    assert_eq!(old.code(), "conflict");
    assert_eq!(
        old.json()["message"],
        "Update Claude Code first: `claude auth status` gave no answer PitCrew understands, so \
         this Claude Code may not have `claude auth login`."
    );
    assert_eq!(daemon.get(&sign_in, Some(&device)).status, 404);

    rig.nothing_installed();
}

/// tmux on `PATH`, if it is 3.2 or newer; otherwise why not.
fn usable_tmux(socket: &Path) -> Result<PathBuf, String> {
    let tmux = [
        "/usr/bin/tmux",
        "/bin/tmux",
        "/usr/local/bin/tmux",
        "/opt/homebrew/bin/tmux",
    ]
    .iter()
    .map(PathBuf::from)
    .find(|p| p.is_file())
    .ok_or("tmux is not installed")?;
    let out = Command::new(&tmux)
        .arg("-S")
        .arg(socket)
        .arg("-V")
        .env_remove("TMUX")
        .output()
        .map_err(|e| format!("cannot run {}: {e}", tmux.display()))?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    let version = text
        .strip_prefix("tmux ")
        .ok_or_else(|| format!("{text:?} is not a tmux version"))?;
    let number = version.strip_prefix("next-").unwrap_or(version);
    let mut parts = number.split('.');
    let major: u32 = parts.next().and_then(|m| m.parse().ok()).unwrap_or(0);
    let minor: u32 = parts
        .next()
        .map(|m| {
            m.chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>()
        })
        .and_then(|m| m.parse().ok())
        .unwrap_or(0);
    if (major, minor) < (3, 2) {
        return Err(format!("tmux {version} is older than 3.2"));
    }
    Ok(tmux)
}

/// Kills the tmux server on `socket` when dropped, however the test ends.
struct Server<'a> {
    tmux: &'a Path,
    socket: &'a Path,
}

impl Drop for Server<'_> {
    fn drop(&mut self) {
        let _ = Command::new(self.tmux)
            .arg("-S")
            .arg(self.socket)
            .arg("kill-server")
            .env_remove("TMUX")
            .output();
    }
}

/// Reads a terminal socket until `done` holds over its output, at most [`WAIT`]; whether an `exit`
/// frame came.
fn read_until(ws: &mut Ws, output: &mut Vec<u8>, done: impl Fn(&str, bool) -> bool) -> bool {
    let deadline = Instant::now() + WAIT;
    let mut exited = false;
    loop {
        let text = String::from_utf8_lossy(output).into_owned();
        if done(&text, exited) {
            return exited;
        }
        assert!(Instant::now() < deadline, "never came: {text:?}");
        match ws.next(Duration::from_millis(500)) {
            Ok(Some(Frame::Binary(bytes))) => output.extend(bytes),
            Ok(Some(Frame::Text(text))) => {
                let value: Value = serde_json::from_str(&text).unwrap();
                if value["type"] == "exit" {
                    exited = true;
                }
            }
            Ok(Some(Frame::Ping)) => ws.pong().unwrap(),
            Ok(Some(Frame::Close(..)) | None) => exited = true,
            Ok(Some(Frame::Pong)) => {}
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                ) => {}
            Err(e) => panic!("the terminal socket failed: {e}"),
        }
    }
}

/// Signing in runs the CLI's own login in a terminal the person drives through the terminals
/// route: it is not a session, asking again gives the same one while it runs, and once it ends
/// the CLI's own status says signed in.
#[test]
fn signing_in_runs_the_clis_own_login_in_a_terminal() {
    let rig = Rig::new();
    // Short, for the socket path limit.
    let socket = rig.tmp.path().join("t").join("s");
    let tmux = match usable_tmux(&socket) {
        Ok(tmux) => tmux,
        Err(why) => {
            let required = std::env::var("PITCREW_REQUIRE_TMUX").is_ok_and(|v| v == "1");
            assert!(!required, "PITCREW_REQUIRE_TMUX=1, but: {why}");
            eprintln!("skipped: sign-in terminals are tested in tmux 3.2 or newer: {why}");
            return;
        }
    };
    let _server = Server {
        tmux: &tmux,
        socket: &socket,
    };
    let state = rig.state("state");
    let second = second_person(&state);
    let mut daemon = rig.start(&state, Tmux::At(&socket), &[]);
    let device = daemon.device_token();
    let agent = daemon.agent_token();
    let laptop = id::LAPTOP;
    let accounts = format!("/v1/machines/{laptop}/agents");
    let sign_in = format!("/v1/machines/{laptop}/agents/claude/sign-in");

    let claude = |daemon: &Daemon| daemon.get(&accounts, Some(&device)).json()[0].clone();
    assert_eq!(claude(&daemon)["signed_in"], false);

    let started = daemon.post(&sign_in, Some(&device), &json!({}));
    assert_eq!(started.status, 201, "{}", started.body);
    let started = started.json();
    assert_eq!(started["engine"], "claude");
    assert_eq!(started["command"], json!(["claude", "auth", "login"]));
    assert_eq!(started["running"], true);
    let terminal = started["terminal"].as_str().unwrap().to_owned();

    // Asking again while it runs: the same one.
    let again = daemon.post(&sign_in, Some(&device), &json!({}));
    assert_eq!(again.status, 200, "{}", again.body);
    assert_eq!(again.json()["terminal"], terminal.as_str());
    assert_eq!(
        daemon.get(&sign_in, Some(&device)).json()["terminal"],
        terminal.as_str()
    );

    // Not a session.
    assert_eq!(
        daemon
            .get(&format!("/v1/sessions/{terminal}"), Some(&device))
            .status,
        404
    );
    // The terminals route serves it, to a person only.
    let path = format!("/v1/sessions/{terminal}/terminal");
    let refused = Ws::connect(daemon.port, &path, &agent)
        .map(|_| ())
        .unwrap_err();
    assert_eq!(refused.status, 403, "{}", refused.body);
    // Nor to another person: only the one who started it reads its screen (a device code),
    // however its id is written in the path (the check reads the id the route reads).
    let ulid = terminal.strip_prefix("ses_").unwrap_or(&terminal);
    for form in common::id_forms(ulid) {
        let theirs = Ws::connect(
            daemon.port,
            &format!("/v1/sessions/{form}/terminal"),
            &second,
        )
        .map(|_| ())
        .unwrap_err();
        assert_eq!(theirs.status, 403, "as {form}: {}", theirs.body);
    }
    assert_eq!(
        daemon.get(&sign_in, Some(&second)).status,
        403,
        "nor its status"
    );
    let mut ws = Ws::connect(daemon.port, &path, &device)
        .unwrap_or_else(|reply| panic!("{path}: {} {}", reply.status, reply.body));
    let mut output = Vec::new();
    read_until(&mut ws, &mut output, |text, _| {
        text.contains("STAND-IN LOGIN")
    });
    ws.send_binary(b"synthetic-code\r").unwrap();
    read_until(&mut ws, &mut output, |text, exited| {
        text.contains("STAND-IN SIGNED IN") && exited
    });

    // Its login has ended, and the CLI says so.
    let deadline = Instant::now() + WAIT;
    while daemon.get(&sign_in, Some(&device)).json()["running"] != false {
        assert!(Instant::now() < deadline, "the sign-in still runs");
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(
        claude(&daemon),
        json!({ "engine": "claude", "installed": true, "signed_in": true, "account": "sam@example.com" })
    );
    // An ended one is replaced by a new one.
    let next = daemon.post(&sign_in, Some(&device), &json!({}));
    assert_eq!(next.status, 201, "{}", next.body);
    assert_ne!(next.json()["terminal"], terminal.as_str());
    let gone = Ws::connect(daemon.port, &path, &device)
        .map(|_| ())
        .unwrap_err();
    assert_eq!(
        gone.status, 404,
        "the old terminal is removed: {}",
        gone.body
    );

    // Leaving it: `DELETE` stops it and removes its terminal.
    let next_path = format!(
        "/v1/sessions/{}/terminal",
        next.json()["terminal"].as_str().unwrap()
    );
    assert_eq!(delete(&daemon, &sign_in, &second).status, 403);
    assert_eq!(delete(&daemon, &sign_in, &device).status, 204);
    assert_eq!(daemon.get(&sign_in, Some(&device)).status, 404);
    let left = Ws::connect(daemon.port, &next_path, &device)
        .map(|_| ())
        .unwrap_err();
    assert_eq!(left.status, 404, "{}", left.body);
    assert_eq!(delete(&daemon, &sign_in, &device).status, 404);
    wait_for_windows(&tmux, &socket, 0);

    let log = daemon.stderr();
    assert!(!log.contains("synthetic-code"), "no keystroke in the log");
    assert!(!log.contains("sam@example.com"), "no account in the log");

    // A daemon that stops stops its sign-ins: nothing runs on until its next start.
    let running = daemon.post(&sign_in, Some(&device), &json!({}));
    assert_eq!(running.status, 201, "{}", running.body);
    wait_for_windows(&tmux, &socket, 1);
    daemon.stop();
    wait_for_windows(&tmux, &socket, 0);
    let ledger = state.join("sign-in-terminals.json");
    assert_eq!(
        serde_json::from_str::<Value>(&std::fs::read_to_string(&ledger).unwrap()).unwrap(),
        json!({ "terminals": [] })
    );

    // One that a crash left is removed when the daemon next starts: its id was noted.
    let mut daemon = rig.restart(&state, Tmux::At(&socket));
    let crashed = daemon.post(&sign_in, Some(&device), &json!({}));
    assert_eq!(crashed.status, 201, "{}", crashed.body);
    wait_for_windows(&tmux, &socket, 1);
    daemon.signal("KILL");
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(sign_in_windows(&tmux, &socket), 1, "a crash leaves it");
    let mut daemon = rig.restart(&state, Tmux::At(&socket));
    wait_for_windows(&tmux, &socket, 0);
    daemon.stop();
    rig.nothing_installed();
}

/// How many sign-in terminals' windows the tmux server on `socket` has (none without a server).
fn sign_in_windows(tmux: &Path, socket: &Path) -> usize {
    let out = Command::new(tmux)
        .arg("-S")
        .arg(socket)
        .args(["list-windows", "-a", "-F", "#{window_name}"])
        .env_remove("TMUX")
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|name| name.starts_with("pitcrew sign-in: "))
        .count()
}

/// Waits, at most [`WAIT`], until there are `count` sign-in windows.
fn wait_for_windows(tmux: &Path, socket: &Path, count: usize) {
    let deadline = Instant::now() + WAIT;
    loop {
        let found = sign_in_windows(tmux, socket);
        if found == count {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{found} sign-in windows, not {count}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}
