//! The runner's terminals, end to end: the real binary, temporary agent homes (`--homes`), and
//! the daemon's terminals' runtime.
//!
//! - **In tmux** (Unix, where tmux 3.2 or newer is installed; otherwise skipped with a message):
//!   each daemon gets a private tmux socket of the test's own (`PITCREW_TMUX_SOCKET`), never
//!   PitCrew's default and never the user's tmux, and a stand-in `claude` on its `PATH`. Every
//!   process the daemon starts carries `PITCREW_TEST_RUN=<mark>` (tmux's server and its panes
//!   inherit the daemon's environment); the test ends by checking through `/proc` that nothing
//!   with its mark is left and, however it ends, kills its server (`tmux -S <its socket>
//!   kill-server`) and anything still marked.
//! - **Without tmux** (a refused socket directory, as every other test's daemon has): the daemon
//!   serves as before, with no terminal runtime.

#![allow(clippy::unwrap_used)]

mod common;

use common::{Daemon, id, refused_tmux_socket};
use serde_json::json;
use std::time::Duration;

const WAIT: Duration = Duration::from_secs(30);

/// Without tmux (here its socket's directory is refused, as for every test daemon that does not
/// ask for tmux), the daemon serves as it did: host info has no `tmux`, the log says why, no
/// session of this machine has a terminal, and starting one is `503`.
#[test]
fn without_tmux_the_daemon_serves_with_no_terminal_runtime() {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    let homes = tmp.path().join("homes");
    std::fs::create_dir_all(homes.join(".claude")).unwrap();
    let daemon = Daemon::start(&state, &["--demo", "--homes", homes.to_str().unwrap()]);
    let device = daemon.device_token();
    daemon.wait_for_log("the runner watches these homes", WAIT);

    let info = daemon.get("/v1/host/info", None).json();
    assert_eq!(info["roles"], json!(["hub", "runner"]));
    assert_eq!(info["capabilities"], json!(["watch"]), "no tmux: {info}");
    let logs = daemon.stderr();
    if cfg!(unix) {
        let why = logs
            .lines()
            .find(|l| l.contains("the runner's terminals cannot use tmux"))
            .unwrap_or_else(|| panic!("no reason in the log:\n{logs}"));
        assert!(why.contains("WARN"), "{why}");
        // The reason: tmux is missing, or the refused socket (which was not made).
        assert!(
            why.contains("not installed") || why.contains("cannot resolve"),
            "{why}"
        );
        assert!(!refused_tmux_socket(&state).parent().unwrap().exists());
    } else {
        assert!(
            logs.contains("the runner's terminals have no runtime on this system yet"),
            "{logs}"
        );
    }

    // The demo's session of this machine has no terminal; starting one fails as unavailable.
    let terminal = daemon.get(
        &format!("/v1/sessions/{}/terminal", id::SES1),
        Some(&device),
    );
    assert_eq!(terminal.status, 404, "{}", terminal.body);
    let work = tmp.path().join("work");
    std::fs::create_dir(&work).unwrap();
    let started = daemon.post(
        "/v1/sessions",
        Some(&device),
        &json!({ "machine": id::LAPTOP, "engine": "claude", "cwd": work.to_str().unwrap() }),
    );
    assert_eq!(started.status, 503, "{}", started.body);
    assert_eq!(started.code(), "unavailable");
    assert!(
        started.body.contains("no terminal runtime"),
        "{}",
        started.body
    );
    // Commands for a session without a terminal here are a conflict.
    let send = daemon.post(
        &format!("/v1/sessions/{}/send", id::SES1),
        Some(&device),
        &json!({ "text": "hello" }),
    );
    assert_eq!(send.status, 409, "{}", send.body);
    // Another machine's session is unavailable, an unknown one not found, a bad body invalid.
    let remote = daemon.post(
        &format!("/v1/sessions/{}/interrupt", id::SES2),
        Some(&device),
        &json!({}),
    );
    assert_eq!(remote.status, 503, "{}", remote.body);
    let unknown = daemon.post(
        &format!("/v1/sessions/{}/keys", id::SES_UNKNOWN),
        Some(&device),
        &json!({ "keys": ["enter"] }),
    );
    assert_eq!(unknown.status, 404, "{}", unknown.body);
    for (path, body) in [
        ("keys", json!({ "keys": [] })),
        ("keys", json!({ "keys": ["meta"] })),
        ("end", json!({ "mode": "soon" })),
        ("send", json!({})),
    ] {
        let reply = daemon.post(
            &format!("/v1/sessions/{}/{path}", id::SES1),
            Some(&device),
            &body,
        );
        assert_eq!(reply.status, 400, "{path} {body}: {}", reply.body);
    }
    // Device routes: an agent may not.
    let agent = daemon.post(
        &format!("/v1/sessions/{}/interrupt", id::SES1),
        Some(&daemon.agent_token()),
        &json!({}),
    );
    assert_eq!(agent.status, 403, "{}", agent.body);
}

#[cfg(unix)]
mod tmux {
    use super::WAIT;
    use super::common::{Daemon, Frame, TMUX_SOCKET, Ws, id};
    use serde_json::{Value, json};
    use std::ffi::OsString;
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Output};
    use std::time::{Duration, Instant};

    /// The variable that marks every process a test's daemon starts.
    const MARK: &str = "PITCREW_TEST_RUN";

    /// The Claude fixture's own session id, which its lines carry.
    const FIXTURE_ID: &str = "2b6f1a8e-4c1d-4f5e-9a37-0c8d1e2f3a4b";

    /// A stand-in for Claude Code: like Claude given a first prompt, it writes its transcript at
    /// once, for the session id it was given (`--session-id=`), in the Claude home `@PROJECTS@`;
    /// then it shows each byte it reads as a line, `KEY <hex>`. Ctrl-C ends it (`FAKE CLAUDE
    /// BYE`).
    const FAKE_CLAUDE: &str = r#"#!/bin/sh
id=
for arg in "$@"; do
  case "$arg" in --session-id=*) id=${arg#--session-id=} ;; esac
done
[ -n "$id" ] || { echo "no --session-id" >&2; exit 2; }
dir='@PROJECTS@/-tmp-pitcrew-work'
mkdir -p "$dir"
sed "s/@NATIVE@/$id/g" '@TEMPLATE@' > "$dir/$id.jsonl.part" && mv "$dir/$id.jsonl.part" "$dir/$id.jsonl"
trap 'echo "FAKE CLAUDE BYE"; exit 0' INT
stty -icanon -echo min 1 time 0
echo "FAKE CLAUDE READY $id"
while :; do
  b=$(dd bs=1 count=1 2>/dev/null | od -An -tx1 | tr -d ' \n')
  [ -n "$b" ] || exit 0
  echo "KEY $b"
done
"#;

    /// tmux on `PATH` (absolute entries), if it is 3.2 or newer; otherwise why not.
    fn usable_tmux(socket: &Path) -> Result<PathBuf, String> {
        let path = std::env::var_os("PATH").unwrap_or_default();
        let tmux = std::env::split_paths(&path)
            .filter(|dir| dir.is_absolute())
            .map(|dir| dir.join("tmux"))
            .find(|p| p.is_file())
            .ok_or("tmux is not installed")?;
        // `-V` only prints the version; `-S` keeps even that away from any default socket.
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
        let major: u32 = parts
            .next()
            .and_then(|m| m.parse().ok())
            .ok_or_else(|| format!("tmux {version}: not a release this test reads"))?;
        let minor: u32 = parts
            .next()
            .map(|m| {
                m.chars()
                    .take_while(char::is_ascii_digit)
                    .collect::<String>()
            })
            .and_then(|m| m.parse().ok())
            .ok_or_else(|| format!("tmux {version}: not a release this test reads"))?;
        if (major, minor) < (3, 2) {
            return Err(format!("tmux {version} is older than 3.2"));
        }
        Ok(tmux)
    }

    /// One test's daemon setup: homes, a working folder, the stand-in `claude`, and a private
    /// tmux socket. Dropped, it kills its tmux server and anything still carrying its mark.
    struct Rig {
        _tmp: tempfile::TempDir,
        state: PathBuf,
        homes: PathBuf,
        work: PathBuf,
        bin: PathBuf,
        socket: PathBuf,
        tmux: PathBuf,
        mark: String,
    }

    impl Rig {
        /// `None`, after saying why, where tmux 3.2 or newer is not installed.
        fn new() -> Option<Self> {
            let tmp = tempfile::tempdir().unwrap();
            // Short, for the socket path limit; its directory is made private by the daemon.
            let socket = tmp.path().join("t").join("s");
            let tmux = match usable_tmux(&socket) {
                Ok(tmux) => tmux,
                Err(why) => {
                    eprintln!("skipped: the daemon's tmux terminals need tmux 3.2 or newer: {why}");
                    return None;
                }
            };
            let state = tmp.path().join("state");
            let homes = tmp.path().join("homes");
            let work = tmp.path().join("work");
            let bin = tmp.path().join("bin");
            for dir in [&homes, &work, &bin] {
                std::fs::create_dir_all(dir).unwrap();
            }
            let projects = homes.join(".claude").join("projects");
            std::fs::create_dir_all(&projects).unwrap();
            // The fixture's first five records, as the session the stand-in is given.
            let fixture =
                pitcrew_fixtures::data_dir().join("transcripts/claude/demo-session.jsonl");
            let lines: String = std::fs::read_to_string(fixture)
                .unwrap()
                .replace(FIXTURE_ID, "@NATIVE@")
                .split_inclusive('\n')
                .take(5)
                .collect();
            let template = bin.join("transcript.jsonl");
            std::fs::write(&template, lines).unwrap();
            let claude = bin.join("claude");
            let script = FAKE_CLAUDE
                .replace("@PROJECTS@", projects.to_str().unwrap())
                .replace("@TEMPLATE@", template.to_str().unwrap());
            std::fs::write(&claude, script).unwrap();
            std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            Some(Self {
                _tmp: tmp,
                state,
                homes,
                work,
                bin,
                socket,
                tmux,
                mark: format!("terminals-{}-{nanos}", std::process::id()),
            })
        }

        /// A daemon on this rig's state, homes and tmux socket, the stand-in first on its `PATH`.
        fn start(&self, extra: &[&str]) -> Daemon {
            let mut path = OsString::from(&self.bin);
            path.push(":");
            path.push(std::env::var_os("PATH").unwrap_or_default());
            let mut args = extra.to_vec();
            args.extend(["--homes", self.homes.to_str().unwrap()]);
            Daemon::start_with(
                &self.state,
                &args,
                &[
                    (TMUX_SOCKET, self.socket.clone().into_os_string()),
                    ("PATH", path),
                    (MARK, OsString::from(&self.mark)),
                ],
            )
        }

        /// `tmux -S <this rig's socket> <args>`.
        fn tmux(&self, args: &[&str]) -> Output {
            Command::new(&self.tmux)
                .arg("-S")
                .arg(&self.socket)
                .args(args)
                .env_remove("TMUX")
                .output()
                .unwrap()
        }

        /// Live processes carrying this rig's mark (a zombie has no environment).
        fn marked(&self) -> Vec<(u32, String)> {
            let want = format!("{MARK}={}", self.mark);
            let me = std::process::id();
            let Ok(entries) = std::fs::read_dir("/proc") else {
                return Vec::new();
            };
            entries
                .filter_map(|entry| {
                    let pid: u32 = entry.ok()?.file_name().to_str()?.parse().ok()?;
                    if pid == me {
                        return None;
                    }
                    let env = std::fs::read(format!("/proc/{pid}/environ")).ok()?;
                    if !env.split(|b| *b == 0).any(|v| v == want.as_bytes()) {
                        return None;
                    }
                    let cmd = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
                    Some((pid, String::from_utf8_lossy(&cmd).replace('\0', " ")))
                })
                .collect()
        }

        /// Waits up to `grace` for every marked process to end; returns those left.
        fn left_after(&self, grace: Duration) -> Vec<(u32, String)> {
            let deadline = Instant::now() + grace;
            loop {
                let left = self.marked();
                if left.is_empty() || Instant::now() >= deadline {
                    return left;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }

    impl Drop for Rig {
        fn drop(&mut self) {
            let _ = self.tmux(&["kill-server"]);
            for (pid, _) in self.left_after(Duration::from_secs(2)) {
                let _ = Command::new("kill")
                    .args(["-KILL", &pid.to_string()])
                    .status();
            }
        }
    }

    /// What a terminal WebSocket sent: its bytes from `from` on, its text frames, and its close.
    struct Terminal {
        ws: Ws,
        from: u64,
        bytes: Vec<u8>,
        texts: Vec<Value>,
        closed: Option<Option<u16>>,
    }

    impl Terminal {
        /// `GET /v1/sessions/<session>/terminal[?from=<from>]`, upgraded.
        fn open(daemon: &Daemon, session: &str, from: Option<u64>, token: &str) -> Self {
            let mut path = format!("/v1/sessions/{session}/terminal");
            if let Some(from) = from {
                path.push_str(&format!("?from={from}"));
            }
            let ws = Ws::connect(daemon.port, &path, token)
                .unwrap_or_else(|reply| panic!("{path}: {} {}", reply.status, reply.body));
            Self {
                ws,
                from: from.unwrap_or(0),
                bytes: Vec::new(),
                texts: Vec::new(),
                closed: None,
            }
        }

        /// The output so far, as text.
        fn text(&self) -> String {
            String::from_utf8_lossy(&self.bytes).into_owned()
        }

        /// Where the output this socket received ends.
        fn end(&self) -> u64 {
            self.from + u64::try_from(self.bytes.len()).unwrap()
        }

        /// Takes one frame, waiting at most `within`; `false` if none came.
        fn take(&mut self, within: Duration) -> bool {
            let frame = match self.ws.next(within) {
                Ok(frame) => frame,
                Err(e) if e.kind() == std::io::ErrorKind::TimedOut => return false,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return false,
                Err(e) => panic!("the terminal socket failed: {e}"),
            };
            match frame {
                Some(Frame::Binary(bytes)) => self.bytes.extend(bytes),
                Some(Frame::Text(text)) => {
                    let value: Value = serde_json::from_str(&text).unwrap();
                    if value["type"] == "truncated" && self.bytes.is_empty() {
                        self.from = value["from"].as_u64().unwrap();
                    }
                    self.texts.push(value);
                }
                Some(Frame::Ping) => self.ws.pong().unwrap(),
                Some(Frame::Pong) => {}
                Some(Frame::Close(code, _)) => self.closed = Some(code),
                None => self.closed = Some(None),
            }
            true
        }

        /// Reads until `done` holds, at most [`WAIT`].
        fn until(&mut self, what: &str, done: impl Fn(&Self) -> bool) {
            let deadline = Instant::now() + WAIT;
            while !done(self) {
                assert!(
                    self.closed.is_none(),
                    "closed ({:?}) before {what}: {:?} {:?}",
                    self.closed,
                    self.text(),
                    self.texts
                );
                assert!(
                    Instant::now() < deadline,
                    "never {what}: {:?} {:?}",
                    self.text(),
                    self.texts
                );
                self.take(Duration::from_millis(500));
            }
        }

        /// Reads until `needle` is in the output.
        fn shows(&mut self, needle: &str) {
            self.until(&format!("shows {needle:?}"), |t| t.text().contains(needle));
        }

        /// Reads until nothing has come for half a second.
        fn settle(&mut self) {
            while self.take(Duration::from_millis(500)) {}
        }
    }

    fn post(daemon: &Daemon, path: &str, token: &str, body: &Value) -> u16 {
        let reply = daemon.post(path, Some(token), body);
        assert!(
            reply.status == 204 || reply.status == 202,
            "{path} {body}: {} {}",
            reply.status,
            reply.body
        );
        reply.status
    }

    fn session(daemon: &Daemon, token: &str, id: &str) -> Value {
        let reply = daemon.get(&format!("/v1/sessions/{id}"), Some(token));
        assert_eq!(reply.status, 200, "{}", reply.body);
        reply.json()
    }

    fn eventually(what: &str, mut holds: impl FnMut() -> bool) {
        let deadline = Instant::now() + WAIT;
        while !holds() {
            assert!(Instant::now() < deadline, "never: {what}");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// `POST /v1/sessions` for the stand-in in the rig's working folder: the session, once the
    /// runner has found it in its terminal.
    fn start_claude(daemon: &Daemon, rig: &Rig, token: &str) -> Value {
        let reply = daemon.post(
            "/v1/sessions",
            Some(token),
            &json!({
                "machine": id::LAPTOP,
                "engine": "claude",
                "cwd": rig.work.to_str().unwrap(),
                "brief": "Draft section 3",
            }),
        );
        assert_eq!(reply.status, 202, "{}\n{}", reply.body, daemon.stderr());
        reply.json()
    }

    /// The whole life of a session PitCrew starts in tmux: started through the API, streamed,
    /// typed into; the daemon stops and the terminal keeps running with its offset stored; the
    /// next daemon finds it and streams on from that offset; `end` kills it, or ends it
    /// gracefully. Nothing is left behind.
    #[test]
    fn sessions_run_in_tmux_terminals_that_outlive_the_daemon() {
        let Some(rig) = Rig::new() else {
            return;
        };
        let mut daemon = rig.start(&["--demo"]);
        let device = daemon.device_token();
        let info = daemon.get("/v1/host/info", None).json();
        assert_eq!(info["capabilities"], json!(["tmux", "watch"]), "{info}");
        let logs = daemon.stderr();
        assert!(
            logs.contains("the runner's terminals run in tmux")
                && logs.contains(&format!(
                    "tmux -S {} attach -t pitcrew",
                    rig.socket.display()
                )),
            "{logs}"
        );

        // Started: a session of this machine, in a terminal running the stand-in.
        let a = start_claude(&daemon, &rig, &device);
        let a_id = a["id"].as_str().unwrap().to_owned();
        let native = a["native_id"].as_str().unwrap().to_owned();
        assert_eq!(native.len(), 36, "Claude's session id: {a}");
        assert_eq!(a["machine"], id::LAPTOP);
        assert_eq!(a["engine"], "claude");
        assert!(a["terminal"].is_string(), "{a}");
        // People can find it in tmux themselves.
        let windows = rig.tmux(&["list-windows", "-t", "pitcrew", "-F", "#{window_name}"]);
        assert!(windows.status.success(), "{windows:?}");
        assert!(
            String::from_utf8_lossy(&windows.stdout).contains("claude work"),
            "{windows:?}"
        );

        // Its WebSocket streams the stand-in's output; text, keys and interrupts reach it.
        let mut terminal = Terminal::open(&daemon, &a_id, None, &device);
        terminal.shows(&format!("FAKE CLAUDE READY {native}"));
        assert!(terminal.texts.is_empty(), "{:?}", terminal.texts);
        post(
            &daemon,
            &format!("/v1/sessions/{a_id}/send"),
            &device,
            &json!({ "text": "hi" }),
        );
        terminal.shows("KEY 68");
        terminal.shows("KEY 69");
        post(
            &daemon,
            &format!("/v1/sessions/{a_id}/keys"),
            &device,
            &json!({ "keys": ["tab"] }),
        );
        terminal.shows("KEY 09");
        post(
            &daemon,
            &format!("/v1/sessions/{a_id}/interrupt"),
            &device,
            &json!({}),
        );
        terminal.shows("KEY 1b");
        terminal.settle();
        let offset = terminal.end();
        drop(terminal);

        // The stop leaves the terminal running, its exact offset stored in tmux.
        daemon.stop();
        let logs = daemon.stderr();
        let at = |what: &str| {
            logs.find(what)
                .unwrap_or_else(|| panic!("no {what:?} in the log:\n{logs}"))
        };
        assert!(
            at("the runner stopped") < at("the terminals' runtime detached"),
            "{logs}"
        );
        assert!(
            at("the terminals' runtime detached") < at("store closed"),
            "{logs}"
        );
        drop(daemon);
        let target = format!("pitcrew:{}", window_of(&rig, &a));
        let pane = rig.tmux(&["display-message", "-p", "-t", &target, "#{pane_dead}"]);
        assert_eq!(
            String::from_utf8_lossy(&pane.stdout).trim(),
            "0",
            "{pane:?}"
        );
        let stored = rig.tmux(&["show-options", "-p", "-v", "-t", &target, "@pitcrew-offset"]);
        assert_eq!(
            String::from_utf8_lossy(&stored.stdout).trim(),
            offset.to_string(),
            "{stored:?}"
        );

        // The next daemon finds the terminal, and its output goes on from that offset.
        let daemon = rig.start(&[]);
        let info = daemon.get("/v1/host/info", None).json();
        assert_eq!(info["capabilities"], json!(["tmux", "watch"]), "{info}");
        let mut resumed = Terminal::open(&daemon, &a_id, Some(offset), &device);
        let mut from_start = Terminal::open(&daemon, &a_id, None, &device);
        post(
            &daemon,
            &format!("/v1/sessions/{a_id}/send"),
            &device,
            &json!({ "text": "x" }),
        );
        resumed.shows("KEY 78");
        assert!(
            resumed.texts.is_empty(),
            "nothing lost: {:?}",
            resumed.texts
        );
        assert!(resumed.text().starts_with("KEY 78"), "{:?}", resumed.text());
        // Output from before the restart is not kept by the new daemon: a reader from the start
        // is told where the output it has begins.
        from_start.shows("KEY 78");
        assert_eq!(
            from_start.texts.first(),
            Some(&json!({ "type": "truncated", "from": offset })),
            "{:?}",
            from_start.texts
        );
        drop(from_start);

        // A second session, ended gracefully: Ctrl-C twice, and the CLI exits.
        let b = start_claude(&daemon, &rig, &device);
        let b_id = b["id"].as_str().unwrap().to_owned();
        assert_ne!(b_id, a_id);
        let mut second = Terminal::open(&daemon, &b_id, None, &device);
        second.shows("FAKE CLAUDE READY");
        post(
            &daemon,
            &format!("/v1/sessions/{b_id}/end"),
            &device,
            &json!({ "mode": "graceful" }),
        );
        second.shows("FAKE CLAUDE BYE");
        second.until("the exit", |t| t.texts.iter().any(|v| v["type"] == "exit"));
        eventually("the second session ends", || {
            session(&daemon, &device, &b_id)["state"] == "ended"
        });

        // `end` with `kill` ends the first: its stream says so and closes, and so does tmux.
        post(
            &daemon,
            &format!("/v1/sessions/{a_id}/end"),
            &device,
            &json!({ "mode": "kill" }),
        );
        resumed.until("the exit", |t| t.texts.iter().any(|v| v["type"] == "exit"));
        let deadline = Instant::now() + WAIT;
        while resumed.closed.is_none() {
            assert!(Instant::now() < deadline, "the stream did not close");
            resumed.take(Duration::from_millis(500));
        }
        assert_eq!(resumed.closed, Some(Some(1000)));
        eventually("the first session ends", || {
            session(&daemon, &device, &a_id)["state"] == "ended"
        });
        let ended = daemon.post(
            &format!("/v1/sessions/{a_id}/send"),
            Some(&device),
            &json!({ "text": "too late" }),
        );
        assert_eq!(ended.status, 409, "{}", ended.body);

        // Both terminals have ended, so tmux's server exits; with the daemon stopped, nothing is
        // left.
        let mut daemon = daemon;
        daemon.stop();
        drop(daemon);
        let left = rig.left_after(WAIT);
        assert!(left.is_empty(), "left behind: {left:?}");
    }

    /// The tmux window id (`@<n>`) of a session's terminal, from the runner's record of it.
    fn window_of(rig: &Rig, session: &Value) -> String {
        let terminal = session["terminal"].as_str().unwrap();
        let panes = rig.tmux(&[
            "list-panes",
            "-a",
            "-F",
            "#{window_id} #{@pitcrew-terminal}",
        ]);
        let text = String::from_utf8_lossy(&panes.stdout).into_owned();
        text.lines()
            .find_map(|line| {
                let (window, tag) = line.split_once(' ')?;
                (tag == terminal || tag.ends_with(terminal) || terminal.ends_with(tag))
                    .then(|| window.to_owned())
            })
            .unwrap_or_else(|| panic!("no window for terminal {terminal}: {text}"))
    }
}
