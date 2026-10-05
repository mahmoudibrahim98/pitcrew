//! The runner's terminals and session commands, end to end: the real binary, temporary agent
//! homes (`--homes`), and the daemon's terminals' runtime.
//!
//! - **In tmux** (Unix, where tmux 3.2 or newer is installed; otherwise skipped with a message,
//!   or failed when `PITCREW_REQUIRE_TMUX=1`): each daemon gets a private tmux socket of the
//!   test's own (`--tmux-socket`), or its state directory's own socket under a `TMUX_TMPDIR` of
//!   the test's; never PitCrew's default directory, never the user's tmux. A stand-in `claude` is
//!   first on its `PATH`. Every process the daemon starts carries `PITCREW_TEST_RUN=<mark>` (tmux's
//!   server and its panes inherit the daemon's environment); the test ends by checking through
//!   `/proc` that nothing with its mark is left and, however it ends, kills its servers (`tmux -S
//!   <its socket> kill-server`) and anything still marked.
//! - **Without tmux** (a refused socket directory, as every other test's daemon has, and no
//!   pitcrew-ptyd where it is looked for): the daemon serves with no terminal runtime, and the
//!   session commands' checks hold. The PTY runtime's tests are in `tests/pty.rs`.

#![allow(clippy::unwrap_used)]

mod common;

use common::{Daemon, id, refused_tmux_socket};
use pitcrew_protocol::events::{Event, EventBody};
use pitcrew_protocol::ids::{MemberId, SessionId};
use pitcrew_protocol::model::{Engine, Member, MemberKind, Session, SessionState};
use serde_json::{Value, json};
use std::path::Path;
use std::time::Duration;

const WAIT: Duration = Duration::from_secs(30);

/// Appends `bodies` to the store in `state` from this process, with the work model's
/// projections, so the hub's tables have them at once (as the runner's tests give agents).
fn append_with_projections(state: &Path, bodies: Vec<EventBody>) {
    let store = pitcrew_store::Store::open_with(
        state.join("hub.db"),
        pitcrew_store::StoreOptions::default(),
        pitcrew_hub_work::projections(),
    )
    .unwrap();
    let events: Vec<Event> = bodies
        .into_iter()
        .map(|body| {
            Event::now(
                id::WORKSPACE.parse().unwrap(),
                id::SAM.parse().unwrap(),
                body,
            )
        })
        .collect();
    store.append(&events).unwrap();
}

/// A session of the demo's laptop (the runner's machine), with no terminal, run as `agent`.
fn session_of(agent: Option<MemberId>) -> Session {
    Session {
        id: SessionId::new(),
        engine: Engine::Claude,
        native_id: SessionId::new().to_string(),
        machine: id::LAPTOP.parse().unwrap(),
        cwd: "/home/sam/work".into(),
        branch: None,
        title: None,
        agent,
        workstream: None,
        task: None,
        link_basis: None,
        state: SessionState::Idle,
        status_line: None,
        started: 1,
        last_activity: 1,
        terminal: None,
        parent: None,
        recorded: None,
    }
}

/// Without tmux (here its socket's directory is refused, as for every test daemon that does not
/// ask for tmux), the daemon serves as it did: host info has no `tmux`, the log says why, no
/// session of this machine has a terminal, and starting one is `503`. Every check a session
/// command makes before the runner is asked holds: bounds, folders, who may.
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
    assert_eq!(
        info["capabilities"],
        json!(["watch", "scan"]),
        "no tmux, no pty: {info}"
    );
    let logs = daemon.stderr();
    let why = logs
        .lines()
        .find(|l| l.contains("the runner's terminals cannot use tmux or pitcrew-ptyd"))
        .unwrap_or_else(|| panic!("no reason in the log:\n{logs}"));
    assert!(why.contains("WARN"), "{why}");
    // Where pitcrew-ptyd was looked for (here where the test helper put none).
    assert!(
        why.contains(&format!(
            "pitcrew-ptyd is not installed at {}",
            common::missing_ptyd(&state).display()
        )),
        "{why}"
    );
    assert!(logs.contains("--ptyd"), "the override is warned: {logs}");
    if cfg!(unix) {
        // The refused socket's directory, which was not made.
        assert!(why.contains("cannot create"), "{why}");
        assert!(!refused_tmux_socket(&state).parent().unwrap().exists());
        assert!(
            logs.contains("--tmux-socket"),
            "the override is warned: {logs}"
        );
    } else {
        assert!(why.contains("tmux runs on Unix-like systems only"), "{why}");
    }

    // The demo's session of this machine has no terminal; starting one fails as unavailable.
    let terminal = daemon.get(
        &format!("/v1/sessions/{}/terminal", id::SES1),
        Some(&device),
    );
    assert_eq!(terminal.status, 404, "{}", terminal.body);
    let work = tmp.path().join("work");
    std::fs::create_dir(&work).unwrap();
    let start = |body: &Value| daemon.post("/v1/sessions", Some(&device), body);
    let start_in = |cwd: &str| json!({ "machine": id::LAPTOP, "engine": "claude", "cwd": cwd });
    let started = start(&start_in(work.to_str().unwrap()));
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

    // Bounds: each one past its limit is `400 invalid`, before anything is looked up; a body
    // past 1 MiB too (not a bare 413).
    let invalid = |reply: common::Reply, what: &str| {
        assert_eq!(reply.status, 400, "{what}: {}", reply.body);
        assert_eq!(reply.code(), "invalid", "{what}");
    };
    let send_to = |body: &Value| {
        daemon.post(
            &format!("/v1/sessions/{}/send", id::SES1),
            Some(&device),
            body,
        )
    };
    invalid(
        send_to(&json!({ "text": "a".repeat(64 * 1024 + 1) })),
        "text",
    );
    let many = vec!["enter"; 65];
    invalid(
        daemon.post(
            &format!("/v1/sessions/{}/keys", id::SES1),
            Some(&device),
            &json!({ "keys": many }),
        ),
        "keys",
    );
    let mut long_brief = start_in(work.to_str().unwrap());
    long_brief["brief"] = json!("b".repeat(64 * 1024 + 1));
    invalid(start(&long_brief), "brief");
    let long_cwd = format!("/{}", "c".repeat(4096));
    invalid(start(&start_in(&long_cwd)), "cwd");
    let big = send_to(&json!({ "text": "a".repeat(1024 * 1024) }));
    invalid(big, "a body past 1 MiB");
    // At the limits, the bounds pass (and the session's lack of a terminal answers).
    let at = send_to(&json!({ "text": "a".repeat(64 * 1024) }));
    assert_eq!(at.status, 409, "{}", at.body);

    // Folders: relative, missing, or under one every user can write to are refused; under a
    // group-writable one, the start goes on (here to the missing runtime), and says so.
    invalid(start(&start_in("work")), "a relative cwd");
    let missing = tmp.path().join("none").join("work");
    invalid(start(&start_in(missing.to_str().unwrap())), "a missing cwd");
    // `..` after a folder that does not exist: Unix resolves it on disk, so the folder is
    // missing; Windows resolves it in the text, so it is `work`, and the start goes on (here to
    // the missing runtime).
    let through_missing = tmp.path().join("none").join("..").join("work");
    let through_missing = start(&start_in(through_missing.to_str().unwrap()));
    if cfg!(unix) {
        invalid(through_missing, "a cwd through a missing folder");
    } else {
        assert_eq!(through_missing.status, 503, "{}", through_missing.body);
        assert!(
            through_missing.body.contains("no terminal runtime"),
            "{}",
            through_missing.body
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let shared = tmp.path().join("shared");
        std::fs::create_dir_all(shared.join("w")).unwrap();
        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o777)).unwrap();
        let open = start(&start_in(shared.join("w").to_str().unwrap()));
        assert_eq!(open.status, 400, "{}", open.body);
        assert!(open.body.contains("changed by every user"), "{}", open.body);
        let team = tmp.path().join("team");
        std::fs::create_dir_all(team.join("w")).unwrap();
        std::fs::set_permissions(&team, std::fs::Permissions::from_mode(0o775)).unwrap();
        let shared_by_group = start(&start_in(team.join("w").to_str().unwrap()));
        assert_eq!(shared_by_group.status, 503, "{}", shared_by_group.body);
        assert!(
            shared_by_group.body.contains("no terminal runtime"),
            "{}",
            shared_by_group.body
        );
        let team = std::fs::canonicalize(&team).unwrap();
        daemon.wait_for_log("members of its group can change", WAIT);
        let logged = daemon.stderr();
        let line = logged
            .lines()
            .find(|l| l.contains("members of its group can change"))
            .unwrap();
        assert!(line.contains("INFO"), "{line}");
        assert!(line.contains(team.to_str().unwrap()), "{line}");
    }

    // Who may: a session run by another person's agent is refused, whatever the command, before
    // its lack of a terminal is; one run by the person's own agent is not.
    let kim = Member {
        id: MemberId::new(),
        kind: MemberKind::Human,
        handle: "@kim".into(),
        name: "Kim".into(),
        owner: None,
        persona: None,
    };
    let kims = Member {
        id: MemberId::new(),
        kind: MemberKind::Agent,
        handle: "@kimbot".into(),
        name: "Kimbot".into(),
        owner: Some(kim.id),
        persona: None,
    };
    let theirs = session_of(Some(kims.id));
    let mine = session_of(Some(id::WRITER.parse().unwrap()));
    append_with_projections(
        &state,
        vec![
            EventBody::MemberAdded {
                member: kim.clone(),
            },
            EventBody::MemberAdded {
                member: kims.clone(),
            },
            EventBody::SessionDiscovered {
                session: theirs.clone(),
            },
            EventBody::SessionDiscovered {
                session: mine.clone(),
            },
        ],
    );
    for (path, body) in [
        ("send", json!({ "text": "hello" })),
        ("keys", json!({ "keys": ["enter"] })),
        ("interrupt", json!({})),
        ("end", json!({ "mode": "kill" })),
    ] {
        let refused = daemon.post(
            &format!("/v1/sessions/{}/{path}", theirs.id),
            Some(&device),
            &body,
        );
        assert_eq!(refused.status, 403, "{path}: {}", refused.body);
        let allowed = daemon.post(
            &format!("/v1/sessions/{}/{path}", mine.id),
            Some(&device),
            &body,
        );
        assert_eq!(allowed.status, 409, "{path}: {}", allowed.body);
    }

    // Device routes: an agent may not use any of them.
    let agent = daemon.agent_token();
    for (path, body) in [
        ("/v1/sessions".to_owned(), start_in(work.to_str().unwrap())),
        (
            format!("/v1/sessions/{}/send", mine.id),
            json!({ "text": "x" }),
        ),
        (
            format!("/v1/sessions/{}/keys", mine.id),
            json!({ "keys": ["enter"] }),
        ),
        (format!("/v1/sessions/{}/interrupt", mine.id), json!({})),
        (
            format!("/v1/sessions/{}/end", mine.id),
            json!({ "mode": "kill" }),
        ),
    ] {
        let reply = daemon.post(&path, Some(&agent), &body);
        assert_eq!(reply.status, 403, "{path}: {}", reply.body);
    }
}

#[cfg(unix)]
mod tmux {
    use super::WAIT;
    use super::common::{Daemon, Frame, Tmux, Ws, id};
    use serde_json::{Value, json};
    use std::ffi::OsString;
    use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
    use std::path::{Path, PathBuf};
    use std::process::{Command, Output};
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    /// The variable that marks every process a test's daemon starts.
    const MARK: &str = "PITCREW_TEST_RUN";

    /// The Claude fixture's own session id, which its lines carry.
    const FIXTURE_ID: &str = "2b6f1a8e-4c1d-4f5e-9a37-0c8d1e2f3a4b";

    /// A stand-in for Claude Code: like Claude given a first prompt, it writes its transcript at
    /// once, for the session id it was given (`--session-id=`), in its Claude home
    /// (`CLAUDE_CONFIG_DIR`, which the daemon's tmux server passes on); then it shows each byte it
    /// reads as a line, `KEY <hex>`. Ctrl-C ends it (`FAKE CLAUDE BYE`).
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

    /// The tmux tests run one at a time, as the runtime's own do: several daemons, each with
    /// tmux servers, at once on a loaded machine is where tmux was seen to drop a client before
    /// its first answer.
    static SERIAL: Mutex<()> = Mutex::new(());

    /// One test's setup: homes, a working folder, the stand-in `claude`, and a private tmux
    /// socket. Dropped, it kills every tmux server it knows of and anything still carrying its
    /// mark (while it still holds [`SERIAL`]).
    struct Rig {
        tmp: tempfile::TempDir,
        homes: PathBuf,
        work: PathBuf,
        bin: PathBuf,
        socket: PathBuf,
        tmux: PathBuf,
        mark: String,
        /// Every socket a server may run on, killed at the end.
        sockets: Mutex<Vec<PathBuf>>,
        _serial: std::sync::MutexGuard<'static, ()>,
    }

    impl Rig {
        /// `None`, after saying why, where tmux 3.2 or newer is not installed; a failure instead
        /// when `PITCREW_REQUIRE_TMUX=1`.
        fn new() -> Option<Self> {
            let serial = SERIAL
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let tmp = tempfile::tempdir().unwrap();
            // Short, for the socket path limit; its directory is made private by the daemon.
            let socket = tmp.path().join("t").join("s");
            let tmux = match usable_tmux(&socket) {
                Ok(tmux) => tmux,
                Err(why) => {
                    let required = std::env::var("PITCREW_REQUIRE_TMUX").is_ok_and(|v| v == "1");
                    assert!(
                        !required,
                        "PITCREW_REQUIRE_TMUX=1, but the daemon's tmux terminals cannot be \
                         tested: {why}"
                    );
                    eprintln!("skipped: the daemon's tmux terminals need tmux 3.2 or newer: {why}");
                    return None;
                }
            };
            let homes = tmp.path().join("homes");
            let work = tmp.path().join("work");
            let bin = tmp.path().join("bin");
            for dir in [&homes, &work, &bin] {
                std::fs::create_dir_all(dir).unwrap();
            }
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
            let script = FAKE_CLAUDE.replace("@TEMPLATE@", template.to_str().unwrap());
            std::fs::write(&claude, script).unwrap();
            std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            Some(Self {
                homes,
                work,
                bin,
                sockets: Mutex::new(vec![socket.clone()]),
                socket,
                tmux,
                mark: format!("terminals-{}-{nanos}", std::process::id()),
                tmp,
                _serial: serial,
            })
        }

        fn root(&self) -> &Path {
            self.tmp.path()
        }

        /// A daemon on this rig's state, homes and tmux socket.
        fn start(&self, extra: &[&str]) -> Daemon {
            self.start_on(
                &self.root().join("state"),
                &self.homes,
                extra,
                Tmux::At(&self.socket),
                &[],
            )
        }

        /// A daemon on `state`, watching `homes` (its stand-in writes there), its terminals on
        /// `tmux`, with the stand-in first on its `PATH`, this rig's mark, and `env`.
        fn start_on(
            &self,
            state: &Path,
            homes: &Path,
            extra: &[&str],
            tmux: Tmux<'_>,
            env: &[(&str, OsString)],
        ) -> Daemon {
            let claude = homes.join(".claude");
            std::fs::create_dir_all(claude.join("projects")).unwrap();
            let mut path = OsString::from(&self.bin);
            path.push(":");
            path.push(std::env::var_os("PATH").unwrap_or_default());
            let mut args = extra.to_vec();
            args.extend(["--homes", homes.to_str().unwrap()]);
            let mut all = vec![
                ("PATH", path),
                (MARK, OsString::from(&self.mark)),
                ("CLAUDE_CONFIG_DIR", claude.into_os_string()),
            ];
            all.extend(env.iter().cloned());
            Daemon::start_with(state, &args, &all, tmux)
        }

        /// `tmux -S <socket> <args>`; the server it may start carries this rig's mark.
        fn tmux_on(&self, socket: &Path, args: &[&str]) -> Output {
            self.sockets.lock().unwrap().push(socket.to_path_buf());
            Command::new(&self.tmux)
                .arg("-S")
                .arg(socket)
                .args(args)
                .env_remove("TMUX")
                .env(MARK, &self.mark)
                .output()
                .unwrap()
        }

        /// `tmux -S <this rig's socket> <args>`.
        fn tmux(&self, args: &[&str]) -> Output {
            self.tmux_on(&self.socket, args)
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
            let sockets = self.sockets.lock().unwrap().clone();
            for socket in sockets {
                let _ = Command::new(&self.tmux)
                    .arg("-S")
                    .arg(&socket)
                    .arg("kill-server")
                    .env_remove("TMUX")
                    .output();
            }
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

    /// `POST /v1/sessions` for the stand-in in `cwd`: the session, once the runner has found it
    /// in its terminal.
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

    /// The windows of the server on `socket`: `<window id> <terminal tag> <name>`.
    fn windows(rig: &Rig, socket: &Path) -> Vec<String> {
        let out = rig.tmux_on(
            socket,
            &[
                "list-panes",
                "-a",
                "-F",
                "#{window_id} #{@pitcrew-terminal} #{window_name}",
            ],
        );
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::to_owned)
            .collect()
    }

    /// The tmux window id (`@<n>`) of a session's terminal, on the rig's own socket.
    fn window_of(rig: &Rig, session: &Value) -> String {
        let terminal = terminal_tag(session);
        let all = windows(rig, &rig.socket);
        all.iter()
            .find_map(|line| {
                let mut parts = line.splitn(3, ' ');
                let (window, tag) = (parts.next()?, parts.next()?);
                (tag == terminal).then(|| window.to_owned())
            })
            .unwrap_or_else(|| panic!("no window for terminal {terminal}: {all:?}"))
    }

    /// A session's terminal as tmux's tag holds it (`term_<ulid>`).
    fn terminal_tag(session: &Value) -> String {
        let terminal = session["terminal"].as_str().unwrap();
        let id: pitcrew_protocol::ids::TerminalId = terminal.parse().unwrap();
        id.to_string()
    }

    /// The tmux socket a daemon says its terminals run on.
    fn socket_in_log(daemon: &Daemon) -> PathBuf {
        let logs = daemon.stderr();
        let start = logs
            .find("attach to them with `tmux -S ")
            .unwrap_or_else(|| panic!("no tmux socket in the log:\n{logs}"))
            + "attach to them with `tmux -S ".len();
        let end = logs[start..].find(" attach").unwrap() + start;
        PathBuf::from(&logs[start..end])
    }

    /// The whole life of a session PitCrew starts in tmux: started through the API, streamed,
    /// typed into; the daemon stops and the terminal keeps running with its offset stored; the
    /// next daemon finds it and streams on from that offset; `end` kills it, or ends it
    /// gracefully. A start for another machine, or with what the runner refuses, starts nothing.
    /// Nothing is left behind.
    #[test]
    fn sessions_run_in_tmux_terminals_that_outlive_the_daemon() {
        let Some(rig) = Rig::new() else {
            return;
        };
        let mut daemon = rig.start(&["--demo"]);
        let device = daemon.device_token();
        let info = daemon.get("/v1/host/info", None).json();
        assert_eq!(
            info["capabilities"],
            json!(["tmux", "watch", "scan"]),
            "{info}"
        );
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
        let work = rig.work.to_str().unwrap().to_owned();
        let a = start_claude(&daemon, &work, &device);
        let a_id = a["id"].as_str().unwrap().to_owned();
        let native = a["native_id"].as_str().unwrap().to_owned();
        assert_eq!(native.len(), 36, "Claude's session id: {a}");
        assert_eq!(a["machine"], id::LAPTOP);
        assert_eq!(a["engine"], "claude");
        assert!(a["terminal"].is_string(), "{a}");
        // People can find it in tmux themselves.
        let listed = windows(&rig, &rig.socket);
        assert_eq!(listed.len(), 1, "{listed:?}");
        assert!(listed[0].ends_with(" claude work"), "{listed:?}");

        // Starts that must start nothing: another machine of the workspace; a permission mode
        // the runner refuses; a model that reads as an option.
        let refused = [
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
        ];
        for (body, status) in refused {
            let reply = daemon.post("/v1/sessions", Some(&device), &body);
            assert_eq!(reply.status, status, "{body}: {}", reply.body);
        }
        assert_eq!(windows(&rig, &rig.socket), listed, "nothing more started");

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
        assert_eq!(
            info["capabilities"],
            json!(["tmux", "watch", "scan"]),
            "{info}"
        );
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

        // A second session, its folder given with `..` (resolved, and named after where it
        // leads), ended gracefully: Ctrl-C twice, and the CLI exits.
        let dotted = format!("{work}/../work");
        let b = start_claude(&daemon, &dotted, &device);
        let b_id = b["id"].as_str().unwrap().to_owned();
        assert_ne!(b_id, a_id);
        let b_target = format!("pitcrew:{}", window_of(&rig, &b));
        let path = rig.tmux(&[
            "display-message",
            "-p",
            "-t",
            &b_target,
            "#{pane_current_path} #{window_name}",
        ]);
        let real = std::fs::canonicalize(&rig.work).unwrap();
        assert_eq!(
            String::from_utf8_lossy(&path.stdout).trim(),
            format!("{} claude work", real.display()),
            "{path:?}"
        );
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

        // A third, in a folder under a group-writable one (a shared project): it starts, and the
        // log names that folder at info. Killed at once.
        let team = rig.root().join("team");
        std::fs::create_dir_all(team.join("work")).unwrap();
        std::fs::set_permissions(&team, std::fs::Permissions::from_mode(0o2775)).unwrap();
        let c = start_claude(&daemon, team.join("work").to_str().unwrap(), &device);
        let c_id = c["id"].as_str().unwrap().to_owned();
        let logs = daemon.stderr();
        let line = logs
            .lines()
            .find(|l| l.contains("members of its group can change"))
            .unwrap_or_else(|| panic!("no group-writable folder in the log:\n{logs}"));
        let team = std::fs::canonicalize(&team).unwrap();
        assert!(line.contains("INFO"), "{line}");
        assert!(line.contains(team.to_str().unwrap()), "{line}");
        post(
            &daemon,
            &format!("/v1/sessions/{c_id}/end"),
            &device,
            &json!({ "mode": "kill" }),
        );
        eventually("the third session ends", || {
            session(&daemon, &device, &c_id)["state"] == "ended"
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

    /// One tmux server per state directory: two daemons of one user, on their default sockets,
    /// run their terminals on two servers, each holding only its own; a third daemon given the
    /// first's socket finds it locked and runs without terminals.
    #[test]
    fn each_state_directory_has_a_tmux_server_of_its_own() {
        let Some(rig) = Rig::new() else {
            return;
        };
        // The runtime's per-user directory, inside this test's temporary folder.
        let tmpdir = rig.root().join("td");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&tmpdir)
            .unwrap();
        let env = [("TMUX_TMPDIR", tmpdir.clone().into_os_string())];
        let start = |name: &str| {
            rig.start_on(
                &rig.root().join(name),
                &rig.root().join(format!("{name}-homes")),
                &["--demo"],
                Tmux::StateDefault,
                &env,
            )
        };
        let (mut a, mut b) = (start("a"), start("b"));
        let (sa, sb) = (socket_in_log(&a), socket_in_log(&b));
        rig.sockets.lock().unwrap().extend([sa.clone(), sb.clone()]);
        assert_ne!(sa, sb);
        for (daemon, socket) in [(&a, &sa), (&b, &sb)] {
            let info = daemon.get("/v1/host/info", None).json();
            assert_eq!(
                info["capabilities"],
                json!(["tmux", "watch", "scan"]),
                "{info}"
            );
            assert!(socket.starts_with(&tmpdir), "{}", socket.display());
            let dir = socket
                .parent()
                .unwrap()
                .file_name()
                .unwrap()
                .to_str()
                .unwrap();
            assert!(
                dir.len() == 8 && dir.bytes().all(|c| c.is_ascii_hexdigit()),
                "{dir}"
            );
        }

        // Each starts a session: each server holds its own terminal, and only that one.
        let work = rig.work.to_str().unwrap().to_owned();
        let in_a = start_claude(&a, &work, &a.device_token());
        let in_b = start_claude(&b, &work, &b.device_token());
        let on_a = windows(&rig, &sa);
        let on_b = windows(&rig, &sb);
        assert_eq!(on_a.len(), 1, "{on_a:?}");
        assert_eq!(on_b.len(), 1, "{on_b:?}");
        assert!(on_a[0].contains(&terminal_tag(&in_a)), "{on_a:?}");
        assert!(on_b[0].contains(&terminal_tag(&in_b)), "{on_b:?}");

        // A third daemon pointed at the first's socket: locked, so no terminals for it.
        let c = rig.start_on(
            &rig.root().join("c"),
            &rig.root().join("c-homes"),
            &["--demo"],
            Tmux::At(&sa),
            &[],
        );
        let info = c.get("/v1/host/info", None).json();
        assert_eq!(info["capabilities"], json!(["watch", "scan"]), "{info}");
        let logs = c.stderr();
        assert!(
            logs.contains("another pitcrewd uses this tmux socket"),
            "{logs}"
        );
        assert_eq!(
            windows(&rig, &sa),
            on_a,
            "the first's server was not touched"
        );
        drop(c);

        for (daemon, session) in [(&a, &in_a), (&b, &in_b)] {
            let id = session["id"].as_str().unwrap();
            post(
                daemon,
                &format!("/v1/sessions/{id}/end"),
                &daemon.device_token(),
                &json!({ "mode": "kill" }),
            );
        }
        a.stop();
        b.stop();
        drop((a, b));
        let left = rig.left_after(WAIT);
        assert!(left.is_empty(), "left behind: {left:?}");
    }

    /// A tmux server that has sessions of someone else's (a person's own, say) is not
    /// PitCrew's: the daemon runs without terminals, and leaves it as it was.
    #[test]
    fn a_tmux_server_with_other_sessions_is_refused() {
        let Some(rig) = Rig::new() else {
            return;
        };
        let dir = rig.root().join("f");
        std::fs::DirBuilder::new().mode(0o700).create(&dir).unwrap();
        let socket = dir.join("s");
        let made = rig.tmux_on(
            &socket,
            &[
                "-f",
                "/dev/null",
                "new-session",
                "-d",
                "-s",
                "mine",
                "sleep 600",
            ],
        );
        assert!(made.status.success(), "{made:?}");

        let mut daemon = rig.start_on(
            &rig.root().join("state"),
            &rig.homes,
            &["--demo"],
            Tmux::At(&socket),
            &[],
        );
        let info = daemon.get("/v1/host/info", None).json();
        assert_eq!(info["capabilities"], json!(["watch", "scan"]), "{info}");
        let logs = daemon.stderr();
        assert!(logs.contains("that are not PitCrew's"), "{logs}");
        let sessions = rig.tmux_on(&socket, &["list-sessions", "-F", "#{session_name}"]);
        assert_eq!(String::from_utf8_lossy(&sessions.stdout).trim(), "mine");
        daemon.stop();
        drop(daemon);
        let sessions = rig.tmux_on(&socket, &["list-sessions", "-F", "#{session_name}"]);
        assert_eq!(String::from_utf8_lossy(&sessions.stdout).trim(), "mine");
    }
}
