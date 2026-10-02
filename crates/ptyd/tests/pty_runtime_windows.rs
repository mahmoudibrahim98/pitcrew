//! `PtyRuntime` and a real pitcrew-ptyd on Windows (ConPTY), with `cmd.exe /d /v:on /c` in place
//! of `sh -c`. See `common` for how each test gets its own ptyd (on a pipe of its own) and checks
//! that nothing is left behind.
//!
//! ConPTY does not pass a program's bytes through: the console host renders them and sends its
//! own redrawing of the screen. So these tests check the screen model and what the stream
//! contains, not exact bytes as the Unix tests do. The scripts use no `"`: the argument is
//! quoted once for `cmd.exe`, which strips the outer quotes and parses the rest itself.

#![cfg(windows)]

mod common;

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use common::{Fixture, IDLE, contains, eventually, running, wait_for, wait_for_screen};
use pitcrew_interfaces::runtime::{Runtime, RuntimeError, RuntimeKind};
use pitcrew_protocol::ids::TerminalId;
use pitcrew_protocol::runner::{Capability, Key};
use pitcrew_runtime::pty;
use pitcrew_runtime::tmux::TmuxOptions;

/// Prints `ready`, then each line typed as `[line]`.
const ECHO: &str = "echo ready& for /l %n in (1,1,100000) do @(set l=& set /p l=& echo [!l!])";

#[test]
fn write_read_by_offset_resize_ctrl_c_and_kill() {
    let fx = Fixture::new("basic");
    let rt = fx.runtime();
    assert_eq!(rt.kind(), RuntimeKind::Pty);
    assert!(rt.list().expect("list without ptyd").is_empty());
    let t = rt.start(&fx.script("round trip", ECHO)).expect("start");
    fx.saw_ptyd(&rt);
    fx.saw(t.pid);
    assert!(t.alive);
    assert_eq!(t.name, "round trip");
    assert_eq!(t.native_target, None);
    let (_, ready) = wait_for(&rt, t.id, 0, b"ready");
    rt.write(t.id, b"hello\r").expect("write");
    wait_for_screen(&rt, t.id, "[hello]");
    // Offsets address one stream: any slice of it is the same bytes.
    let all = rt.read_output(t.id, 0, usize::MAX).expect("read");
    assert!(!all.truncated);
    assert!(all.end > ready);
    let part = rt.read_output(t.id, 2, 3).expect("read");
    assert_eq!(part.offset, 2);
    assert_eq!(part.data, all.data[2..5]);
    let past = rt
        .read_output(t.id, all.end + 100, 10)
        .expect("read past the end");
    assert_eq!(past.offset, past.end);
    assert!(past.data.is_empty());
    rt.send_keys(t.id, &[Key::Left, Key::Enter]).expect("keys");
    rt.resize(t.id, 100, 30).expect("resize");
    let screen = rt.screen(t.id).expect("screen");
    assert_eq!((screen.cols, screen.rows.len()), (100, 30));
    assert!(matches!(
        rt.resize(t.id, 0, 30),
        Err(RuntimeError::Io(e)) if e.kind() == std::io::ErrorKind::InvalidInput
    ));
    // Ctrl-C reaches the console (it may or may not end cmd); kill always does.
    rt.send_keys(t.id, &[Key::CtrlC]).expect("ctrl-c");
    rt.kill(t.id).expect("kill");
    assert!(!rt.info(t.id).expect("info").alive);
    assert!(
        rt.kill(t.id).is_ok(),
        "killing an ended terminal is a no-op"
    );
    assert!(matches!(
        rt.write(t.id, b"x"),
        Err(RuntimeError::Io(e)) if e.kind() == std::io::ErrorKind::BrokenPipe
    ));
    let unknown = TerminalId::new();
    assert!(matches!(rt.info(unknown), Err(RuntimeError::NotFound(_))));
    drop(rt);
    fx.finish();
}

#[test]
fn a_new_runtime_finds_the_terminal_and_output_resumes_at_the_last_offset() {
    let fx = Fixture::new("restart");
    let rt = fx.runtime();
    let t = rt.start(&fx.script("survivor", ECHO)).expect("start");
    fx.saw_ptyd(&rt);
    fx.saw(t.pid);
    wait_for(&rt, t.id, 0, b"ready");
    rt.write(t.id, b"one\r").expect("write");
    wait_for_screen(&rt, t.id, "[one]");
    let end = rt.read_output(t.id, u64::MAX, 0).expect("end").end;
    drop(rt);
    let rt = fx.runtime();
    let listed = rt.list().expect("list");
    assert_eq!(listed.len(), 1, "{listed:?}");
    assert_eq!(listed[0].id, t.id);
    assert!(listed[0].alive);
    let before = rt.read_output(t.id, 0, 10).expect("read");
    assert_eq!((before.offset, before.truncated), (0, false));
    let resumed = rt.read_output(t.id, end, usize::MAX).expect("read");
    assert_eq!((resumed.offset, resumed.truncated), (end, false));
    rt.write(t.id, b"two\r").expect("write after restart");
    wait_for_screen(&rt, t.id, "[two]");
    rt.kill(t.id).expect("kill");
    drop(rt);
    fx.finish();
}

#[test]
fn the_screen_shows_a_prompt_drawn_with_cursor_movement() {
    let fx = Fixture::new("screen");
    let rt = fx.runtime();
    let draw = "echo \x1b[2J\x1b[5;10Hprompt^> \x1b[1;1Htop& set /p x=";
    let t = rt.start(&fx.script("screen", draw)).expect("start");
    fx.saw_ptyd(&rt);
    fx.saw(t.pid);
    let screen = wait_for_screen(&rt, t.id, "prompt>");
    assert_eq!((screen.cols, screen.rows.len()), (80, 24));
    assert!(screen.rows[0].starts_with("top"), "{:#?}", screen.rows);
    assert_eq!(
        screen.rows[4].trim_end(),
        "         prompt>",
        "{:#?}",
        screen.rows
    );
    rt.kill(t.id).expect("kill");
    drop(rt);
    fx.finish();
}

#[test]
fn hostile_output_neither_slows_the_screen_nor_stops_others() {
    let fx = Fixture::new("flood");
    let rt = fx.runtime();
    let flood = format!("echo {}FLOODED& set /p x=", "\x1b[65535L".repeat(600));
    let f = rt.start(&fx.script("flood", &flood)).expect("start flood");
    fx.saw_ptyd(&rt);
    fx.saw(f.pid);
    let steady = rt.start(&fx.script("steady", ECHO)).expect("start steady");
    fx.saw(steady.pid);
    wait_for(&rt, steady.id, 0, b"ready");
    let took = std::thread::scope(|scope| {
        let screen = scope.spawn(|| {
            let started = Instant::now();
            let screen = wait_for_screen(&rt, f.id, "FLOODED");
            (started.elapsed(), screen)
        });
        rt.write(steady.id, b"tick\r").expect("write");
        wait_for_screen(&rt, steady.id, "[tick]");
        screen.join().expect("screen thread")
    });
    assert!(took.0 < Duration::from_secs(20), "{:?}", took.0);
    let started = Instant::now();
    rt.screen(f.id).expect("screen");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
    rt.kill(f.id).expect("kill");
    rt.kill(steady.id).expect("kill");
    drop(rt);
    fx.finish();
}

#[test]
fn two_terminals_interleave_without_mixing() {
    let fx = Fixture::new("two");
    let rt = fx.runtime();
    let count =
        |tag: &str| format!("for /l %i in (1000,1,1199) do @(echo {tag}%i)& echo DONE& set /p x=");
    let ta = rt.start(&fx.script("a", &count("A"))).expect("start a");
    fx.saw_ptyd(&rt);
    fx.saw(ta.pid);
    let tb = rt.start(&fx.script("b", &count("B"))).expect("start b");
    fx.saw(tb.pid);
    for (id, mine, theirs) in [(ta.id, "A", "B"), (tb.id, "B", "A")] {
        let (out, _) = wait_for(&rt, id, 0, b"DONE");
        assert!(contains(&out, format!("{mine}1100").as_bytes()));
        assert!(!contains(&out, format!("{theirs}1").as_bytes()));
        let screen = rt.screen(id).expect("screen");
        assert!(
            screen
                .rows
                .iter()
                .any(|r| r.starts_with(&format!("{mine}1199")))
        );
        assert!(!screen.rows.iter().any(|r| r.starts_with(theirs)));
    }
    rt.kill(ta.id).expect("kill");
    rt.kill(tb.id).expect("kill");
    drop(rt);
    fx.finish();
}

#[test]
fn a_reconnect_after_the_connection_dies_loses_no_offset() {
    let fx = Fixture::new("reconnect");
    let rt = fx.runtime();
    let t = rt.start(&fx.script("steady", ECHO)).expect("start");
    fx.saw_ptyd(&rt);
    fx.saw(t.pid);
    wait_for(&rt, t.id, 0, b"ready");
    let ptyd = rt.ptyd_pid().expect("connected");
    rt.disconnect();
    rt.write(t.id, b"after\r")
        .expect("write on a new connection");
    wait_for_screen(&rt, t.id, "[after]");
    assert_eq!(rt.ptyd_pid(), Some(ptyd));
    assert!(!rt.read_output(t.id, 0, usize::MAX).expect("read").truncated);
    rt.kill(t.id).expect("kill");
    drop(rt);
    fx.finish();
}

#[test]
fn ptyd_outlives_its_client_and_exits_once_idle() {
    let fx = Fixture::new("outlive");
    let rt = fx.runtime();
    let t = rt.start(&fx.script("long", ECHO)).expect("start");
    fx.saw_ptyd(&rt);
    fx.saw(t.pid);
    wait_for(&rt, t.id, 0, b"ready");
    let ptyd = rt.ptyd_pid().expect("connected");
    drop(rt);
    std::thread::sleep(IDLE * 4);
    assert!(running(ptyd), "ptyd ended with its client");
    assert!(
        running(t.pid.expect("pid")),
        "the terminal ended with the client"
    );
    let rt = fx.runtime();
    assert!(rt.info(t.id).expect("info").alive);
    rt.kill(t.id).expect("kill");
    drop(rt);
    eventually("ptyd exits once idle", || !running(ptyd));
    fx.finish();
}

#[test]
fn a_second_ptyd_is_refused() {
    let fx = Fixture::new("second");
    let rt = fx.runtime();
    let t = rt.start(&fx.script("one", ECHO)).expect("start");
    fx.saw_ptyd(&rt);
    fx.saw(t.pid);
    wait_for(&rt, t.id, 0, b"ready");
    let second = Command::new(common::ptyd())
        .args(["serve", "--foreground", "--endpoint"])
        .arg(&fx.options.endpoint)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .expect("run a second ptyd");
    assert_eq!(second.status.code(), Some(3), "{second:?}");
    rt.write(t.id, b"still\r").expect("write");
    wait_for_screen(&rt, t.id, "[still]");
    rt.kill(t.id).expect("kill");
    drop(rt);
    fx.finish();
}

#[test]
fn kill_ends_the_program_and_what_it_started() {
    let fx = Fixture::new("kill");
    let rt = fx.runtime();
    let t = rt
        .start(&fx.script("tree", "echo ready& ping -n 1000 127.0.0.1"))
        .expect("start");
    fx.saw_ptyd(&rt);
    fx.saw(t.pid);
    wait_for(&rt, t.id, 0, b"ready");
    let pid = t.pid.expect("pid");
    let mut children = Vec::new();
    eventually("cmd starts ping", || {
        children = children_of(pid);
        !children.is_empty()
    });
    for child in &children {
        fx.saw(Some(*child));
    }
    rt.kill(t.id).expect("kill");
    assert!(!rt.info(t.id).expect("info").alive);
    eventually("the program and its child end", || {
        !running(pid) && children.iter().all(|c| !running(*c))
    });
    drop(rt);
    fx.finish();
}

#[test]
fn detection_reports_the_pty_runtime_or_why_not() {
    let fx = Fixture::new("detect");
    let support = pty::detect(&fx.options).expect("usable");
    assert_eq!(support.capability(), Capability::Pty);
    let mut missing = fx.options.clone();
    missing.ptyd = fx.dir.join("no-ptyd-here.exe");
    assert!(matches!(
        pty::detect(&missing),
        Err(RuntimeError::Unavailable(_))
    ));
    // tmux never runs on Windows: the PTY runtime is chosen.
    let tmux = TmuxOptions::new(fx.dir.join("tmux"));
    let chosen = pitcrew_runtime::choose(&tmux, &fx.options).expect("chosen");
    assert_eq!(chosen.capability(), Capability::Pty);
    let mut bad = fx.options.clone();
    bad.endpoint = fx.dir.join("not-a-pipe");
    assert!(matches!(
        pty::detect(&bad),
        Err(RuntimeError::Unavailable(_))
    ));
    fx.finish();
}

/// Process ids whose parent is `pid`.
fn children_of(pid: u32) -> Vec<u32> {
    let query = format!(
        "Get-CimInstance Win32_Process -Filter 'ParentProcessId={pid}' | ForEach-Object {{ $_.ProcessId }}"
    );
    let out = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", &query])
        .stdin(Stdio::null())
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    out.lines().filter_map(|l| l.trim().parse().ok()).collect()
}
