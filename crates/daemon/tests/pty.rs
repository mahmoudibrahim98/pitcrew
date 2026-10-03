//! The runner's terminals in pitcrew-ptyd (the PTY runtime), end to end: the real binary, forced
//! to the PTY runtime (`--terminal-runtime pty`) even where tmux is installed, with the
//! pitcrew-ptyd cargo built next to it (`--ptyd`), temporary agent homes (`--homes`), and a
//! stand-in `claude` first on its `PATH` (a shell script on Unix; on Windows a batch file running a
//! PowerShell script, as npm's shims do).
//!
//! - **Never the user's ptyd.** Every daemon names its ptyd's endpoint (`--ptyd-endpoint`, a socket
//!   in a new private folder of the test's, or a pipe of the test's), or uses its state
//!   directory's own: on Unix under a `TMUX_TMPDIR` of the test's, on Windows the user's pipe with
//!   the test's state directory's digits. The ptyd a test's daemon starts exits half a second after
//!   it is idle (`--ptyd-idle-exit-ms`).
//! - **Nothing left behind.** Every process a daemon starts carries `PITCREW_TEST_RUN=<mark>`: ptyd
//!   inherits the daemon's environment, and its terminals ptyd's. Each test ends by checking that
//!   nothing with its mark is left (Linux: `/proc`; macOS: `ps -E`; Windows, where another
//!   process's environment cannot be read: processes whose command line names its folder or its
//!   pipe), and, however it ends, kills what is.
//! - **ptyd itself** is looked at as `tmux -S <socket> list-panes` would be: a `PtyRuntime` of the
//!   test's own on the same endpoint lists its terminals and reads their output (it never starts a
//!   ptyd).
//! - pitcrew-ptyd must have been built next to `pitcrewd` (`cargo test --workspace` builds it). If
//!   it is not there, the tests that need it say so and pass, unless `CI` or
//!   `PITCREW_REQUIRE_PTYD=1` is set.

#![allow(clippy::unwrap_used)]

mod common;

use common::{Daemon, Frame, Tmux, Ws, id};
use pitcrew_interfaces::runtime::Runtime as _;
use pitcrew_protocol::ids::TerminalId;
use pitcrew_runtime::pty::{PtyOptions, PtyRuntime};
use serde_json::{Value, json};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(30);

/// The variable that marks every process a test's daemon starts.
const MARK: &str = "PITCREW_TEST_RUN";

/// pitcrew-ptyd's file name here.
const PTYD: &str = pitcrew_runtime::pty::launch::PTYD;

/// The Claude fixture's own session id, which its lines carry.
const FIXTURE_ID: &str = "2b6f1a8e-4c1d-4f5e-9a37-0c8d1e2f3a4b";

/// A stand-in for Claude Code: like Claude given a first prompt, it writes its transcript at once,
/// for the session id it was given (`--session-id=`), in its Claude home (`CLAUDE_CONFIG_DIR`,
/// which ptyd passes on from the daemon); then it shows each byte it reads as a line, `KEY <hex>`,
/// and after a `d` waits two seconds and prints `DELAYED`. Ctrl-C ends it (`FAKE CLAUDE BYE`).
const FAKE_CLAUDE: &str = r#"#!/bin/sh
id=
for arg in "$@"; do
  case "$arg" in --session-id=*) id=${arg#--session-id=} ;; esac
done
[ -n "$id" ] || { echo "no --session-id" >&2; exit 2; }
dir="${CLAUDE_CONFIG_DIR:?}/projects/-tmp-pitcrew-work"
mkdir -p "$dir"
sed "s/@NATIVE@/$id/g" '@TEMPLATE@' > "$dir/$id.jsonl.part" && mv "$dir/$id.jsonl.part" "$dir/$id.jsonl"
trap 'echo "FAKE CLAUDE BYE"; exit 0' INT
stty -icanon -echo min 1 time 0
echo "FAKE CLAUDE READY $id"
while :; do
  b=$(dd bs=1 count=1 2>/dev/null | od -An -tx1 | tr -d ' \n')
  [ -n "$b" ] || exit 0
  echo "KEY $b"
  if [ "$b" = 64 ]; then sleep 2; echo DELAYED; fi
done
"#;

/// The same stand-in on Windows: `claude.cmd` (found through `PATHEXT`, as npm's shims are) hands
/// its arguments to this script in a variable, since `cmd.exe` and PowerShell would each parse
/// them again. Ctrl-C is read as a key (`TreatControlCAsInput`), as Claude reads it.
const FAKE_CLAUDE_PS1: &str = r#"$m = [regex]::Match([string]$env:PITCREW_STAND_IN_ARGS, '--session-id=([0-9A-Za-z-]+)')
if (-not $m.Success) { [Console]::Error.WriteLine('no --session-id'); exit 2 }
$id = $m.Groups[1].Value
$dir = Join-Path $env:CLAUDE_CONFIG_DIR 'projects\-tmp-pitcrew-work'
[void][IO.Directory]::CreateDirectory($dir)
$text = [IO.File]::ReadAllText('@TEMPLATE@').Replace('@NATIVE@', $id)
[IO.File]::WriteAllText("$dir\$id.jsonl.part", $text)
[IO.File]::Move("$dir\$id.jsonl.part", "$dir\$id.jsonl")
[Console]::TreatControlCAsInput = $true
[Console]::Out.WriteLine("FAKE CLAUDE READY $id")
while ($true) {
  $c = [int][Console]::ReadKey($true).KeyChar
  if ($c -eq 3) { [Console]::Out.WriteLine('FAKE CLAUDE BYE'); exit 0 }
  [Console]::Out.WriteLine(('KEY {0:x2}' -f $c))
  if ($c -eq 100) { Start-Sleep -Seconds 2; [Console]::Out.WriteLine('DELAYED') }
}
"#;

/// The batch file that runs [`FAKE_CLAUDE_PS1`].
const FAKE_CLAUDE_CMD: &str = "@echo off\r\nset \"PITCREW_STAND_IN_ARGS=%*\"\r\n\
     powershell.exe -NoLogo -NoProfile -ExecutionPolicy Bypass -File \"%~dp0claude.ps1\"\r\n";

/// The tests that start a ptyd run one at a time: each runs daemons, a ptyd and terminals.
static SERIAL: Mutex<()> = Mutex::new(());

/// The pitcrew-ptyd built next to the `pitcrewd` under test.
fn built_ptyd() -> PathBuf {
    Path::new(common::PITCREWD).with_file_name(PTYD)
}

/// A new temporary folder, in the system's own: the daemons' homes are made in it, and
/// `pitcrew_fixtures::homes` accepts homes only inside that folder (`/var/folders/…/T` on macOS,
/// not `/tmp`). Its sockets still fit macOS's 103 bytes: the longest, a state directory's default
/// endpoint under the test's `TMUX_TMPDIR` (`<tmp>/td/pitcrew-<uid>/<8 hex>/ptyd`), is about 90.
fn temporary() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

/// One test's setup: homes, a working folder, the stand-in `claude`, and the pitcrew-ptyd to run.
/// Dropped, it kills anything of its own still running.
struct Rig {
    tmp: tempfile::TempDir,
    homes: PathBuf,
    work: PathBuf,
    bin: PathBuf,
    ptyd: PathBuf,
    mark: String,
    /// What names this test's processes in their command lines (Windows): its folder, its pipes.
    names: Mutex<Vec<String>>,
    _serial: MutexGuard<'static, ()>,
}

impl Rig {
    /// `None`, after saying why, where pitcrew-ptyd was not built next to `pitcrewd`; a failure
    /// instead under CI or with `PITCREW_REQUIRE_PTYD=1`.
    fn new(test: &str) -> Option<Self> {
        let serial = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
        let ptyd = built_ptyd();
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
        let tmp = temporary();
        let homes = tmp.path().join("homes");
        let work = tmp.path().join("work");
        let bin = tmp.path().join("bin");
        for dir in [&homes, &work, &bin] {
            std::fs::create_dir_all(dir).unwrap();
        }
        // The fixture's first five records, as the session the stand-in is given.
        let fixture = pitcrew_fixtures::data_dir().join("transcripts/claude/demo-session.jsonl");
        let lines: String = std::fs::read_to_string(fixture)
            .unwrap()
            .replace(FIXTURE_ID, "@NATIVE@")
            .split_inclusive('\n')
            .take(5)
            .collect();
        let template = bin.join("transcript.jsonl");
        std::fs::write(&template, lines).unwrap();
        let template = template.to_str().unwrap();
        if cfg!(windows) {
            let script = FAKE_CLAUDE_PS1.replace("@TEMPLATE@", template);
            std::fs::write(bin.join("claude.ps1"), script).unwrap();
            std::fs::write(bin.join("claude.cmd"), FAKE_CLAUDE_CMD).unwrap();
        } else {
            let claude = bin.join("claude");
            std::fs::write(&claude, FAKE_CLAUDE.replace("@TEMPLATE@", template)).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let mark = format!("pty-{test}-{}-{nanos}", std::process::id());
        // As given, and resolved (Windows may show a temporary folder by its short name or its
        // long one).
        let mut names = vec![tmp.path().display().to_string()];
        if let Ok(real) = std::fs::canonicalize(tmp.path()) {
            let real = real.display().to_string();
            names.push(real.strip_prefix(r"\\?\").unwrap_or(&real).to_owned());
        }
        let names = Mutex::new(names);
        Some(Self {
            tmp,
            homes,
            work,
            bin,
            ptyd,
            mark,
            names,
            _serial: serial,
        })
    }

    fn root(&self) -> &Path {
        self.tmp.path()
    }

    /// An endpoint of this test's own, `name`: a socket in a new private folder (Unix), or a pipe.
    fn endpoint(&self, name: &str) -> PathBuf {
        let endpoint = if cfg!(windows) {
            PathBuf::from(format!(r"\\.\pipe\pitcrew-ptyd-{}-{name}", self.mark))
        } else {
            self.root().join(name).join("ptyd")
        };
        self.named(&endpoint);
        endpoint
    }

    /// Remembers that `what` names this test's processes (Windows).
    fn named(&self, what: &Path) {
        self.names.lock().unwrap().push(what.display().to_string());
    }

    /// A daemon on this rig's state and homes, its terminals forced into this rig's ptyd.
    fn start(&self, extra: &[&str]) -> Daemon {
        self.start_on(&self.root().join("state"), &self.homes, extra, &[])
    }

    /// A daemon on `state`, watching `homes` (its stand-in writes there), its terminals forced into
    /// pitcrew-ptyd, with the stand-in first on its `PATH`, this rig's mark, and `env`.
    fn start_on(
        &self,
        state: &Path,
        homes: &Path,
        extra: &[&str],
        env: &[(&str, OsString)],
    ) -> Daemon {
        let claude = homes.join(".claude");
        std::fs::create_dir_all(claude.join("projects")).unwrap();
        let path = std::env::var_os("PATH").unwrap_or_default();
        let path = std::env::join_paths(
            std::iter::once(self.bin.clone()).chain(std::env::split_paths(&path)),
        )
        .unwrap();
        let mut args = vec![
            "--terminal-runtime",
            "pty",
            "--ptyd",
            self.ptyd.to_str().unwrap(),
            "--ptyd-idle-exit-ms",
            "500",
        ];
        args.extend(extra);
        args.extend(["--homes", homes.to_str().unwrap()]);
        let mut all = vec![
            ("PATH", path),
            (MARK, OsString::from(&self.mark)),
            ("CLAUDE_CONFIG_DIR", claude.into_os_string()),
        ];
        all.extend(env.iter().cloned());
        Daemon::start_with(state, &args, &all, Tmux::Refused)
    }

    /// This test's processes still running: marked (Unix), or named in their command line
    /// (Windows).
    fn running(&self) -> Vec<(u32, String)> {
        #[cfg(unix)]
        {
            marked(&self.mark)
        }
        #[cfg(not(unix))]
        {
            named(&self.names.lock().unwrap())
        }
    }

    /// Waits up to `grace` for every process of this test to end; returns those left.
    fn left_after(&self, grace: Duration) -> Vec<(u32, String)> {
        let deadline = Instant::now() + grace;
        loop {
            let left = self.running();
            if left.is_empty() || Instant::now() >= deadline {
                return left;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        for (pid, _) in self.left_after(Duration::from_secs(2)) {
            kill(pid);
        }
    }
}

/// Live processes other than this one carrying `MARK=<mark>` (Linux: `/proc`; macOS: `ps -E`).
#[cfg(unix)]
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

/// Live processes other than this one whose command line holds one of `names` (Windows).
#[cfg(not(unix))]
fn named(names: &[String]) -> Vec<(u32, String)> {
    let out = std::process::Command::new("powershell.exe")
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "Get-CimInstance Win32_Process | ForEach-Object { '{0}|{1}' -f $_.ProcessId, $_.CommandLine }",
        ])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    let me = std::process::id();
    let names: Vec<String> = names.iter().map(|n| n.to_lowercase()).collect();
    out.lines()
        .filter_map(|line| {
            let (pid, cmd) = line.split_once('|')?;
            let pid: u32 = pid.trim().parse().ok()?;
            let lower = cmd.to_lowercase();
            (pid != me && names.iter().any(|n| lower.contains(n.as_str())))
                .then(|| (pid, cmd.trim().to_owned()))
        })
        .collect()
}

fn kill(pid: u32) {
    if cfg!(windows) {
        let _ = std::process::Command::new("taskkill")
            .args(["/F", "/T", "/PID", &pid.to_string()])
            .status();
    } else {
        let _ = std::process::Command::new("kill")
            .args(["-KILL", &pid.to_string()])
            .status();
    }
}

/// A client of the ptyd on `endpoint`, to look at it: it lists and reads, and never starts one.
fn observer(rig: &Rig, endpoint: &Path) -> PtyRuntime {
    let mut options = PtyOptions::new(endpoint);
    options.ptyd.clone_from(&rig.ptyd);
    PtyRuntime::new(options).unwrap()
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
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                ) =>
            {
                return false;
            }
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

    /// Reads until the server closes the socket, and returns its close code.
    fn closes(&mut self) -> Option<u16> {
        let deadline = Instant::now() + WAIT;
        while self.closed.is_none() {
            assert!(Instant::now() < deadline, "the stream did not close");
            self.take(Duration::from_millis(500));
        }
        self.closed.flatten()
    }
}

fn post(daemon: &Daemon, path: &str, token: &str, body: &Value) {
    let reply = daemon.post(path, Some(token), body);
    assert!(
        reply.status == 204 || reply.status == 202,
        "{path} {body}: {} {}",
        reply.status,
        reply.body
    );
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

/// `POST /v1/sessions` for the stand-in in `cwd`: the session, once the runner has found it in
/// its terminal.
fn start_claude(daemon: &Daemon, cwd: &str, token: &str) -> Value {
    let reply = daemon.post(
        "/v1/sessions",
        Some(token),
        &json!({
            "machine": id::LAPTOP,
            "engine": "claude",
            "cwd": cwd,
            "brief": "Draft section 3",
        }),
    );
    assert_eq!(reply.status, 202, "{}\n{}", reply.body, daemon.stderr());
    reply.json()
}

/// A session's terminal.
fn terminal_of(session: &Value) -> TerminalId {
    session["terminal"]
        .as_str()
        .unwrap_or_else(|| panic!("no terminal: {session}"))
        .parse()
        .unwrap()
}

/// The line of the daemon's log that says its terminals run in pitcrew-ptyd.
fn pty_line(daemon: &Daemon) -> String {
    let logs = daemon.stderr();
    logs.lines()
        .find(|l| l.contains("the runner's terminals run in pitcrew-ptyd, which keeps them"))
        .unwrap_or_else(|| panic!("the terminals do not run in pitcrew-ptyd:\n{logs}"))
        .to_owned()
}

/// Waits until the ptyd `observer` looks at holds output past `from` for terminal `id` that
/// contains every one of `needles`; returns that output.
fn output_shows(observer: &PtyRuntime, id: TerminalId, from: u64, needles: &[&str]) -> String {
    let deadline = Instant::now() + WAIT;
    loop {
        let chunk = observer.read_output(id, from, usize::MAX).unwrap();
        let text = String::from_utf8_lossy(&chunk.data).into_owned();
        if needles.iter().all(|n| text.contains(n)) {
            return text;
        }
        assert!(
            Instant::now() < deadline,
            "ptyd never held {needles:?} past {from}: {text:?}"
        );
        let _ = observer.wait_for_output(id, chunk.end, Duration::from_millis(500));
    }
}

/// The whole life of a session PitCrew starts in pitcrew-ptyd: started through the API, streamed,
/// typed into; the daemon stops, and ptyd keeps the terminal running and its output, the output
/// printed while no daemon runs included; the next daemon finds it and streams on from the offset
/// it had, with nothing lost, and from the start with all of it; `end` ends it gracefully, or kills
/// it. A start for another machine, or with what the runner refuses, starts nothing. Nothing is
/// left behind.
#[test]
fn sessions_run_in_ptyd_terminals_that_outlive_the_daemon() {
    let Some(rig) = Rig::new("life") else {
        return;
    };
    let endpoint = rig.endpoint("p");
    let endpoint_arg = endpoint.to_str().unwrap().to_owned();
    let mut daemon = rig.start(&["--demo", "--ptyd-endpoint", &endpoint_arg]);
    let device = daemon.device_token();
    let info = daemon.get("/v1/host/info", None).json();
    assert_eq!(info["roles"], json!(["hub", "runner"]));
    assert_eq!(info["capabilities"], json!(["pty", "watch", "scan"]), "{info}");
    let line = pty_line(&daemon);
    assert!(line.contains("INFO"), "{line}");
    assert!(line.contains(&endpoint_arg), "{line}");
    assert!(line.contains("--terminal-runtime pty"), "{line}");
    let logs = daemon.stderr();
    for warned in [
        "--terminal-runtime pty:",
        "--ptyd:",
        "--ptyd-endpoint:",
        "--ptyd-idle-exit-ms:",
    ] {
        let found = logs.lines().find(|l| l.contains(warned));
        assert!(
            found.is_some_and(|l| l.contains("WARN")),
            "{warned} is not warned:\n{logs}"
        );
    }
    assert!(!logs.contains("attach to them with `tmux"), "{logs}");
    // ptyd starts with the first terminal, not before.
    let ptyds = || -> Vec<(u32, String)> {
        rig.running()
            .into_iter()
            .filter(|(_, cmd)| cmd.contains(" --endpoint "))
            .collect()
    };
    assert!(ptyds().is_empty(), "{:?}", ptyds());

    // Started: a session of this machine, in a terminal of ptyd's running the stand-in.
    let work = rig.work.to_str().unwrap().to_owned();
    let a = start_claude(&daemon, &work, &device);
    let a_id = a["id"].as_str().unwrap().to_owned();
    let native = a["native_id"].as_str().unwrap().to_owned();
    assert_eq!(native.len(), 36, "Claude's session id: {a}");
    assert_eq!(a["machine"], id::LAPTOP);
    let a_terminal = terminal_of(&a);
    let ptyd = observer(&rig, &endpoint);
    let listed = ptyd.list().unwrap();
    assert_eq!(listed.len(), 1, "{listed:?}");
    assert_eq!(listed[0].id, a_terminal);
    assert!(listed[0].alive);
    assert_eq!(listed[0].name, "claude work");
    assert_eq!(
        listed[0].native_target, None,
        "nothing attaches to a PTY by hand"
    );
    let ptyd_pid = ptyd.ptyd_pid().unwrap();
    assert!(
        ptyds()
            .iter()
            .any(|(pid, cmd)| *pid == ptyd_pid && cmd.contains(&endpoint_arg)),
        "ptyd runs on the test's endpoint: {:?}",
        ptyds()
    );

    // Starts that must start nothing: another machine of the workspace; a permission mode the
    // runner refuses; a model that reads as an option.
    for (body, status) in [
        (
            json!({ "machine": id::CLUSTER, "engine": "claude", "cwd": work }),
            503,
        ),
        (
            json!({ "machine": id::LAPTOP, "engine": "claude", "cwd": work,
                    "permission_mode": "bypass_permissions" }),
            400,
        ),
        (
            json!({ "machine": id::LAPTOP, "engine": "claude", "cwd": work,
                    "model": "--dangerously-skip-permissions" }),
            400,
        ),
    ] {
        let reply = daemon.post("/v1/sessions", Some(&device), &body);
        assert_eq!(reply.status, status, "{body}: {}", reply.body);
    }
    assert_eq!(ptyd.list().unwrap().len(), 1, "nothing more started");

    // Its WebSocket streams the stand-in's output; text, keys and interrupts reach it.
    let mut terminal = Terminal::open(&daemon, &a_id, None, &device);
    terminal.shows(&format!("FAKE CLAUDE READY {native}"));
    assert!(terminal.texts.is_empty(), "{:?}", terminal.texts);
    let send = format!("/v1/sessions/{a_id}/send");
    post(&daemon, &send, &device, &json!({ "text": "hi" }));
    terminal.shows("KEY 68");
    terminal.shows("KEY 69");
    // Enter: CR, which a Unix terminal hands the program as LF (`icrnl`).
    terminal.until("Enter", |t| {
        t.text().contains("KEY 0d") || t.text().contains("KEY 0a")
    });
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
    if cfg!(unix) {
        // ptyd holds the same stream, byte for byte.
        let all = ptyd.read_output(a_terminal, 0, usize::MAX).unwrap();
        assert_eq!((all.offset, all.end, all.truncated), (0, offset, false));
    }

    // A `d` makes the stand-in print again two seconds later: by then no daemon runs.
    post(&daemon, &send, &device, &json!({ "text": "d" }));
    daemon.stop();
    if cfg!(unix) {
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
        let detached = logs
            .lines()
            .find(|l| l.contains("the terminals' runtime detached"))
            .unwrap();
        assert!(detached.contains("pitcrew-ptyd"), "{detached}");
    }
    drop(daemon);
    // ptyd and the terminal keep running, and ptyd keeps what is printed meanwhile.
    let listed = ptyd.list().unwrap();
    assert_eq!(listed.len(), 1, "{listed:?}");
    assert!(listed[0].alive, "the terminal outlived the daemon");
    assert_eq!(ptyd.ptyd_pid(), Some(ptyd_pid));
    output_shows(&ptyd, a_terminal, offset, &["KEY 64", "DELAYED"]);

    // The next daemon finds the terminal, and its output goes on from that offset, nothing lost;
    // a reader from the start gets all of it, as ptyd kept it.
    let daemon = rig.start(&["--ptyd-endpoint", &endpoint_arg]);
    let info = daemon.get("/v1/host/info", None).json();
    assert_eq!(info["capabilities"], json!(["pty", "watch", "scan"]), "{info}");
    let mut resumed = Terminal::open(&daemon, &a_id, Some(offset), &device);
    resumed.shows("DELAYED");
    assert!(
        resumed.texts.is_empty(),
        "nothing lost: {:?}",
        resumed.texts
    );
    if cfg!(unix) {
        assert!(resumed.text().starts_with("KEY 64"), "{:?}", resumed.text());
    }
    let mut from_start = Terminal::open(&daemon, &a_id, None, &device);
    from_start.shows(&format!("FAKE CLAUDE READY {native}"));
    from_start.shows("DELAYED");
    assert!(from_start.texts.is_empty(), "{:?}", from_start.texts);
    drop(from_start);
    post(&daemon, &send, &device, &json!({ "text": "x" }));
    resumed.shows("KEY 78");

    // A second session, ended gracefully: Ctrl-C twice, and the CLI exits.
    let cwd = if cfg!(unix) {
        // Its folder given with `..`, resolved.
        format!("{work}/../work")
    } else {
        work.clone()
    };
    let b = start_claude(&daemon, &cwd, &device);
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

    // `end` with `kill` ends the first: its stream says so and closes.
    post(
        &daemon,
        &format!("/v1/sessions/{a_id}/end"),
        &device,
        &json!({ "mode": "kill" }),
    );
    resumed.until("the exit", |t| t.texts.iter().any(|v| v["type"] == "exit"));
    assert_eq!(resumed.closes(), Some(1000));
    eventually("the first session ends", || {
        session(&daemon, &device, &a_id)["state"] == "ended"
    });
    let ended = daemon.post(&send, Some(&device), &json!({ "text": "too late" }));
    assert_eq!(ended.status, 409, "{}", ended.body);
    let listed = ptyd.list().unwrap();
    assert_eq!(listed.len(), 2, "{listed:?}");
    assert!(listed.iter().all(|t| !t.alive), "{listed:?}");
    drop(ptyd);

    // With every terminal ended and the daemon stopped, ptyd exits once idle: nothing is left.
    let mut daemon = daemon;
    daemon.stop();
    drop(daemon);
    let left = rig.left_after(WAIT);
    assert!(left.is_empty(), "left behind: {left:?}");
    if cfg!(unix) {
        assert!(!endpoint.exists(), "ptyd removed its socket");
    }
}

/// A new folder, private (0700) on Unix.
fn private_folder(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        std::fs::DirBuilder::new().mode(0o700).create(path).unwrap();
    }
    #[cfg(not(unix))]
    std::fs::create_dir(path).unwrap();
}

/// 8 hex digits of the sha256 of the canonical state directory, as the daemon names its places.
fn digits(state: &Path) -> String {
    use sha2::{Digest as _, Sha256};
    let canonical = std::fs::canonicalize(state).unwrap();
    let hash = Sha256::digest(canonical.as_os_str().as_encoded_bytes());
    hash[..4].iter().map(|b| format!("{b:02x}")).collect()
}

/// One ptyd per state directory: two daemons of one user, on their state directories' own
/// endpoints, run their terminals in two ptyds, each holding only its own; a third daemon given
/// the first's endpoint finds it locked and runs without terminals.
#[test]
fn each_state_directory_has_a_ptyd_of_its_own() {
    let Some(rig) = Rig::new("two") else {
        return;
    };
    // On Unix the runtime's per-user directory is inside this test's folder. On Windows the
    // endpoints' lock files are in `%LOCALAPPDATA%\PitCrew`, which one user's daemons share: here
    // a folder of this test's, given to all three.
    let tmpdir = rig.root().join("td");
    let env = if cfg!(unix) {
        private_folder(&tmpdir);
        vec![("TMUX_TMPDIR", tmpdir.clone().into_os_string())]
    } else {
        vec![("LOCALAPPDATA", rig.root().join("local").into_os_string())]
    };
    let start = |name: &str| {
        rig.start_on(
            &rig.root().join(name),
            &rig.root().join(format!("{name}-homes")),
            &["--demo"],
            &env,
        )
    };
    let (mut a, mut b) = (start("a"), start("b"));
    let expected = |daemon: &Daemon| {
        let digits = digits(&daemon.state);
        #[cfg(unix)]
        let endpoint = tmpdir
            .join(format!("pitcrew-{}", pitcrew_auth::euid()))
            .join(&digits)
            .join("ptyd");
        #[cfg(not(unix))]
        let endpoint = PathBuf::from(format!(
            "{}-{digits}",
            PtyOptions::default_endpoint().display()
        ));
        rig.named(&endpoint);
        endpoint
    };
    let (ea, eb) = (expected(&a), expected(&b));
    assert_ne!(ea, eb);
    for (daemon, endpoint) in [(&a, &ea), (&b, &eb)] {
        let info = daemon.get("/v1/host/info", None).json();
        assert_eq!(info["capabilities"], json!(["pty", "watch", "scan"]), "{info}");
        let line = pty_line(daemon);
        assert!(
            line.contains(&format!("endpoint={}", endpoint.display())),
            "{line}"
        );
    }

    // Each starts a session: each ptyd holds its own terminal, and only that one.
    let work = rig.work.to_str().unwrap().to_owned();
    let in_a = start_claude(&a, &work, &a.device_token());
    let in_b = start_claude(&b, &work, &b.device_token());
    let (on_a, on_b) = (observer(&rig, &ea), observer(&rig, &eb));
    for (ptyd, session) in [(&on_a, &in_a), (&on_b, &in_b)] {
        let listed = ptyd.list().unwrap();
        assert_eq!(listed.len(), 1, "{listed:?}");
        assert_eq!(listed[0].id, terminal_of(session));
    }
    assert_ne!(
        on_a.ptyd_pid().unwrap(),
        on_b.ptyd_pid().unwrap(),
        "two ptyds"
    );

    // A third daemon given the first's endpoint: locked, so no terminals for it, and the first's
    // ptyd is not touched.
    {
        let ea_arg = ea.to_str().unwrap().to_owned();
        let c_env: &[(&str, OsString)] = if cfg!(unix) { &[] } else { &env };
        let c = rig.start_on(
            &rig.root().join("c"),
            &rig.root().join("c-homes"),
            &["--demo", "--ptyd-endpoint", &ea_arg],
            c_env,
        );
        let info = c.get("/v1/host/info", None).json();
        assert_eq!(info["capabilities"], json!(["watch", "scan"]), "{info}");
        let logs = c.stderr();
        assert!(
            logs.contains("another pitcrewd uses this pitcrew-ptyd endpoint"),
            "{logs}"
        );
        assert_eq!(on_a.list().unwrap().len(), 1);
        drop(c);
    }

    for (daemon, session) in [(&a, &in_a), (&b, &in_b)] {
        let id = session["id"].as_str().unwrap();
        post(
            daemon,
            &format!("/v1/sessions/{id}/end"),
            &daemon.device_token(),
            &json!({ "mode": "kill" }),
        );
    }
    drop((on_a, on_b));
    a.stop();
    b.stop();
    drop((a, b));
    let left = rig.left_after(WAIT);
    assert!(left.is_empty(), "left behind: {left:?}");
}

/// Where no pitcrew-ptyd is installed (here where the daemon is told to look), the PTY runtime
/// is not used: no runtime, a warning naming where it looked, `503` for a start, and nothing made
/// for its endpoint.
#[test]
fn a_missing_ptyd_means_no_runtime_and_a_clear_log_line() {
    let tmp = temporary();
    let state = tmp.path().join("state");
    let homes = tmp.path().join("homes");
    let work = tmp.path().join("work");
    for dir in [homes.join(".claude"), work.clone()] {
        std::fs::create_dir_all(dir).unwrap();
    }
    let nowhere = tmp.path().join("nowhere").join(PTYD);
    let endpoint = if cfg!(windows) {
        format!(
            r"\\.\pipe\pitcrew-ptyd-missing-{}-{}",
            std::process::id(),
            digits(tmp.path())
        )
    } else {
        tmp.path().join("p").join("ptyd").display().to_string()
    };
    let daemon = Daemon::start(
        &state,
        &[
            "--demo",
            "--homes",
            homes.to_str().unwrap(),
            "--terminal-runtime",
            "pty",
            "--ptyd",
            nowhere.to_str().unwrap(),
            "--ptyd-endpoint",
            &endpoint,
        ],
    );
    let info = daemon.get("/v1/host/info", None).json();
    assert_eq!(info["roles"], json!(["hub", "runner"]));
    assert_eq!(info["capabilities"], json!(["watch", "scan"]), "{info}");
    let logs = daemon.stderr();
    let why = logs
        .lines()
        .find(|l| l.contains("the runner's terminals cannot use tmux or pitcrew-ptyd"))
        .unwrap_or_else(|| panic!("no reason in the log:\n{logs}"));
    assert!(why.contains("WARN"), "{why}");
    assert!(
        why.contains("tmux is not tried (--terminal-runtime pty)"),
        "{why}"
    );
    assert!(
        why.contains(&format!(
            "pitcrew-ptyd is not installed at {}",
            nowhere.display()
        )),
        "{why}"
    );
    let started = daemon.post(
        "/v1/sessions",
        Some(&daemon.device_token()),
        &json!({ "machine": id::LAPTOP, "engine": "claude", "cwd": work.to_str().unwrap() }),
    );
    assert_eq!(started.status, 503, "{}", started.body);
    assert!(
        started.body.contains("no terminal runtime"),
        "{}",
        started.body
    );
    assert!(
        !tmp.path().join("p").exists(),
        "nothing made for the endpoint"
    );
    assert!(!tmp.path().join("nowhere").exists());
}
