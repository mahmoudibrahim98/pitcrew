//! What the PTY runtime's integration tests share: a private ptyd per test, and checks that
//! nothing is left behind.
//!
//! Every test has its own endpoint (on Unix a socket in a new 0700 directory under `/tmp`, on
//! Windows a pipe with a random name), never the user's default one. Every process a test
//! starts carries `PITCREW_TEST_RUN=<its mark>`: the ptyd the runtime starts gets it, and every
//! terminal inherits it from ptyd. A test ends by checking, with its runtimes dropped, that no
//! runtime thread is left in this process, that its ptyd exits by itself (with no terminals and
//! no client, after a short idle time) and that nothing carrying its mark is left. On Windows,
//! where another process's environment cannot be read, it checks the processes it saw instead
//! (ptyd and the terminals' programs). The tests run one at a time.

#![allow(dead_code)]

use std::collections::hash_map::RandomState;
use std::hash::BuildHasher;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use pitcrew_interfaces::runtime::{Runtime, Screen, StartSpec};
use pitcrew_protocol::ids::TerminalId;
use pitcrew_runtime::pty::{PtyOptions, PtyRuntime};

/// Generous: other agents build on this machine at the same time, and CI machines are slow.
pub const WAIT: Duration = Duration::from_secs(30);
pub const MARK: &str = "PITCREW_TEST_RUN";
/// How long the tests' ptyd waits, idle, before it exits.
pub const IDLE: Duration = Duration::from_millis(500);

static SERIAL: Mutex<()> = Mutex::new(());

pub struct Fixture {
    pub dir: PathBuf,
    pub options: PtyOptions,
    pub mark: String,
    seen: Mutex<Vec<u32>>,
    finished: bool,
    _serial: MutexGuard<'static, ()>,
}

pub fn ptyd() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_pitcrew-ptyd"))
}

impl Fixture {
    pub fn new(test: &str) -> Self {
        let serial = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
        let random = RandomState::new().hash_one((std::process::id(), SystemTime::now(), test));
        let hex = format!("{:012x}", random & 0xffff_ffff_ffff);
        #[cfg(unix)]
        let (dir, endpoint) = {
            use std::os::unix::fs::DirBuilderExt;
            let dir = PathBuf::from(format!("/tmp/pc-{hex}"));
            // Not create_all: the directory must be new, and so ours.
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&dir)
                .expect("private directory");
            let endpoint = dir.join("ptyd");
            (dir, endpoint)
        };
        #[cfg(windows)]
        let (dir, endpoint) = {
            let dir = std::env::temp_dir().join(format!("pc-{hex}"));
            std::fs::create_dir(&dir).expect("directory");
            (
                dir,
                PathBuf::from(format!(r"\\.\pipe\pitcrew-ptyd-test-{hex}")),
            )
        };
        let mark = format!("{test}-{}-{hex}", std::process::id());
        let mut options = PtyOptions::new(endpoint);
        options.ptyd = ptyd();
        options.env.push((MARK.into(), mark.clone()));
        options.idle_exit = Some(IDLE);
        options.call_timeout = Duration::from_secs(20);
        options.start_timeout = Duration::from_secs(30);
        Self {
            dir,
            options,
            mark,
            seen: Mutex::new(Vec::new()),
            finished: false,
            _serial: serial,
        }
    }

    pub fn runtime(&self) -> PtyRuntime {
        PtyRuntime::new(self.options.clone()).expect("runtime")
    }

    /// A program and its arguments in a new terminal of 80 by 24.
    pub fn spec(&self, name: &str, program: &str, args: &[&str]) -> StartSpec {
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

    /// A shell script: `sh -c` on Unix, `cmd.exe /d /v:on /c` on Windows.
    pub fn script(&self, name: &str, script: &str) -> StartSpec {
        if cfg!(windows) {
            self.spec(name, "cmd.exe", &["/d", "/v:on", "/c", script])
        } else {
            self.spec(name, "sh", &["-c", script])
        }
    }

    /// Remembers a process to check at the end (on Windows, where marks cannot be read).
    pub fn saw(&self, pid: Option<u32>) {
        if let Some(pid) = pid {
            self.seen
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(pid);
        }
    }

    /// Remembers the runtime's ptyd.
    pub fn saw_ptyd(&self, rt: &PtyRuntime) {
        self.saw(rt.ptyd_pid());
    }

    /// With every runtime of the test dropped: no runtime thread is left, ptyd exits by itself,
    /// and nothing with this test's mark (or that it saw) is left.
    pub fn finish(mut self) {
        self.finished = true;
        let deadline = Instant::now() + WAIT;
        loop {
            let threads = runtime_threads();
            let left = self.left();
            if threads.is_empty() && left.is_empty() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "left behind: threads {threads:?}, processes {left:?}"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = std::fs::remove_dir_all(&self.dir);
        assert!(!self.dir.exists(), "the private directory is left");
    }

    /// Processes of this test still running.
    fn left(&self) -> Vec<(u32, String)> {
        let mut left = marked(&self.mark);
        let seen = self
            .seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        for pid in seen {
            if !left.iter().any(|(p, _)| *p == pid) && running(pid) {
                left.push((pid, "seen".into()));
            }
        }
        left
    }

    fn teardown(&self) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !self.left().is_empty() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        for (pid, _) in self.left() {
            kill(pid);
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if !self.finished {
            self.teardown();
        }
    }
}

/// Live processes carrying `mark` (Linux: `/proc`; macOS: `ps -E`; Windows: none can be read).
pub fn marked(mark: &str) -> Vec<(u32, String)> {
    let want = format!("{MARK}={mark}");
    #[cfg(target_os = "linux")]
    {
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
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        let out = std::process::Command::new("ps")
            .args(["-axE", "-o", "pid=,command="])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default();
        out.lines()
            .filter(|line| line.split_whitespace().any(|word| word == want))
            .filter_map(|line| {
                let line = line.trim();
                let (pid, rest) = line.split_once(' ')?;
                Some((pid.parse().ok()?, rest.chars().take(80).collect()))
            })
            .collect()
    }
    #[cfg(windows)]
    {
        let _ = want;
        Vec::new()
    }
}

/// Whether process `pid` runs (not a zombie).
pub fn running(pid: u32) -> bool {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
            stat.rsplit_once(')')
                .and_then(|(_, rest)| rest.split_whitespace().next())
                .is_some_and(|state| state != "Z")
        })
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        std::process::Command::new("ps")
            .args(["-o", "stat=", "-p", &pid.to_string()])
            .output()
            .is_ok_and(|o| {
                let state = String::from_utf8_lossy(&o.stdout);
                !state.trim().is_empty() && !state.trim().starts_with('Z')
            })
    }
    #[cfg(windows)]
    {
        std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
            .output()
            .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains(&format!("\"{pid}\"")))
    }
}

pub fn kill(pid: u32) {
    #[cfg(unix)]
    {
        let _ = std::process::Command::new("kill")
            .args(["-KILL", &pid.to_string()])
            .status();
    }
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("taskkill")
            .args(["/F", "/T", "/PID", &pid.to_string()])
            .status();
    }
}

/// This process's threads that belong to a PTY runtime (named `pitcrew-pty…`). Linux only;
/// elsewhere, none are seen.
pub fn runtime_threads() -> Vec<String> {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_dir("/proc/self/task")
            .map(|tasks| {
                tasks
                    .flatten()
                    .filter_map(|task| std::fs::read_to_string(task.path().join("comm")).ok())
                    .map(|comm| comm.trim().to_owned())
                    .filter(|comm| comm.starts_with("pitcrew-pty"))
                    .collect()
            })
            .unwrap_or_default()
    }
    #[cfg(not(target_os = "linux"))]
    {
        Vec::new()
    }
}

pub fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    needle.is_empty() || haystack.windows(needle.len()).any(|w| w == needle)
}

/// Waits until the output from `from` contains `needle`; returns that output and its end.
pub fn wait_for(rt: &PtyRuntime, id: TerminalId, from: u64, needle: &[u8]) -> (Vec<u8>, u64) {
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
        let end = rt
            .wait_for_output(id, chunk.end, left.min(Duration::from_secs(2)))
            .expect("wait");
        if end == chunk.end && !rt.info(id).expect("info").alive {
            panic!(
                "ended without {:?}: {:?}",
                String::from_utf8_lossy(needle),
                shown()
            );
        }
    }
}

/// Waits until the screen has a row containing `needle`.
pub fn wait_for_screen(rt: &PtyRuntime, id: TerminalId, needle: &str) -> Screen {
    let deadline = Instant::now() + WAIT;
    loop {
        let screen = rt.screen(id).expect("screen");
        if screen.rows.iter().any(|row| row.contains(needle)) {
            return screen;
        }
        assert!(
            Instant::now() < deadline,
            "no {needle:?} on the screen: {:#?}",
            screen.rows
        );
        let end = rt.read_output(id, u64::MAX, 0).expect("end").end;
        let _ = rt.wait_for_output(id, end, Duration::from_millis(200));
    }
}

/// Waits until the terminal's program has ended.
pub fn wait_dead(rt: &PtyRuntime, id: TerminalId) {
    let deadline = Instant::now() + WAIT;
    while rt.info(id).expect("info").alive {
        assert!(Instant::now() < deadline, "the program did not end");
        let end = rt.read_output(id, u64::MAX, 0).expect("end").end;
        let _ = rt.wait_for_output(id, end, Duration::from_millis(200));
    }
}

/// Polls `check` until it holds, for at most [`WAIT`].
pub fn eventually(what: &str, mut check: impl FnMut() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !check() {
        assert!(Instant::now() < deadline, "never: {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

pub fn crlf(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).replace("\r\n", "\n")
}
