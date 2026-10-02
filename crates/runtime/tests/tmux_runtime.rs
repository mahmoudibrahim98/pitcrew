//! `TmuxRuntime` on a real tmux (3.2 or newer; skipped where tmux is missing).
//!
//! Every test runs its own server on a private socket (`/tmp/pc-<hex>/s`, mode 0700), never the
//! user's. Every process a test starts carries `PITCREW_TEST_RUN=<its mark>` (the runtime passes
//! it to tmux, whose server and panes inherit it). Each test ends by checking that, with its
//! runtime dropped, no control client and no runtime thread is left, then kills its server and
//! checks through `/proc` that nothing with its mark is left. The tests run one at a time, so
//! the thread check sees only its own test.
//!
//! The throughput measurement is ignored by default:
//! `cargo test -p pitcrew-runtime --test tmux_runtime -- --ignored --nocapture`.

#![cfg(unix)]

use std::collections::hash_map::RandomState;
use std::future::Future;
use std::hash::BuildHasher;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::{Context, Poll, Wake};
use std::time::{Duration, Instant, SystemTime};

use pitcrew_interfaces::runtime::{Runtime, RuntimeError, StartSpec};
use pitcrew_protocol::ids::TerminalId;
use pitcrew_protocol::runner::{Capability, Key};
use pitcrew_runtime::detect::{DetectError, detect_tmux};
use pitcrew_runtime::tmux::{self, OFFSET_OPTION, TERMINAL_OPTION, TmuxOptions, TmuxRuntime};

/// Generous: other agents build on this machine at the same time.
const WAIT: Duration = Duration::from_secs(30);
const MARK: &str = "PITCREW_TEST_RUN";

static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(PoisonError::into_inner)
}

struct Fixture {
    dir: PathBuf,
    options: TmuxOptions,
    mark: String,
    finished: bool,
    _serial: MutexGuard<'static, ()>,
}

impl Fixture {
    /// A private server's setting, or `None` (the test is skipped) without a usable tmux.
    fn new(test: &str) -> Option<Self> {
        let serial = serial();
        match detect_tmux("tmux") {
            Err(DetectError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
                eprintln!("skipped: tmux is not installed");
                return None;
            }
            Err(DetectError::Unsupported(version)) => {
                eprintln!("skipped: tmux {version} is below the supported floor");
                return None;
            }
            Err(e) => panic!("cannot probe tmux: {e}"),
            Ok(_) => {}
        }
        let random = RandomState::new().hash_one((std::process::id(), SystemTime::now(), test));
        let dir = PathBuf::from(format!("/tmp/pc-{:012x}", random & 0xffff_ffff_ffff));
        // Not create_all: the directory must be new, and so ours.
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&dir)
            .expect("private directory");
        let mark = format!("{test}-{}-{random:x}", std::process::id());
        let mut options = TmuxOptions::new(dir.join("s"));
        options.env.push((MARK.into(), mark.clone()));
        options.call_timeout = Duration::from_secs(20);
        options.start_timeout = Duration::from_secs(30);
        Some(Self {
            dir,
            options,
            mark,
            finished: false,
            _serial: serial,
        })
    }

    fn runtime(&self) -> TmuxRuntime {
        TmuxRuntime::new(self.options.clone()).expect("runtime")
    }

    fn spec(&self, name: &str, program: &str, args: &[&str]) -> StartSpec {
        StartSpec {
            program: program.into(),
            args: args.iter().map(|a| (*a).to_owned()).collect(),
            cwd: self.dir.display().to_string(),
            env: Vec::new(),
            name: name.into(),
            cols: 80,
            rows: 24,
        }
    }

    /// A tmux client for this test's server only.
    fn tmux(&self, args: &[&str]) -> String {
        let out = Command::new("tmux")
            .arg("-S")
            .arg(&self.options.socket)
            .args(args)
            .env_remove("TMUX")
            .env(MARK, &self.mark)
            .stdin(Stdio::null())
            .output()
            .expect("run tmux");
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }

    fn server_running(&self) -> bool {
        Command::new("tmux")
            .arg("-S")
            .arg(&self.options.socket)
            .arg("list-sessions")
            .env_remove("TMUX")
            .env(MARK, &self.mark)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }

    fn wait_for_no_server(&self) {
        let deadline = Instant::now() + WAIT;
        while self.server_running() {
            assert!(
                Instant::now() < deadline,
                "the tmux server is still running"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn pane_option(&self, pane: &str, option: &str) -> String {
        self.tmux(&["show-options", "-p", "-v", "-t", pane, option])
    }

    /// `(pane id, tag)` for every pane of the session.
    fn tags(&self) -> Vec<(String, String)> {
        self.tmux(&[
            "list-panes",
            "-s",
            "-t",
            "pitcrew",
            "-F",
            "#{pane_id} #{@pitcrew-terminal}",
        ])
        .lines()
        .map(|line| {
            let (pane, tag) = line.split_once(' ').unwrap_or((line, ""));
            (pane.to_owned(), tag.to_owned())
        })
        .collect()
    }

    fn pane_of(&self, id: TerminalId) -> String {
        let id = id.to_string();
        self.tags()
            .into_iter()
            .find(|(_, tag)| *tag == id)
            .map(|(pane, _)| pane)
            .expect("the terminal's pane")
    }

    /// With every runtime of the test dropped: no control client or runtime thread is left
    /// (before the server is killed); then kills the server, and checks nothing with this
    /// test's mark is left.
    fn finish(mut self) {
        self.finished = true;
        let deadline = Instant::now() + WAIT;
        loop {
            let clients = children_marked(&self.mark);
            let threads = runtime_threads();
            if clients.is_empty() && threads.is_empty() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "left behind by a dropped runtime: clients {clients:?}, threads {threads:?}"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        let left = self.teardown();
        assert!(left.is_empty(), "processes left behind: {left:?}");
        assert!(!self.dir.exists(), "the private directory is left");
    }

    fn teardown(&self) -> Vec<(u32, String)> {
        self.tmux(&["kill-server"]);
        let left = sweep(&self.mark, Duration::from_secs(10));
        let _ = std::fs::remove_dir_all(&self.dir);
        left
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if !self.finished {
            self.teardown();
        }
    }
}

/// Live processes carrying `mark` (a zombie has no environment, so it does not count).
fn marked(mark: &str) -> Vec<(u32, String)> {
    let want = format!("{MARK}={mark}");
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .filter_map(|entry| {
            let pid: u32 = entry.ok()?.file_name().to_str()?.parse().ok()?;
            let env = std::fs::read(format!("/proc/{pid}/environ")).ok()?;
            if !env.split(|b| *b == 0).any(|v| v == want.as_bytes()) {
                return None;
            }
            let cmd = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
            Some((
                pid,
                String::from_utf8_lossy(&cmd)
                    .replace('\0', " ")
                    .trim()
                    .to_owned(),
            ))
        })
        .collect()
}

/// Marked processes this test process started itself: tmux control clients (the server and
/// the panes are not its children).
fn children_marked(mark: &str) -> Vec<(u32, String)> {
    let me = std::process::id().to_string();
    marked(mark)
        .into_iter()
        .filter(|(pid, _)| {
            std::fs::read_to_string(format!("/proc/{pid}/stat"))
                .ok()
                .and_then(|stat| {
                    let after = stat.rsplit_once(')')?.1.to_owned();
                    after.split_whitespace().nth(1).map(str::to_owned)
                })
                .is_some_and(|ppid| ppid == me)
        })
        .collect()
}

/// This process's threads that belong to a runtime (named `pitcrew-tmux-…`).
fn runtime_threads() -> Vec<String> {
    std::fs::read_dir("/proc/self/task")
        .map(|tasks| {
            tasks
                .flatten()
                .filter_map(|task| std::fs::read_to_string(task.path().join("comm")).ok())
                .map(|comm| comm.trim().to_owned())
                .filter(|comm| comm.starts_with("pitcrew-tmux"))
                .collect()
        })
        .unwrap_or_default()
}

/// Waits up to `grace` for `mark`'s processes to end, then kills the rest and returns them.
fn sweep(mark: &str, grace: Duration) -> Vec<(u32, String)> {
    let start = Instant::now();
    loop {
        let left = marked(mark);
        if left.is_empty() || start.elapsed() >= grace {
            for (pid, _) in &left {
                signal(*pid, rustix::process::Signal::KILL);
            }
            return left;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn signal(pid: u32, signal: rustix::process::Signal) {
    if let Some(pid) = i32::try_from(pid)
        .ok()
        .and_then(rustix::process::Pid::from_raw)
    {
        let _ = rustix::process::kill_process(pid, signal);
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    needle.is_empty() || haystack.windows(needle.len()).any(|w| w == needle)
}

/// Waits until the output from `from` contains `needle`; returns that output and its end.
fn wait_for(rt: &TmuxRuntime, id: TerminalId, from: u64, needle: &[u8]) -> (Vec<u8>, u64) {
    let deadline = Instant::now() + WAIT;
    loop {
        let chunk = rt.read_output(id, from, usize::MAX).expect("read output");
        if contains(&chunk.data, needle) {
            return (chunk.data, chunk.end);
        }
        let shown = || String::from_utf8_lossy(&chunk.data).into_owned();
        let left = deadline
            .checked_duration_since(Instant::now())
            .unwrap_or_else(|| panic!("no {:?} in {:?}", String::from_utf8_lossy(needle), shown()));
        let end = rt.wait_for_output(id, chunk.end, left).expect("wait");
        if end == chunk.end && !rt.info(id).expect("info").alive {
            panic!(
                "ended without {:?}: {:?}",
                String::from_utf8_lossy(needle),
                shown()
            );
        }
    }
}

/// Waits until the terminal's program has ended.
fn wait_dead(rt: &TmuxRuntime, id: TerminalId) {
    let deadline = Instant::now() + WAIT;
    while rt.info(id).expect("info").alive {
        let left = deadline
            .checked_duration_since(Instant::now())
            .expect("the program did not end");
        let end = rt.read_output(id, u64::MAX, 0).expect("end").end;
        rt.wait_for_output(id, end, left.min(Duration::from_millis(200)))
            .expect("wait");
    }
}

/// Polls `check` until it holds, for at most [`WAIT`].
fn eventually(what: &str, mut check: impl FnMut() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !check() {
        assert!(Instant::now() < deadline, "never: {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn crlf(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).replace("\r\n", "\n")
}

#[test]
fn write_read_by_offset_resize_ctrl_c_and_kill() {
    let Some(fx) = Fixture::new("basic") else {
        return;
    };
    let rt = fx.runtime();
    assert!(rt.list().expect("list without a server").is_empty());
    let t = rt
        .start(&fx.spec("round trip", "sh", &["-c", "printf ready; cat"]))
        .expect("start");
    assert!(t.alive);
    assert_eq!(t.name, "round trip");
    assert!(t.pid.is_some());
    let target = t.native_target.clone().expect("target");
    assert!(target.starts_with("pitcrew:@"), "{target}");
    let (_, ready) = wait_for(&rt, t.id, 0, b"ready");

    rt.write(t.id, b"hello\r").expect("write");
    // The terminal echoes the line, then cat copies it.
    let (typed, _) = wait_for(&rt, t.id, ready, b"hello\r\nhello\r\n");
    assert_eq!(typed, b"hello\r\nhello\r\n");
    let chunk = rt.read_output(t.id, 0, 5).expect("read");
    assert_eq!(
        (chunk.offset, chunk.data.as_slice(), chunk.truncated),
        (0, &b"ready"[..], false)
    );
    let chunk = rt.read_output(t.id, 2, 3).expect("read");
    assert_eq!((chunk.offset, chunk.data.as_slice()), (2, &b"ady"[..]));
    assert_eq!(chunk.end, ready + 14);
    let past = rt
        .read_output(t.id, chunk.end + 100, 10)
        .expect("read past the end");
    assert_eq!(
        (past.offset, past.data.len(), past.end),
        (chunk.end, 0, chunk.end)
    );
    let keys_from = chunk.end;
    rt.send_keys(t.id, &[Key::Tab, Key::Enter]).expect("keys");
    wait_for(&rt, t.id, keys_from, b"\t\r\n\t\r\n");

    rt.resize(t.id, 100, 30).expect("resize");
    let screen = rt.screen(t.id).expect("screen");
    assert_eq!((screen.cols, screen.rows.len()), (100, 30));
    assert_eq!(screen.rows[0], "readyhello");
    assert_eq!(
        fx.tmux(&[
            "list-panes",
            "-t",
            &target,
            "-F",
            "#{pane_width}x#{pane_height}"
        ]),
        "100x30"
    );
    assert!(matches!(
        rt.resize(t.id, 0, 30),
        Err(RuntimeError::Io(e)) if e.kind() == std::io::ErrorKind::InvalidInput
    ));

    // The program itself sees the new size.
    let sized = rt
        .start(&fx.spec(
            "size",
            "sh",
            &["-c", "printf ready; read x; stty size; cat"],
        ))
        .expect("start");
    let (_, at) = wait_for(&rt, sized.id, 0, b"ready");
    rt.resize(sized.id, 90, 33).expect("resize");
    rt.write(sized.id, b"\r").expect("write");
    wait_for(&rt, sized.id, at, b"33 90");

    // Ctrl-C ends cat and its shell; the output stays readable.
    rt.send_keys(t.id, &[Key::CtrlC]).expect("ctrl-c");
    wait_dead(&rt, t.id);
    assert!(!rt.info(t.id).expect("info").alive);
    assert_eq!(rt.read_output(t.id, 0, 5).expect("read").data, b"ready");
    assert!(rt.write(t.id, b"x").is_err());
    assert!(
        rt.kill(t.id).is_ok(),
        "killing an ended terminal is a no-op"
    );

    rt.kill(sized.id).expect("kill the last terminal");
    assert!(!rt.info(sized.id).expect("info").alive);
    // Its window was the last one: the server has ended with it.
    fx.wait_for_no_server();
    let listed = rt.list().expect("list");
    assert_eq!(listed.len(), 2);
    assert!(listed.iter().all(|t| !t.alive));
    let unknown = TerminalId::new();
    assert!(matches!(rt.info(unknown), Err(RuntimeError::NotFound(id)) if id == unknown));
    assert!(matches!(
        rt.read_output(unknown, 0, 1),
        Err(RuntimeError::NotFound(_))
    ));
    drop(rt);
    fx.finish();
}

#[test]
fn a_new_runtime_finds_the_terminal_and_resumes_at_the_last_offset() {
    let Some(fx) = Fixture::new("restart") else {
        return;
    };
    let rt = fx.runtime();
    let t = rt
        .start(&fx.spec("survivor", "sh", &["-c", "printf ready; cat"]))
        .expect("start");
    wait_for(&rt, t.id, 0, b"ready");
    rt.write(t.id, b"one\r").expect("write");
    let (_, end) = wait_for(&rt, t.id, 0, b"one\r\none\r\n");
    let pane = fx.pane_of(t.id);
    // While running, the stored offset stays ahead of what readers have seen.
    let stored: u64 = fx
        .pane_option(&pane, OFFSET_OPTION)
        .parse()
        .expect("stored offset");
    assert!(stored > end, "{stored} <= {end}");
    drop(rt);
    // Dropping stores the exact end, and leaves the terminal running.
    assert_eq!(fx.pane_option(&pane, OFFSET_OPTION), end.to_string());

    let rt = fx.runtime();
    let listed = rt.list().expect("list");
    assert_eq!(listed.len(), 1, "{listed:?}");
    assert_eq!(listed[0].id, t.id);
    assert!(listed[0].alive);
    assert_eq!(listed[0].name, "survivor");
    assert_eq!(listed[0].native_target, t.native_target);
    let resumed = rt.read_output(t.id, end, 100).expect("read");
    assert_eq!(
        (
            resumed.offset,
            resumed.data.len(),
            resumed.end,
            resumed.truncated
        ),
        (end, 0, end, false)
    );
    let before = rt.read_output(t.id, 0, 100).expect("read");
    assert!(before.truncated);
    assert_eq!(before.offset, end);
    rt.write(t.id, b"two\r").expect("write after restart");
    let (more, _) = wait_for(&rt, t.id, end, b"two\r\ntwo\r\n");
    assert_eq!(more, b"two\r\ntwo\r\n");
    // The screen model starts empty after a restart and follows new output.
    assert!(
        rt.screen(t.id)
            .expect("screen")
            .rows
            .iter()
            .any(|r| r.contains("two"))
    );
    rt.kill(t.id).expect("kill");
    drop(rt);
    fx.finish();
}

#[test]
fn the_screen_shows_a_prompt_drawn_with_cursor_movement() {
    let Some(fx) = Fixture::new("screen") else {
        return;
    };
    let rt = fx.runtime();
    let draw = r"printf '\033[2J\033[5;10Hprompt> \033[1;1Htop\rT\033[2;1Hmenu: \033[7mone\033[0m two\033[K\033[5;18H'; exec cat";
    let t = rt
        .start(&fx.spec("screen", "sh", &["-c", draw]))
        .expect("start");
    wait_for(&rt, t.id, 0, b"\x1b[5;18H");
    let screen = rt.screen(t.id).expect("screen");
    assert_eq!(screen.cols, 80);
    assert_eq!(screen.rows.len(), 24);
    assert_eq!(screen.rows[0], "Top");
    assert_eq!(screen.rows[1], "menu: one two");
    assert_eq!(screen.rows[4], "         prompt>");
    assert!(screen.rows[5..].iter().all(String::is_empty));
    assert_eq!((screen.cursor_row, screen.cursor_col), (4, 17));
    // Typing at the prompt lands where the cursor is.
    rt.write(t.id, b"go").expect("write");
    wait_for(&rt, t.id, 0, b"go");
    assert_eq!(
        rt.screen(t.id).expect("screen").rows[4],
        "         prompt> go"
    );
    rt.kill(t.id).expect("kill");
    drop(rt);
    fx.finish();
}

#[test]
fn a_flood_of_huge_counts_neither_slows_the_screen_nor_stops_others() {
    let Some(fx) = Fixture::new("flood") else {
        return;
    };
    let rt = fx.runtime();
    // 8200 times `ESC[65535L` is 64 KiB; vt100 alone would insert 65535 lines each time.
    let flood = rt
        .start(&fx.spec(
            "flood",
            "sh",
            &[
                "-c",
                r"i=0; while [ $i -lt 8200 ]; do printf '\033[65535L'; i=$((i+1)); done; printf '\033[1;1HFLOODED'; exec cat",
            ],
        ))
        .expect("start flood");
    let steady = rt
        .start(&fx.spec("steady", "sh", &["-c", "printf ready; exec cat"]))
        .expect("start steady");
    let (_, flooded) = wait_for(&rt, flood.id, 0, b"FLOODED");
    assert!(flooded >= 64 << 10, "{flooded}");
    let (_, ready) = wait_for(&rt, steady.id, 0, b"ready");
    let took = std::thread::scope(|scope| {
        let screen = scope.spawn(|| {
            let started = Instant::now();
            let screen = rt.screen(flood.id).expect("screen");
            (started.elapsed(), screen)
        });
        // Meanwhile the other terminal's output keeps flowing.
        rt.write(steady.id, b"tick\r").expect("write");
        wait_for(&rt, steady.id, ready, b"tick\r\ntick\r\n");
        screen.join().expect("screen thread")
    });
    assert!(
        took.0 < Duration::from_secs(1),
        "screen() took {:?}",
        took.0
    );
    assert_eq!(took.1.rows[0], "FLOODED");
    rt.kill(flood.id).expect("kill");
    rt.kill(steady.id).expect("kill");
    drop(rt);
    fx.finish();
}

#[test]
fn two_terminals_interleave_without_mixing() {
    let Some(fx) = Fixture::new("two") else {
        return;
    };
    let rt = fx.runtime();
    let count = |tag: &str| {
        format!(
            "i=0; while [ $i -lt 400 ]; do printf '{tag}%04d\\n' $i; i=$((i+1)); done; printf DONE; exec cat"
        )
    };
    let (a, b) = (count("A"), count("B"));
    let ta = rt.start(&fx.spec("a", "sh", &["-c", &a])).expect("start a");
    let tb = rt.start(&fx.spec("b", "sh", &["-c", &b])).expect("start b");
    for (id, tag) in [(ta.id, "A"), (tb.id, "B")] {
        let (out, _) = wait_for(&rt, id, 0, b"DONE");
        let text = crlf(&out);
        let lines: Vec<&str> = text.trim_end_matches("DONE").lines().collect();
        let expected: Vec<String> = (0..400).map(|i| format!("{tag}{i:04}")).collect();
        assert_eq!(lines, expected, "terminal {tag}");
    }
    rt.kill(ta.id).expect("kill");
    rt.kill(tb.id).expect("kill");
    drop(rt);
    fx.finish();
}

#[test]
fn a_dead_control_client_is_replaced_and_offsets_continue() {
    let Some(fx) = Fixture::new("reconnect") else {
        return;
    };
    let rt = fx.runtime();
    let t = rt
        .start(&fx.spec("steady", "sh", &["-c", "printf ready; cat"]))
        .expect("start");
    let (_, end) = wait_for(&rt, t.id, 0, b"ready");
    let first = rt.control_pid().expect("attached");
    signal(first, rustix::process::Signal::KILL);
    // The runtime notices, attaches again in the background, and input works again.
    let deadline = Instant::now() + WAIT;
    loop {
        if rt.control_pid().is_some_and(|pid| pid != first) && rt.write(t.id, b"after\r").is_ok() {
            break;
        }
        assert!(Instant::now() < deadline, "never reattached");
        std::thread::sleep(Duration::from_millis(20));
    }
    let (out, _) = wait_for(&rt, t.id, end, b"after\r\nafter\r\n");
    assert!(out.ends_with(b"after\r\nafter\r\n"));
    // A reader at the old end learns that output may have been lost in between.
    assert!(rt.read_output(t.id, end, 1).expect("read").truncated);
    let chunk = rt.read_output(t.id, 0, usize::MAX).expect("read");
    assert_eq!(chunk.offset, 0, "numbering did not restart");
    assert!(chunk.data.starts_with(b"ready"));
    assert!(rt.info(t.id).expect("info").alive);
    rt.kill(t.id).expect("kill");
    drop(rt);
    fx.finish();
}

#[test]
fn replies_to_hooks_are_not_taken_for_ours() {
    let Some(fx) = Fixture::new("hooks") else {
        return;
    };
    let rt = fx.runtime();
    let one = rt
        .start(&fx.spec("one", "sh", &["-c", "printf ready; exec cat"]))
        .expect("start");
    let (_, ready) = wait_for(&rt, one.id, 0, b"ready");
    // Anything on the server can add hooks; theirs reply to our client with flags 0.
    fx.tmux(&["set-hook", "-g", "after-set-option", "display-message -p x"]);
    fx.tmux(&["set-hook", "-g", "after-list-panes", "display-message -p y"]);
    let two = rt
        .start(&fx.spec("two", "sh", &["-c", "printf ready; exec cat"]))
        .expect("start with hooks set");
    wait_for(&rt, two.id, 0, b"ready");
    let listed = rt.list().expect("list");
    assert_eq!(listed.len(), 2);
    assert!(listed.iter().all(|t| t.alive), "{listed:?}");
    rt.write(one.id, b"still\r").expect("write");
    wait_for(&rt, one.id, ready, b"still\r\nstill\r\n");
    rt.kill(one.id).expect("kill");
    rt.kill(two.id).expect("kill");
    drop(rt);
    fx.finish();
}

#[test]
fn an_abandoned_start_leaves_no_window() {
    let Some(fx) = Fixture::new("abandon") else {
        return;
    };
    let mut options = fx.options.clone();
    options.start_timeout = Duration::from_secs(4);
    let rt = TmuxRuntime::new(options).expect("runtime");
    let first = rt
        .start(&fx.spec("first", "sh", &["-c", "printf ready; exec cat"]))
        .expect("start");
    wait_for(&rt, first.id, 0, b"ready");
    let server: u32 = fx
        .tmux(&["list-sessions", "-F", "#{pid}"])
        .parse()
        .expect("server pid");
    // The server stops answering while a start waits for it, which gives up.
    signal(server, rustix::process::Signal::STOP);
    let late = rt.start(&fx.spec("late", "sh", &["-c", "printf late; exec cat"]));
    signal(server, rustix::process::Signal::CONT);
    assert!(
        matches!(late, Err(RuntimeError::Unavailable(_))),
        "{late:?}"
    );
    // When the server answers, the window it made for nobody is removed.
    let first_id = first.id.to_string();
    eventually("only the first terminal's window is left", || {
        let tags = fx.tags();
        tags.len() == 1 && tags[0].1 == first_id
    });
    rt.kill(first.id).expect("kill");
    drop(rt);
    fx.finish();
}

#[test]
fn a_replaced_socket_directory_or_socket_is_refused() {
    let Some(fx) = Fixture::new("replaced") else {
        return;
    };
    let sub = fx.dir.join("sub");
    let mut options = fx.options.clone();
    options.socket = sub.join("s");
    let rt = TmuxRuntime::new(options.clone()).expect("runtime");
    // Behind the runtime's back, its directory is swapped for one open to others...
    std::fs::rename(&sub, fx.dir.join("sub.old")).expect("move aside");
    std::fs::create_dir(&sub).expect("open directory");
    std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    match rt.start(&fx.spec("x", "sh", &["-c", "exec cat"])) {
        Err(RuntimeError::Unavailable(why)) => assert!(why.contains("0700"), "{why}"),
        other => panic!("{other:?}"),
    }
    // ...or for a link to someone else's...
    std::fs::remove_dir(&sub).expect("remove");
    std::os::unix::fs::symlink(fx.dir.join("sub.old"), &sub).expect("symlink");
    match rt.start(&fx.spec("x", "sh", &["-c", "exec cat"])) {
        Err(RuntimeError::Unavailable(why)) => assert!(why.contains("not a directory"), "{why}"),
        other => panic!("{other:?}"),
    }
    // ...or the socket for something that is not one.
    std::fs::remove_file(&sub).expect("unlink");
    std::fs::rename(fx.dir.join("sub.old"), &sub).expect("move back");
    let _ = std::fs::remove_file(sub.join("s"));
    std::fs::write(sub.join("s"), b"not a socket").expect("file");
    match rt.start(&fx.spec("x", "sh", &["-c", "exec cat"])) {
        Err(RuntimeError::Unavailable(why)) => assert!(why.contains("not a socket"), "{why}"),
        other => panic!("{other:?}"),
    }
    match tmux::detect(&options) {
        Err(RuntimeError::Unavailable(why)) => assert!(why.contains("not a socket"), "{why}"),
        other => panic!("{other:?}"),
    }
    drop(rt);
    fx.finish();
}

#[test]
fn kill_stops_programs_that_ignore_hangup_and_their_jobs() {
    let Some(fx) = Fixture::new("kill") else {
        return;
    };
    let rt = fx.runtime();
    let stubborn = r"trap '' HUP TERM; (trap '' HUP; exec sleep 1001) & printf ready; while :; do sleep 1; done";
    let t = rt
        .start(&fx.spec("stubborn", "sh", &["-c", stubborn]))
        .expect("start");
    wait_for(&rt, t.id, 0, b"ready");
    let gone = || {
        !marked(&fx.mark)
            .iter()
            .any(|(_, cmd)| cmd.contains("sleep 1001") || cmd.contains("trap '' HUP TERM"))
    };
    assert!(!gone(), "the program is not running");
    rt.kill(t.id).expect("kill");
    assert!(!rt.info(t.id).expect("info").alive);
    // Before the server is killed: the program (which ignores SIGHUP and SIGTERM) and its
    // background job (which ignores SIGHUP) are gone.
    eventually("the stubborn program and its job end", gone);
    drop(rt);
    fx.finish();
}

#[test]
fn a_copied_tag_does_not_move_a_terminal() {
    let Some(fx) = Fixture::new("forgery") else {
        return;
    };
    let rt = fx.runtime();
    let t = rt
        .start(&fx.spec("original", "sh", &["-c", "printf ready; exec cat"]))
        .expect("start");
    let (_, ready) = wait_for(&rt, t.id, 0, b"ready");
    let original = fx.pane_of(t.id);
    // Another window gets a copy of the tag; the original's tag is overwritten with garbage.
    fx.tmux(&[
        "new-window",
        "-d",
        "-t",
        "pitcrew:",
        "-n",
        "copy",
        "/bin/sh -c 'exec cat'",
    ]);
    let copy = fx
        .tags()
        .into_iter()
        .map(|(pane, _)| pane)
        .find(|pane| *pane != original)
        .expect("the copy's pane");
    let id = t.id.to_string();
    fx.tmux(&["set-option", "-p", "-t", &copy, TERMINAL_OPTION, &id]);
    fx.tmux(&[
        "set-option",
        "-p",
        "-t",
        &original,
        TERMINAL_OPTION,
        "garbage",
    ]);
    let listed = rt.list().expect("list");
    assert_eq!(listed.len(), 1, "{listed:?}");
    assert!(listed[0].alive, "an unreadable tag is not a missing pane");
    assert_eq!(listed[0].native_target, t.native_target);
    // Output of the copy never lands in the terminal; its own input and output still work.
    fx.tmux(&["send-keys", "-t", &copy, "-l", "intruder\r"]);
    rt.write(t.id, b"mine\r").expect("write");
    let (out, _) = wait_for(&rt, t.id, ready, b"mine\r\nmine\r\n");
    std::thread::sleep(Duration::from_millis(300));
    let out = [
        out,
        rt.read_output(t.id, ready, usize::MAX).expect("read").data,
    ]
    .concat();
    assert!(!contains(&out, b"intruder"), "{}", crlf(&out));
    // After a restart, an id on two panes is adopted on neither.
    fx.tmux(&["set-option", "-p", "-t", &original, TERMINAL_OPTION, &id]);
    drop(rt);
    let rt = fx.runtime();
    assert!(rt.list().expect("list").is_empty());
    drop(rt);
    fx.finish();
}

#[test]
fn a_forged_list_row_neither_moves_nor_ends_a_terminal() {
    let Some(fx) = Fixture::new("forgedrow") else {
        return;
    };
    let rt = fx.runtime();
    let t = rt
        .start(&fx.spec("real", "sh", &["-c", "printf ready; exec cat"]))
        .expect("start");
    let (_, ready) = wait_for(&rt, t.id, 0, b"ready");
    let pane = fx.pane_of(t.id);
    // A raw newline in a user option starts a row of its own in `list-panes`: this one says
    // the pane is dead, in another window.
    let forged = format!("1\n@99 {pane} 1 1 80 24 ");
    fx.tmux(&["set-option", "-p", "-t", &pane, OFFSET_OPTION, &forged]);
    let listed = rt.list().expect("list");
    assert_eq!(listed.len(), 1, "{listed:?}");
    assert!(listed[0].alive, "a forged row ended the terminal");
    assert_eq!(listed[0].native_target, t.native_target);
    rt.write(t.id, b"still\r").expect("write");
    wait_for(&rt, t.id, ready, b"still\r\nstill\r\n");
    rt.kill(t.id).expect("kill");
    drop(rt);
    fx.finish();
}

#[test]
fn a_new_connection_removes_windows_of_unfinished_starts() {
    let Some(fx) = Fixture::new("orphans") else {
        return;
    };
    let rt = fx.runtime();
    let t = rt
        .start(&fx.spec("kept", "sh", &["-c", "printf ready; exec cat"]))
        .expect("start");
    wait_for(&rt, t.id, 0, b"ready");
    // What a start leaves when its connection dies before it can tag the window: an untagged
    // pane started by the wrapper. And a window of someone else's, which must stay.
    fx.tmux(&[
        "new-window",
        "-d",
        "-t",
        "pitcrew:",
        "--",
        "/bin/sh",
        "-c",
        ": pitcrew-wrapper; exec cat",
    ]);
    fx.tmux(&[
        "new-window",
        "-d",
        "-t",
        "pitcrew:",
        "--",
        "/bin/sh",
        "-c",
        "exec cat",
    ]);
    assert_eq!(fx.tags().len(), 3);
    let first = rt.control_pid().expect("attached");
    signal(first, rustix::process::Signal::KILL);
    eventually("a new control client", || {
        rt.control_pid().is_some_and(|pid| pid != first)
    });
    let id = t.id.to_string();
    eventually("the orphan is gone and the rest stays", || {
        let tags = fx.tags();
        tags.len() == 2 && tags.iter().filter(|(_, tag)| *tag == id).count() == 1
    });
    assert!(rt.info(t.id).expect("info").alive);
    rt.kill(t.id).expect("kill");
    drop(rt);
    fx.finish();
}

#[test]
fn copy_mode_is_left_before_input() {
    let Some(fx) = Fixture::new("copymode") else {
        return;
    };
    let rt = fx.runtime();
    let t = rt
        .start(&fx.spec("copy", "sh", &["-c", "printf ready; exec cat"]))
        .expect("start");
    let (_, ready) = wait_for(&rt, t.id, 0, b"ready");
    let target = t.native_target.clone().expect("target");
    fx.tmux(&["copy-mode", "-t", &target]);
    let in_mode = || fx.tmux(&["display-message", "-p", "-t", &target, "#{pane_in_mode}"]);
    assert_eq!(in_mode(), "1");
    // In copy mode, `q` would leave it and Enter would copy: the text must reach cat instead.
    rt.write(t.id, b"q\rCOPY_MODE_TEXT\r").expect("write");
    // The terminal echoes both lines and cat copies them, in either order.
    let mut lines = Vec::new();
    eventually("both lines echoed and copied", || {
        let out = rt.read_output(t.id, ready, usize::MAX).expect("read").data;
        lines = crlf(&out).lines().map(str::to_owned).collect();
        lines.len() >= 4
    });
    lines.sort();
    assert_eq!(lines, ["COPY_MODE_TEXT", "COPY_MODE_TEXT", "q", "q"]);
    assert_eq!(in_mode(), "0");
    rt.kill(t.id).expect("kill");
    drop(rt);
    fx.finish();
}

#[test]
fn an_old_running_server_is_refused() {
    let Some(fx) = Fixture::new("oldserver") else {
        return;
    };
    // A stand-in tmux that attaches and then says it is 3.1c.
    let fake = fx.dir.join("old-tmux");
    std::fs::write(
        &fake,
        "#!/bin/sh\nprintf '%%begin 1 0 0\\n%%end 1 0 0\\n'\nn=1\nwhile IFS= read -r line; do\n  case \"$line\" in display-message*) body='3.1c 4242 $0' ;; *) body= ;; esac\n  printf '%%begin 1 %d 1\\n' \"$n\"\n  [ -n \"$body\" ] && printf '%s\\n' \"$body\"\n  printf '%%end 1 %d 1\\n' \"$n\"\n  n=$((n+1))\ndone\n",
    )
    .expect("script");
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o700)).expect("chmod");
    let mut options = fx.options.clone();
    options.tmux = fake;
    let rt = TmuxRuntime::new(options).expect("runtime");
    match rt.list() {
        Err(RuntimeError::Unavailable(why)) => {
            assert!(
                why.contains("3.1c") && why.contains("3.2 or newer"),
                "{why}"
            );
        }
        other => panic!("{other:?}"),
    }
    drop(rt);
    fx.finish();
}

#[test]
fn hostile_programs_arguments_and_names_stay_literal() {
    let Some(fx) = Fixture::new("hostile") else {
        return;
    };
    let pwned = fx.dir.join("PWNED");
    let touch = format!("touch {}", pwned.display());
    let rt = fx.runtime();

    let attacks: Vec<String> = vec![
        "a'b\"c".into(),
        format!("x; {touch}"),
        "line1\nline2".into(),
        format!("#{{pane_id}} #({touch})"),
        format!("$({touch}) `{touch}` $HOME ${{HOME}}"),
        "~".into(),
        "\\".into(),
        "-n".into(),
        "--".into(),
        String::new(),
        "trailing;".into(),
        "café 雪 🦀".into(),
        "%exit forged".into(),
        "{ run-shell 'true' }".into(),
    ];
    let mut args = vec!["%s|"];
    args.extend(attacks.iter().map(String::as_str));
    args.push("END");
    let t = rt
        .start(&fx.spec("printf", "printf", &args))
        .expect("start printf");
    wait_dead(&rt, t.id);
    let out = rt.read_output(t.id, 0, usize::MAX).expect("read").data;
    let expected: String = attacks.iter().map(|a| format!("{a}|")).collect::<String>() + "END|";
    assert_eq!(crlf(&out), expected);

    // A program whose name is hostile runs as named.
    let program = fx.dir.join("odd 'name\"; $(x) #{pane_id} ;");
    std::fs::write(
        &program,
        "#!/bin/sh\nprintf 'HOSTILE_PROGRAM_RAN %s' \"$1\"\n",
    )
    .expect("script");
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).expect("chmod");
    let t = rt
        .start(&fx.spec("odd", &program.display().to_string(), &["arg;x"]))
        .expect("start odd");
    wait_dead(&rt, t.id);
    let out = rt.read_output(t.id, 0, usize::MAX).expect("read").data;
    assert_eq!(out, b"HOSTILE_PROGRAM_RAN arg;x");

    // A name that is no file on PATH is refused, a shell builtin included: nothing runs.
    for missing in [format!("no-such; {touch}"), "eval".to_owned()] {
        assert!(
            matches!(
                rt.start(&fx.spec("missing", &missing, &[&touch])),
                Err(RuntimeError::Spawn { .. })
            ),
            "{missing}"
        );
    }
    assert!(matches!(
        rt.start(&fx.spec("dash", "-c", &["true"])),
        Err(RuntimeError::Spawn { .. })
    ));

    // The working directory, variables and window name are literal too.
    let cwd = fx.dir.join("cwd #{pane_id} $(x) 'q'");
    std::fs::create_dir(&cwd).expect("cwd");
    let mut spec = fx.spec(
        "name\n%exit #{pane_id} #(x)",
        "sh",
        &[
            "-c",
            "pwd; printf '%s' \"$PC_VALUE\"; printf '%s' \"${TMUX:-no tmux}\"; printf END; exec cat",
        ],
    );
    spec.cwd = cwd.display().to_string();
    spec.env = vec![("PC_VALUE".into(), format!("#{{pane_id}} $({touch}) ;\n'x'"))];
    let t = rt.start(&spec).expect("start env");
    let (out, _) = wait_for(&rt, t.id, 0, b"END");
    assert_eq!(
        crlf(&out),
        format!(
            "{}\n#{{pane_id}} $({touch}) ;\n'x'no tmuxEND",
            cwd.display()
        )
    );
    assert_eq!(t.name, "name %exit #{pane_id} #(x)");
    let target = t.native_target.clone().expect("target");
    assert_eq!(
        fx.tmux(&["list-panes", "-t", &target, "-F", "#{window_name}"]),
        "name %exit #{pane_id} #(x)"
    );
    let mut bad = fx.spec("bad", "true", &[]);
    bad.env = vec![("A=B".into(), "x".into())];
    assert!(matches!(rt.start(&bad), Err(RuntimeError::Spawn { .. })));
    let mut bad = fx.spec("bad", "true", &[]);
    bad.cwd = fx.dir.join("nowhere").display().to_string();
    assert!(matches!(rt.start(&bad), Err(RuntimeError::Spawn { .. })));
    bad.cwd = "relative".into();
    assert!(matches!(rt.start(&bad), Err(RuntimeError::Spawn { .. })));
    let bad = fx.spec("bad", "nul\0byte", &[]);
    assert!(matches!(rt.start(&bad), Err(RuntimeError::Spawn { .. })));

    // Nothing ran a shell or a format job.
    std::thread::sleep(Duration::from_millis(200));
    assert!(!pwned.exists(), "a hostile value was executed");
    rt.kill(t.id).expect("kill");
    drop(rt);
    fx.finish();
}

#[test]
fn detection_reports_tmux_or_why_not() {
    let Some(fx) = Fixture::new("detect") else {
        return;
    };
    let support = tmux::detect(&fx.options).expect("tmux is usable");
    assert!(support.version.is_supported());
    assert!(support.tmux.is_absolute(), "{}", support.tmux.display());
    assert_eq!(support.socket, fx.options.socket);
    assert_eq!(support.capability(), Capability::Tmux);

    let asked = Instant::now();
    let pending = tmux::detect_async(fx.options.clone());
    assert!(
        asked.elapsed() < Duration::from_millis(500),
        "detect_async blocked"
    );
    let support = block_on(pending).expect("tmux is usable");
    assert!(support.version.is_supported());

    let mut missing = fx.options.clone();
    missing.tmux = fx.dir.join("no-tmux-here");
    match block_on(tmux::detect_async(missing.clone())) {
        Err(RuntimeError::Unavailable(why)) => assert!(why.contains("not installed"), "{why}"),
        other => panic!("{other:?}"),
    }
    let rt = TmuxRuntime::new(missing).expect("runtime");
    assert!(matches!(
        rt.start(&fx.spec("x", "true", &[])),
        Err(RuntimeError::Unavailable(_))
    ));
    drop(rt);

    let open = fx.dir.join("open");
    std::fs::create_dir(&open).expect("dir");
    std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let mut unsafe_dir = fx.options.clone();
    unsafe_dir.socket = open.join("s");
    match tmux::detect(&unsafe_dir) {
        Err(RuntimeError::Unavailable(why)) => assert!(why.contains("0700"), "{why}"),
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        TmuxRuntime::new(unsafe_dir),
        Err(RuntimeError::Unavailable(_))
    ));
    // Detection left no server running.
    assert!(!fx.server_running());
    fx.finish();
}

#[test]
#[ignore = "a measurement: run it with --ignored --nocapture"]
fn throughput_of_50_mb() {
    let Some(fx) = Fixture::new("throughput") else {
        return;
    };
    const TOTAL: u64 = 50 * 1024 * 1024;
    let rt = fx.runtime();
    let script = "printf ready; read go; yes 'PitCrew throughput 0123456789 abcdefghijklmnopqrstuvwxyz ABCDEFGHIJKLMNOPQRSTUVWXYZ' | head -c 52428800; printf DONE; exec cat";
    let t = rt
        .start(&fx.spec("throughput", "sh", &["-c", script]))
        .expect("start");
    let (_, ready) = wait_for(&rt, t.id, 0, b"ready");
    let server: u32 = fx
        .tmux(&["list-sessions", "-F", "#{pid}"])
        .parse()
        .expect("server pid");
    let (cpu_before, server_before, wall) = (
        cpu_seconds("self"),
        cpu_seconds(&server.to_string()),
        Instant::now(),
    );
    rt.write(t.id, b"\r").expect("go");
    let deadline = Instant::now() + Duration::from_secs(600);
    let mut end = ready;
    while end < ready + TOTAL {
        let left = deadline
            .checked_duration_since(Instant::now())
            .expect("50 MB within 10 minutes");
        end = rt
            .wait_for_output(t.id, ready + TOTAL - 1, left)
            .expect("wait");
    }
    let wall = wall.elapsed().as_secs_f64();
    let cpu = cpu_seconds("self") - cpu_before;
    let server_cpu = cpu_seconds(&server.to_string()) - server_before;
    let mb = (end - ready) as f64 / (1024.0 * 1024.0);
    println!(
        "throughput: {mb:.1} MiB in {wall:.2} s ({:.1} MiB/s); this process {cpu:.2} s CPU = {:.1}% of one core over the stream, {:.3} s per MiB; tmux server {server_cpu:.2} s CPU",
        mb / wall,
        100.0 * cpu / wall,
        cpu / mb,
    );
    for task in std::fs::read_dir("/proc/self/task")
        .expect("tasks")
        .flatten()
    {
        let tid = task.file_name().to_string_lossy().into_owned();
        let comm = std::fs::read_to_string(task.path().join("comm")).unwrap_or_default();
        println!(
            "thread {tid} {}: {:.2} s",
            comm.trim(),
            cpu_seconds(&format!("self/task/{tid}"))
        );
    }
    let screen = rt.screen(t.id).expect("screen");
    assert!(screen.rows.iter().any(|r| r.contains("PitCrew throughput")));
    rt.kill(t.id).expect("kill");
    drop(rt);
    fx.finish();
}

/// User plus system CPU time of a process (`self` or a pid), from `/proc/<pid>/stat`.
fn cpu_seconds(pid: &str) -> f64 {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).expect("stat");
    let fields: Vec<&str> = stat
        .rsplit_once(')')
        .expect("comm")
        .1
        .split_whitespace()
        .collect();
    // After the command name: state is field 3, utime 14 and stime 15.
    let ticks: f64 =
        fields[11].parse::<f64>().expect("utime") + fields[12].parse::<f64>().expect("stime");
    let hz: f64 = String::from_utf8_lossy(
        &Command::new("getconf")
            .arg("CLK_TCK")
            .output()
            .expect("getconf")
            .stdout,
    )
    .trim()
    .parse()
    .expect("CLK_TCK");
    ticks / hz
}

/// A minimal executor: polls `future` on this thread, parking until it is woken.
fn block_on<F: Future>(future: F) -> F::Output {
    struct Unpark(std::thread::Thread);
    impl Wake for Unpark {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }
    let waker = Arc::new(Unpark(std::thread::current())).into();
    let mut cx = Context::from_waker(&waker);
    let mut future = std::pin::pin!(future);
    let deadline = Instant::now() + WAIT;
    loop {
        if let Poll::Ready(output) = future.as_mut().poll(&mut cx) {
            return output;
        }
        let left = deadline
            .checked_duration_since(Instant::now())
            .expect("the future never finished");
        std::thread::park_timeout(left);
    }
}

#[test]
fn without_tmux_the_runtime_is_unavailable() {
    // Runs everywhere: needs no tmux.
    let _serial = serial();
    let random = RandomState::new().hash_one((std::process::id(), SystemTime::now()));
    let dir = PathBuf::from(format!("/tmp/pc-{:012x}", random & 0xffff_ffff_ffff));
    let mut options = TmuxOptions::new(dir.join("s"));
    options.tmux = PathBuf::from("/nonexistent/pitcrew/tmux");
    let rt = TmuxRuntime::new(options.clone()).expect("runtime");
    let spec = StartSpec {
        program: "true".into(),
        args: Vec::new(),
        cwd: "/".into(),
        env: Vec::new(),
        name: "x".into(),
        cols: 80,
        rows: 24,
    };
    assert!(matches!(rt.start(&spec), Err(RuntimeError::Unavailable(_))));
    assert!(matches!(rt.list(), Err(RuntimeError::Unavailable(_))));
    match tmux::detect(&options) {
        Err(RuntimeError::Unavailable(why)) => assert!(why.contains("not installed"), "{why}"),
        other => panic!("{other:?}"),
    }
    drop(rt);
    let _ = std::fs::remove_dir(&dir);
    assert!(!Path::new(&dir).exists());
}
