//! `PtyRuntime` and a real pitcrew-ptyd, on Unix (`sh -c`). The Windows versions, with
//! `cmd.exe /c`, are in `pty_runtime_windows.rs`. See `common` for how each test gets its own
//! ptyd and checks that nothing is left behind.
//!
//! The throughput measurement is ignored by default:
//! `cargo test -p pitcrew-ptyd --test pty_runtime -- --ignored --nocapture`.

#![cfg(unix)]

mod common;

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use common::{
    Fixture, IDLE, WAIT, crlf, eventually, marked, running, wait_dead, wait_for, wait_for_screen,
};
use pitcrew_interfaces::runtime::{Runtime, RuntimeError, RuntimeKind};
use pitcrew_protocol::ids::TerminalId;
use pitcrew_protocol::runner::{Capability, Key};
use pitcrew_runtime::pty::{self, PtyRuntime};
use pitcrew_runtime::tmux::TmuxOptions;

#[test]
fn write_read_by_offset_resize_ctrl_c_and_kill() {
    let fx = Fixture::new("basic");
    let rt = fx.runtime();
    assert_eq!(rt.kind(), RuntimeKind::Pty);
    // No ptyd yet: no terminals, and listing does not start one.
    assert!(rt.list().expect("list without ptyd").is_empty());
    assert_eq!(rt.ptyd_pid(), None);
    let t = rt
        .start(&fx.script("round trip", "printf ready; cat"))
        .expect("start");
    fx.saw_ptyd(&rt);
    fx.saw(t.pid);
    assert!(t.alive);
    assert_eq!(t.name, "round trip");
    assert!(t.pid.is_some());
    assert_eq!(t.native_target, None);
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
    assert!(matches!(
        rt.resize(t.id, 0, 30),
        Err(RuntimeError::Io(e)) if e.kind() == std::io::ErrorKind::InvalidInput
    ));

    // The program itself sees the new size.
    let sized = rt
        .start(&fx.script("size", "printf ready; read x; stty size; cat"))
        .expect("start");
    fx.saw(sized.pid);
    let (_, at) = wait_for(&rt, sized.id, 0, b"ready");
    rt.resize(sized.id, 90, 33).expect("resize");
    rt.write(sized.id, b"\r").expect("write");
    wait_for(&rt, sized.id, at, b"33 90");

    // Ctrl-C ends cat; the output stays readable.
    rt.send_keys(t.id, &[Key::CtrlC]).expect("ctrl-c");
    wait_dead(&rt, t.id);
    assert!(!rt.info(t.id).expect("info").alive);
    assert_eq!(rt.read_output(t.id, 0, 5).expect("read").data, b"ready");
    assert!(matches!(
        rt.write(t.id, b"x"),
        Err(RuntimeError::Io(e)) if e.kind() == std::io::ErrorKind::BrokenPipe
    ));
    assert!(
        rt.kill(t.id).is_ok(),
        "killing an ended terminal is a no-op"
    );

    rt.kill(sized.id).expect("kill");
    assert!(!rt.info(sized.id).expect("info").alive);
    let listed = rt.list().expect("list");
    assert_eq!(listed.len(), 2);
    assert!(listed.iter().all(|t| !t.alive));
    let unknown = TerminalId::new();
    assert!(matches!(rt.info(unknown), Err(RuntimeError::NotFound(id)) if id == unknown));
    assert!(matches!(
        rt.read_output(unknown, 0, 1),
        Err(RuntimeError::NotFound(_))
    ));
    assert!(matches!(rt.kill(unknown), Err(RuntimeError::NotFound(_))));
    drop(rt);
    fx.finish();
}

#[test]
fn a_new_runtime_finds_the_terminal_and_output_resumes_at_the_last_offset() {
    let fx = Fixture::new("restart");
    let rt = fx.runtime();
    let t = rt
        .start(&fx.script("survivor", "printf ready; cat"))
        .expect("start");
    fx.saw_ptyd(&rt);
    wait_for(&rt, t.id, 0, b"ready");
    rt.write(t.id, b"one\r").expect("write");
    let (_, end) = wait_for(&rt, t.id, 0, b"one\r\none\r\n");
    let ptyd = rt.ptyd_pid().expect("connected");
    drop(rt);

    let rt = fx.runtime();
    let listed = rt.list().expect("list");
    assert_eq!(listed.len(), 1, "{listed:?}");
    assert_eq!(listed[0].id, t.id);
    assert!(listed[0].alive);
    assert_eq!(listed[0].name, "survivor");
    assert_eq!(rt.ptyd_pid(), Some(ptyd), "the same ptyd");
    // Nothing was lost: the history is all there, and the stream goes on from `end`.
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
    assert_eq!((before.offset, before.truncated), (0, false));
    assert!(before.data.starts_with(b"ready"));
    rt.write(t.id, b"two\r").expect("write after restart");
    let (more, _) = wait_for(&rt, t.id, end, b"two\r\ntwo\r\n");
    assert_eq!(more, b"two\r\ntwo\r\n");
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
    let fx = Fixture::new("screen");
    let rt = fx.runtime();
    let draw = r"printf '\033[2J\033[5;10Hprompt> \033[1;1Htop\rT\033[2;1Hmenu: \033[7mone\033[0m two\033[K\033[5;18H'; exec cat";
    let t = rt.start(&fx.script("screen", draw)).expect("start");
    fx.saw_ptyd(&rt);
    wait_for(&rt, t.id, 0, b"\x1b[5;18H");
    let screen = rt.screen(t.id).expect("screen");
    assert_eq!(screen.cols, 80);
    assert_eq!(screen.rows.len(), 24);
    assert_eq!(screen.rows[0], "Top");
    assert_eq!(screen.rows[1], "menu: one two");
    assert_eq!(screen.rows[4], "         prompt>");
    assert!(screen.rows[5..].iter().all(String::is_empty));
    assert_eq!((screen.cursor_row, screen.cursor_col), (4, 17));
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
fn queries_are_answered_and_arrows_follow_application_cursor_mode() {
    let fx = Fixture::new("queries");
    let rt = fx.runtime();
    // The program moves the cursor, asks where it is, and prints the six bytes of the answer.
    let ask = r"stty -icanon -echo min 1; printf '\033[3;5H\033[6n'; dd bs=1 count=6 2>/dev/null | od -An -c; printf ' asked'; exec cat";
    let t = rt.start(&fx.script("ask", ask)).expect("start");
    fx.saw_ptyd(&rt);
    let (out, _) = wait_for(&rt, t.id, 0, b"asked");
    let shown: String = String::from_utf8_lossy(&out).split_whitespace().collect();
    assert!(shown.contains("033[3;5R"), "{shown:?}");
    // A program that asks for application cursor keys gets SS3 arrows; normal mode gets CSI.
    let keys = rt
        .start(&fx.script("keys", r"printf '\033[?1hready'; exec cat"))
        .expect("start");
    let (_, at) = wait_for(&rt, keys.id, 0, b"ready");
    rt.send_keys(keys.id, &[Key::Up, Key::Enter]).expect("keys");
    wait_for(&rt, keys.id, at, b"\x1bOA");
    let normal = rt
        .start(&fx.script("normal", "printf ready; exec cat"))
        .expect("start");
    let (_, at) = wait_for(&rt, normal.id, 0, b"ready");
    rt.send_keys(normal.id, &[Key::Left, Key::Enter])
        .expect("keys");
    wait_for(&rt, normal.id, at, b"\x1b[D");
    for id in [t.id, keys.id, normal.id] {
        rt.kill(id).expect("kill");
    }
    drop(rt);
    fx.finish();
}

#[test]
fn hostile_output_neither_slows_the_screen_nor_stops_others() {
    let fx = Fixture::new("flood");
    let rt = fx.runtime();
    // 8200 times `ESC[65535L` is 64 KiB; vt100 alone would insert 65535 lines each time. Then
    // an OSC string that never ends (4 MiB), which vte would keep in memory.
    let flood = rt
        .start(&fx.script(
            "flood",
            r"i=0; while [ $i -lt 8200 ]; do printf '\033[65535L'; i=$((i+1)); done; printf '\033[1;1HFLOODED'; exec cat",
        ))
        .expect("start flood");
    fx.saw_ptyd(&rt);
    let osc = rt
        .start(&fx.script(
            "osc",
            r"printf '\033]0;'; head -c 4194304 /dev/zero | tr '\0' x; printf '\033\\\033[1;1HAFTER'; exec cat",
        ))
        .expect("start osc");
    let steady = rt
        .start(&fx.script("steady", "printf ready; exec cat"))
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
    // The endless string is cut: the screen is still drawn after it.
    let screen = wait_for_screen(&rt, osc.id, "AFTER");
    assert!(screen.rows[0].starts_with("AFTER"), "{:?}", screen.rows[0]);
    for id in [flood.id, osc.id, steady.id] {
        rt.kill(id).expect("kill");
    }
    drop(rt);
    fx.finish();
}

#[test]
fn two_terminals_interleave_without_mixing() {
    let fx = Fixture::new("two");
    let rt = fx.runtime();
    let count = |tag: &str| {
        format!(
            "i=0; while [ $i -lt 400 ]; do printf '{tag}%04d\\n' $i; i=$((i+1)); done; printf DONE; exec cat"
        )
    };
    let (a, b) = (count("A"), count("B"));
    let ta = rt.start(&fx.script("a", &a)).expect("start a");
    fx.saw_ptyd(&rt);
    let tb = rt.start(&fx.script("b", &b)).expect("start b");
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
fn a_reconnect_after_the_connection_dies_loses_no_offset() {
    let fx = Fixture::new("reconnect");
    let rt = fx.runtime();
    let t = rt
        .start(&fx.script("steady", "printf ready; cat"))
        .expect("start");
    fx.saw_ptyd(&rt);
    let (_, end) = wait_for(&rt, t.id, 0, b"ready");
    let ptyd = rt.ptyd_pid().expect("connected");
    // The client's side of the connection goes away; ptyd keeps recording meanwhile.
    rt.disconnect();
    assert_eq!(rt.ptyd_pid(), None);
    rt.write(t.id, b"after\r")
        .expect("write on a new connection");
    let (out, _) = wait_for(&rt, t.id, end, b"after\r\nafter\r\n");
    assert_eq!(out, b"after\r\nafter\r\n");
    assert_eq!(rt.ptyd_pid(), Some(ptyd));
    let all = rt.read_output(t.id, 0, usize::MAX).expect("read");
    assert!(!all.truncated);
    assert_eq!(all.data, b"readyafter\r\nafter\r\n");
    rt.kill(t.id).expect("kill");
    drop(rt);
    fx.finish();
}

#[test]
fn ptyd_outlives_its_client_and_exits_once_idle() {
    let fx = Fixture::new("outlive");
    let rt = fx.runtime();
    let t = rt
        .start(&fx.script("long", "printf ready; exec cat"))
        .expect("start");
    fx.saw_ptyd(&rt);
    wait_for(&rt, t.id, 0, b"ready");
    let ptyd = rt.ptyd_pid().expect("connected");
    // Detached: not this process's child, and in a session of its own.
    let stat = std::fs::read_to_string(format!("/proc/{ptyd}/stat")).unwrap_or_default();
    if let Some((_, rest)) = stat.rsplit_once(')') {
        let fields: Vec<&str> = rest.split_whitespace().collect();
        assert_ne!(fields[1], std::process::id().to_string(), "ptyd's parent");
        assert_eq!(fields[3], ptyd.to_string(), "ptyd leads its session");
    }
    drop(rt);
    // Well past its idle time, with no client: its terminal keeps it running.
    std::thread::sleep(IDLE * 4);
    assert!(running(ptyd), "ptyd ended with its client");
    assert!(
        running(t.pid.expect("pid")),
        "the terminal ended with the client"
    );
    let rt = fx.runtime();
    let found = rt.info(t.id).expect("info");
    assert!(found.alive);
    rt.kill(t.id).expect("kill");
    drop(rt);
    // Now idle: it exits by itself (`finish` waits for it).
    eventually("ptyd exits once idle", || !running(ptyd));
    assert!(
        !fx.options.endpoint.exists(),
        "the socket is removed when ptyd exits"
    );
    fx.finish();
}

#[test]
fn a_second_ptyd_is_refused() {
    let fx = Fixture::new("second");
    let rt = fx.runtime();
    let t = rt
        .start(&fx.script("one", "printf ready; exec cat"))
        .expect("start");
    fx.saw_ptyd(&rt);
    wait_for(&rt, t.id, 0, b"ready");
    let second = Command::new(common::ptyd())
        .args(["serve", "--foreground", "--endpoint"])
        .arg(&fx.options.endpoint)
        .env(common::MARK, &fx.mark)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .expect("run a second ptyd");
    assert_eq!(second.status.code(), Some(3), "{second:?}");
    assert!(
        String::from_utf8_lossy(&second.stderr).contains("already"),
        "{second:?}"
    );
    // The first still serves.
    rt.write(t.id, b"still\r").expect("write");
    wait_for(&rt, t.id, 0, b"still");
    rt.kill(t.id).expect("kill");
    drop(rt);
    fx.finish();
}

#[test]
fn kill_ends_programs_that_ignore_signals_and_their_jobs() {
    let fx = Fixture::new("kill");
    let rt = fx.runtime();
    let stubborn = r"trap '' HUP TERM; (trap '' HUP; exec sleep 1001) & printf ready; while :; do sleep 1; done";
    let t = rt.start(&fx.script("stubborn", stubborn)).expect("start");
    fx.saw_ptyd(&rt);
    wait_for(&rt, t.id, 0, b"ready");
    let gone = || {
        !marked(&fx.mark)
            .iter()
            .any(|(_, cmd)| cmd.contains("sleep 1001") || cmd.contains("trap '' HUP TERM"))
    };
    assert!(!gone(), "the program is not running");
    let started = Instant::now();
    rt.kill(t.id).expect("kill");
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(!rt.info(t.id).expect("info").alive);
    // The program (which ignores SIGHUP and SIGTERM) and its background job are gone.
    eventually("the stubborn program and its job end", gone);
    drop(rt);
    fx.finish();
}

#[test]
fn hostile_arguments_stay_literal_and_bad_starts_are_refused() {
    let fx = Fixture::new("args");
    let rt = fx.runtime();
    let hostile = [
        "$(touch pwned)",
        "`touch pwned`",
        "; touch pwned",
        "a b\tc",
        "\"quoted\" 'single'",
        "*",
        "line\nbreak",
        "ünïcode ✓",
    ];
    let mut args = vec!["-c", r#"printf '[%s]' "$@"; printf done; exec cat"#, "sh"];
    args.extend(hostile);
    let t = rt.start(&fx.spec("args", "sh", &args)).expect("start");
    fx.saw_ptyd(&rt);
    let (out, _) = wait_for(&rt, t.id, 0, b"done");
    let shown = crlf(&out);
    for arg in hostile {
        let expected = format!("[{arg}]");
        assert!(shown.contains(&expected), "{expected:?} in {shown:?}");
    }
    assert!(!fx.dir.join("pwned").exists());
    // A builtin is not a program; nor is a relative or missing directory acceptable.
    for (spec, why) in [
        (fx.spec("x", "eval", &["true"]), "not a program"),
        (fx.spec("x", "-sh", &[]), "may not start"),
        (
            {
                let mut s = fx.spec("x", "sh", &[]);
                s.cwd = "relative".into();
                s
            },
            "not absolute",
        ),
        (
            {
                let mut s = fx.spec("x", "sh", &[]);
                s.env = vec![("A B".into(), "x".into())];
                s
            },
            "variable name",
        ),
        (
            {
                let mut s = fx.spec("x", "sh", &[]);
                s.cols = 0;
                s
            },
            "1 to 1000",
        ),
    ] {
        match rt.start(&spec) {
            Err(RuntimeError::Spawn { program, reason }) => {
                assert_eq!(program, spec.program);
                assert!(reason.contains(why), "{reason}");
            }
            other => panic!("{why}: {other:?}"),
        }
    }
    // Variables reach the program, and TERM is set.
    let mut env = fx.spec(
        "env",
        "sh",
        &["-c", r#"printf '%s|%s|' "$PITCREW_X" "$TERM"; exec cat"#],
    );
    env.env = vec![("PITCREW_X".into(), "a;b $c".into())];
    let e = rt.start(&env).expect("start");
    wait_for(&rt, e.id, 0, b"a;b $c|xterm-256color|");
    rt.kill(t.id).expect("kill");
    rt.kill(e.id).expect("kill");
    drop(rt);
    fx.finish();
}

#[test]
fn detection_reports_the_pty_runtime_or_why_not() {
    let fx = Fixture::new("detect");
    let support = pty::detect(&fx.options).expect("usable");
    assert_eq!(support.capability(), Capability::Pty);
    assert_eq!(support.ptyd, common::ptyd());
    let asked = Instant::now();
    let pending = pty::detect_async(fx.options.clone());
    assert!(
        asked.elapsed() < Duration::from_millis(500),
        "detect_async blocked"
    );
    assert!(block_on(pending).is_ok());

    let mut missing = fx.options.clone();
    missing.ptyd = fx.dir.join("no-ptyd-here");
    match pty::detect(&missing) {
        Err(RuntimeError::Unavailable(why)) => assert!(why.contains("not installed"), "{why}"),
        other => panic!("{other:?}"),
    }
    // Without tmux, the PTY runtime is chosen; with neither, both reasons are given.
    let mut no_tmux = TmuxOptions::new(fx.dir.join("tmux-socket"));
    no_tmux.tmux = fx.dir.join("no-tmux-here");
    let chosen = pitcrew_runtime::choose(&no_tmux, &fx.options).expect("chosen");
    assert_eq!(chosen.capability(), Capability::Pty);
    assert_eq!(chosen.kind(), RuntimeKind::Pty);
    let rt = chosen
        .into_runtime(no_tmux.clone(), fx.options.clone())
        .expect("runtime");
    assert_eq!(rt.kind(), RuntimeKind::Pty);
    drop(rt);
    match pitcrew_runtime::choose(&no_tmux, &missing) {
        Err(RuntimeError::Unavailable(why)) => {
            assert!(
                why.contains("tmux") && why.contains("pitcrew-ptyd"),
                "{why}"
            );
        }
        other => panic!("{other:?}"),
    }
    // An endpoint in a directory open to others is refused.
    let open = fx.dir.join("open");
    std::fs::create_dir(&open).expect("dir");
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    let mut unsafe_dir = fx.options.clone();
    unsafe_dir.endpoint = open.join("ptyd");
    assert!(matches!(
        pty::detect(&unsafe_dir),
        Err(RuntimeError::Unavailable(_))
    ));
    assert!(matches!(
        PtyRuntime::new(unsafe_dir),
        Err(RuntimeError::Unavailable(_))
    ));
    fx.finish();
}

#[test]
#[ignore = "a measurement: run it with --ignored --nocapture"]
fn throughput_of_50_mb() {
    const TOTAL: u64 = 50 * 1024 * 1024;
    let fx = Fixture::new("throughput");
    let rt = fx.runtime();
    let script = "printf ready; read go; yes 'PitCrew throughput 0123456789 abcdefghijklmnopqrstuvwxyz ABCDEFGHIJKLMNOPQRSTUVWXYZ' | head -c 52428800; printf DONE; exec cat";
    let t = rt.start(&fx.script("throughput", script)).expect("start");
    fx.saw_ptyd(&rt);
    let (_, ready) = wait_for(&rt, t.id, 0, b"ready");
    let ptyd = rt.ptyd_pid().expect("connected");
    let (cpu_before, ptyd_before, wall) = (
        cpu_seconds("self"),
        cpu_seconds(&ptyd.to_string()),
        Instant::now(),
    );
    rt.write(t.id, b"\r").expect("go");
    // Read every byte, as a daemon streaming it to a client would.
    let deadline = Instant::now() + Duration::from_secs(600);
    let (mut at, mut received, mut gaps) = (ready + 1, 0u64, 0u32);
    while at < ready + TOTAL {
        assert!(Instant::now() < deadline, "50 MB within 10 minutes");
        let chunk = rt.read_output(t.id, at, usize::MAX).expect("read");
        if chunk.truncated {
            gaps += 1;
        }
        received += chunk.data.len() as u64;
        at = chunk.offset + chunk.data.len() as u64;
        if chunk.data.is_empty() {
            rt.wait_for_output(t.id, at, Duration::from_secs(5))
                .expect("wait");
        }
    }
    let wall = wall.elapsed().as_secs_f64();
    let cpu = cpu_seconds("self") - cpu_before;
    let ptyd_cpu = cpu_seconds(&ptyd.to_string()) - ptyd_before;
    let mb = (at - ready) as f64 / (1024.0 * 1024.0);
    println!(
        "throughput (debug build): {mb:.1} MiB in {wall:.2} s ({:.1} MiB/s), {:.1} MiB read by this client ({gaps} gaps where it fell behind the 2 MiB history); \
         this process {cpu:.2} s CPU = {:.1}% of one core over the stream, {:.3} s per MiB; ptyd {ptyd_cpu:.2} s CPU = {:.1}% of one core",
        mb / wall,
        received as f64 / (1024.0 * 1024.0),
        100.0 * cpu / wall,
        cpu / mb,
        100.0 * ptyd_cpu / wall,
    );
    let screen = rt.screen(t.id).expect("screen");
    assert!(screen.rows.iter().any(|r| r.contains("PitCrew throughput")));
    rt.kill(t.id).expect("kill");
    drop(rt);
    fx.finish();
}

/// User plus system CPU time of a process (`self` or a pid), from `/proc/<pid>/stat` (Linux).
fn cpu_seconds(pid: &str) -> f64 {
    let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return f64::NAN;
    };
    let fields: Vec<&str> = stat
        .rsplit_once(')')
        .map(|(_, rest)| rest.split_whitespace().collect())
        .unwrap_or_default();
    let ticks: f64 = fields
        .get(11)
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(f64::NAN)
        + fields
            .get(12)
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(f64::NAN);
    let hz: f64 = String::from_utf8_lossy(
        &Command::new("getconf")
            .arg("CLK_TCK")
            .output()
            .map(|o| o.stdout)
            .unwrap_or_default(),
    )
    .trim()
    .parse()
    .unwrap_or(100.0);
    ticks / hz
}

/// A minimal executor: polls `future` on this thread, parking until it is woken.
fn block_on<F: std::future::Future>(future: F) -> F::Output {
    use std::sync::Arc;
    use std::task::{Context, Poll, Wake};
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
