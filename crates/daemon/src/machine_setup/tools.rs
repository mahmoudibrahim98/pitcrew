//! Finding a tool on this machine's `PATH`, and running it briefly: bounded in time and output,
//! with no input, and with everything it starts: a process group of its own on Unix, a Job Object
//! on Windows (`pitcrew_remote::job`), which a timeout ends whole, and so does dropping the run
//! (a request given up part-way, a `JoinSet` dropped) while it is not finished.
//!
//! Only absolute `PATH` entries are searched: a relative one would name a folder relative to the
//! daemon's working directory, which nobody chose. On Windows the names `PATHEXT` would add are
//! tried in a fixed order (`.exe`, `.cmd`, `.bat`, `.com`), so `claude.cmd` (npm's) is found as
//! well as `claude.exe`.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt as _};

/// The most of each of stdout and stderr kept from one run.
const OUTPUT_CAP: u64 = 64 * 1024;

/// Where tools are looked for, and what their runs get besides the daemon's own environment.
#[derive(Clone, Debug, Default)]
pub struct Tools {
    /// The `PATH` to search; the daemon's own when `None`.
    path: Option<OsString>,
}

/// What one run gave.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Ran {
    /// The exit code, when it exited with one (not when it was killed or did not start).
    pub code: Option<i32>,
    /// Standard output, at most [`OUTPUT_CAP`] bytes of it, lossily as UTF-8.
    pub stdout: String,
    /// Standard error, likewise.
    pub stderr: String,
    /// It did not end within its time, and was stopped.
    pub timed_out: bool,
    /// Why it could not be started, if it was not.
    pub failed: Option<String>,
}

impl Ran {
    /// Exited with 0.
    pub fn succeeded(&self) -> bool {
        self.code == Some(0)
    }

    /// Both streams, stdout first.
    pub fn output(&self) -> String {
        if self.stderr.is_empty() {
            return self.stdout.clone();
        }
        if self.stdout.is_empty() {
            return self.stderr.clone();
        }
        format!("{}\n{}", self.stdout, self.stderr)
    }

    /// Why it did not succeed, in a few words: for a row's detail.
    pub fn why(&self) -> String {
        if let Some(failed) = &self.failed {
            return format!("it could not be run ({failed})");
        }
        if self.timed_out {
            return "it did not answer in time".to_owned();
        }
        match self.code {
            Some(code) => format!("it exited with {code}"),
            None => "it was stopped by a signal".to_owned(),
        }
    }
}

impl Tools {
    /// The daemon's own `PATH`.
    pub fn from_env() -> Self {
        Self { path: None }
    }

    /// This `PATH` instead of the daemon's own (tests).
    #[cfg(test)]
    #[cfg(unix)]
    pub fn with_path(path: impl Into<OsString>) -> Self {
        Self {
            path: Some(path.into()),
        }
    }

    fn path(&self) -> Option<OsString> {
        self.path.clone().or_else(|| std::env::var_os("PATH"))
    }

    /// `name`, as the first absolute `PATH` entry that has it as an executable file.
    pub fn find(&self, name: &str) -> Option<PathBuf> {
        let path = self.path()?;
        std::env::split_paths(&path)
            .filter(|dir| dir.is_absolute())
            .flat_map(|dir| candidates(&dir, name))
            .find(|candidate| is_executable(candidate))
    }

    /// Runs `program` with `args`, for at most `limit`. Its standard input is empty; its output
    /// is kept up to a cap. `NO_COLOR` and `TERM=dumb` ask it for plain text.
    pub async fn run(&self, program: &Path, args: &[&str], limit: Duration) -> Ran {
        let mut command = tokio::process::Command::new(program);
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("NO_COLOR", "1")
            .env("TERM", "dumb")
            .kill_on_drop(true);
        if let Some(path) = &self.path {
            command.env("PATH", path);
        }
        let mut running = match Running::spawn(command) {
            Ok(running) => running,
            Err(e) => {
                return Ran {
                    failed: Some(e.kind().to_string()),
                    ..Ran::default()
                };
            }
        };
        let stdout = running.child.stdout.take();
        let stderr = running.child.stderr.take();
        let finished = tokio::time::timeout(limit, async {
            tokio::join!(capped(stdout), capped(stderr), running.child.wait())
        })
        .await;
        match finished {
            Ok((stdout, stderr, status)) => Ran {
                code: status.ok().and_then(|s| s.code()),
                stdout,
                stderr,
                timed_out: false,
                failed: None,
            },
            Err(_) => {
                running.kill().await;
                Ran {
                    timed_out: true,
                    ..Ran::default()
                }
            }
        }
    }
}

/// A started tool. Stopping it stops everything it started: its process group on Unix, its Job
/// Object on Windows (where `claude.cmd` runs `node.exe`, which killing `cmd.exe` alone leaves
/// running). Dropping it before it is reaped stops them too.
struct Running {
    child: tokio::process::Child,
    /// `None` only if the tool could not be put in it; then only the tool itself is stopped.
    #[cfg(windows)]
    job: Option<pitcrew_remote::job::Job>,
}

impl Running {
    #[cfg(unix)]
    fn spawn(mut command: tokio::process::Command) -> std::io::Result<Self> {
        // A group of its own, so what it starts can be stopped with it.
        command.process_group(0);
        Ok(Self {
            child: command.spawn()?,
        })
    }

    #[cfg(windows)]
    fn spawn(mut command: tokio::process::Command) -> std::io::Result<Self> {
        // CREATE_NO_WINDOW: a console tool run by the daemon opens no window of its own.
        command.creation_flags(0x0800_0000);
        let job = match pitcrew_remote::job::Job::new() {
            Ok(job) => Some(job),
            Err(e) => {
                tracing::debug!(error = %e, "no job object for a version command");
                None
            }
        };
        let child = command.spawn()?;
        let job = job.filter(|job| {
            let assigned = pitcrew_remote::job::handle_of(&child)
                .ok_or_else(|| std::io::Error::other("it has ended"))
                .and_then(|handle| job.assign(handle));
            if let Err(e) = &assigned {
                tracing::debug!(error = %e, "a version command is not in its job object");
            }
            assigned.is_ok()
        });
        Ok(Self { child, job })
    }

    #[cfg(not(any(unix, windows)))]
    fn spawn(mut command: tokio::process::Command) -> std::io::Result<Self> {
        Ok(Self {
            child: command.spawn()?,
        })
    }

    /// Stops everything the tool started.
    /// - Unix: its process group, but only while the tool is not yet reaped (tokio's `id()` is
    ///   `None` after that): until then the group's id cannot belong to anyone else.
    /// - Windows: its whole job.
    fn stop_all(&self) {
        #[cfg(unix)]
        if let Some(pid) = self
            .child
            .id()
            .and_then(|id| i32::try_from(id).ok())
            .and_then(rustix::process::Pid::from_raw)
        {
            let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
        }
        #[cfg(windows)]
        if let Some(job) = &self.job {
            job.terminate();
        }
    }

    /// Stops everything it started, and it, and reaps it.
    async fn kill(&mut self) {
        self.stop_all();
        let _ = self.child.kill().await;
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        // A run given up before its tool ended (tokio's `kill_on_drop` stops only the tool).
        self.stop_all();
    }
}

/// Reads `stream` to its end, keeping at most [`OUTPUT_CAP`] bytes.
async fn capped(stream: Option<impl AsyncRead + Unpin>) -> String {
    let Some(stream) = stream else {
        return String::new();
    };
    let mut kept = Vec::new();
    let mut limited = stream.take(OUTPUT_CAP);
    let _ = limited.read_to_end(&mut kept).await;
    // Whatever is beyond the cap is drained, so the program is not stuck writing it.
    let mut rest = limited.into_inner();
    let mut sink = [0_u8; 8192];
    while let Ok(n) = rest.read(&mut sink).await {
        if n == 0 {
            break;
        }
    }
    String::from_utf8_lossy(&kept).into_owned()
}

/// The names `name` may have in `dir`.
fn candidates(dir: &Path, name: &str) -> Vec<PathBuf> {
    if cfg!(windows) {
        [".exe", ".cmd", ".bat", ".com"]
            .iter()
            .map(|ext| {
                let mut file = OsString::from(name);
                file.push(OsStr::new(ext));
                dir.join(file)
            })
            .collect()
    } else {
        vec![dir.join(name)]
    }
}

fn is_executable(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// The first line of `text` that has something on it, cleaned for a row (no control or hidden
/// characters, at most 120 characters).
pub fn first_line(text: &str) -> Option<String> {
    text.lines().map(clean).find(|line| !line.is_empty())
}

/// `text` as one plain line: escape sequences, control and hidden characters dropped, runs of
/// whitespace made one space, at most 120 characters.
pub fn clean(text: &str) -> String {
    let mut out = String::new();
    let mut space = false;
    for c in strip_escapes(text) {
        if c.is_whitespace() {
            space = !out.is_empty();
            continue;
        }
        if c.is_control() || pitcrew_protocol::text::is_hidden(c) {
            continue;
        }
        if space {
            out.push(' ');
            space = false;
        }
        out.push(c);
        if out.chars().count() >= 120 {
            break;
        }
    }
    out
}

/// `text` without ANSI escape sequences (CSI `ESC [ … final`, OSC `ESC ] … BEL/ST`, and
/// two-character escapes).
fn strip_escapes(text: &str) -> Vec<char> {
    let mut out = Vec::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('[') => {
                // Parameters and intermediates, then one final byte in @..~.
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
            Some(']') => {
                // Up to BEL, or ESC \.
                while let Some(c) = chars.next() {
                    if c == '\u{7}' {
                        break;
                    }
                    if c == '\u{1b}' && chars.peek() == Some(&'\\') {
                        chars.next();
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// The first word of `line` that looks like a version (`2.1.3`, `3.4`, `3.3a`, `v2.45.0`),
/// without a leading `v`.
pub fn version_word(line: &str) -> Option<String> {
    line.split(|c: char| c.is_whitespace() || c == ',' || c == '(' || c == ')')
        .map(|w| w.strip_prefix('v').unwrap_or(w))
        .map(|w| w.strip_prefix("next-").unwrap_or(w))
        .find(|w| {
            let mut parts = w.split('.');
            parts
                .next()
                .is_some_and(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
                && parts
                    .next()
                    .is_some_and(|p| p.bytes().next().is_some_and(|b| b.is_ascii_digit()))
        })
        .map(str::to_owned)
}

/// A version's major and minor numbers (`3.3a` → `(3, 3)`).
pub fn major_minor(version: &str) -> Option<(u32, u32)> {
    let mut parts = version.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor: String = parts
        .next()?
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    Some((major, minor.parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_are_read_from_what_tools_print() {
        let word = |line: &str| version_word(line);
        assert_eq!(word("2.1.3 (Claude Code)").as_deref(), Some("2.1.3"));
        assert_eq!(word("codex-cli 0.50.0").as_deref(), Some("0.50.0"));
        assert_eq!(word("tmux 3.3a").as_deref(), Some("3.3a"));
        assert_eq!(word("tmux next-3.5").as_deref(), Some("3.5"));
        assert_eq!(word("git version 2.43.0").as_deref(), Some("2.43.0"));
        assert_eq!(
            word("gh version 2.45.0 (2024-03-04)").as_deref(),
            Some("2.45.0")
        );
        assert_eq!(word("slurm 23.02.7").as_deref(), Some("23.02.7"));
        assert_eq!(word("v1.0.25").as_deref(), Some("1.0.25"));
        assert_eq!(word("no version here"), None);
        assert_eq!(word("1."), None);
        assert_eq!(major_minor("3.3a"), Some((3, 3)));
        assert_eq!(major_minor("3.10"), Some((3, 10)));
        assert_eq!(major_minor("3"), None);
    }

    #[test]
    fn lines_are_cleaned_of_escapes_controls_and_hidden_characters() {
        assert_eq!(
            clean("\u{1b}[1;32mLogged in\u{1b}[0m  as\tsam\u{202e}\r"),
            "Logged in as sam"
        );
        assert_eq!(clean("\u{1b}]0;title\u{7}after"), "after");
        assert_eq!(clean("\u{1b}]8;;https://x\u{1b}\\link"), "link");
        assert_eq!(
            first_line("\n  \n second \n third").as_deref(),
            Some("second")
        );
        assert_eq!(clean(&"x".repeat(500)).chars().count(), 120);
    }

    #[cfg(unix)]
    mod unix {
        use super::super::*;

        fn script(dir: &Path, name: &str, body: &str) {
            crate::test_scripts::write_script(
                &dir.join(name),
                &format!("#!/bin/sh\n{body}\n"),
                0o755,
            );
        }

        #[test]
        fn only_executable_files_on_absolute_entries_are_found() {
            let tmp = tempfile::tempdir().unwrap();
            let bin = tmp.path().join("bin");
            std::fs::create_dir(&bin).unwrap();
            script(&bin, "tool", "exit 0");
            std::fs::write(bin.join("plain"), "not a program").unwrap();
            std::fs::create_dir(bin.join("folder")).unwrap();
            let mut path = OsString::from("relative:");
            path.push(&bin);
            let tools = Tools::with_path(path);
            assert_eq!(tools.find("tool"), Some(bin.join("tool")));
            assert_eq!(tools.find("plain"), None);
            assert_eq!(tools.find("folder"), None);
            assert_eq!(tools.find("missing"), None);
        }

        /// Whether process `pid` has ended (or is a zombie), within five seconds. Linux only.
        fn gone(pid: i32) -> bool {
            (0..100).any(|_| {
                std::thread::sleep(Duration::from_millis(50));
                !Path::new(&format!("/proc/{pid}")).exists()
                    || std::fs::read_to_string(format!("/proc/{pid}/stat"))
                        .is_ok_and(|s| s.contains(") Z "))
            })
        }

        /// A run given up before its tool ends (its future dropped, as a `JoinSet` is when a
        /// request is cancelled) stops what the tool started too, not only the tool.
        #[tokio::test]
        async fn a_run_given_up_stops_what_it_started() {
            let tmp = tempfile::tempdir().unwrap();
            let bin = tmp.path().to_path_buf();
            let child_file = bin.join("child");
            script(
                &bin,
                "slow",
                &format!("sleep 30 & echo $! > '{}'; wait", child_file.display()),
            );
            let mut path = bin.as_os_str().to_owned();
            path.push(":/usr/bin:/bin");
            let tools = Tools::with_path(path);
            let program = bin.join("slow");
            let run =
                tokio::spawn(
                    async move { tools.run(&program, &[], Duration::from_secs(60)).await },
                );
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            let child = loop {
                let read = std::fs::read_to_string(&child_file).unwrap_or_default();
                if let Ok(pid) = read.trim().parse::<i32>() {
                    break pid;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "the tool never started its child"
                );
                tokio::time::sleep(Duration::from_millis(20)).await;
            };
            run.abort();
            assert!(run.await.unwrap_err().is_cancelled());
            assert!(
                gone(child) || !cfg!(target_os = "linux"),
                "the sleep it started still runs after the run was given up"
            );
        }

        #[tokio::test]
        async fn a_run_is_bounded_in_time_and_output() {
            let tmp = tempfile::tempdir().unwrap();
            let bin = tmp.path();
            script(bin, "talk", "echo out; echo err >&2; exit 3");
            script(bin, "loud", "yes 0123456789 | head -c 1000000; exit 0");
            // The child it starts must end with it: its process group is stopped.
            script(
                bin,
                "slow",
                &format!("sleep 30 & echo $! > '{}/child'; wait", bin.display()),
            );
            // The scripts' own tools (`yes`, `head`, `sleep`) come from the system's folders.
            let mut path = bin.as_os_str().to_owned();
            path.push(":/usr/bin:/bin");
            let tools = Tools::with_path(path);

            let ran = tools
                .run(&bin.join("talk"), &[], Duration::from_secs(10))
                .await;
            assert_eq!(ran.code, Some(3));
            assert_eq!(ran.stdout.trim(), "out");
            assert_eq!(ran.stderr.trim(), "err");
            assert_eq!(ran.why(), "it exited with 3");

            let ran = tools
                .run(&bin.join("loud"), &[], Duration::from_secs(10))
                .await;
            assert!(ran.succeeded(), "{ran:?}");
            assert_eq!(ran.stdout.len(), usize::try_from(OUTPUT_CAP).unwrap());

            let started = std::time::Instant::now();
            let ran = tools
                .run(&bin.join("slow"), &[], Duration::from_millis(500))
                .await;
            assert!(ran.timed_out, "{ran:?}");
            assert!(started.elapsed() < Duration::from_secs(10));
            assert_eq!(ran.why(), "it did not answer in time");
            let child: i32 = std::fs::read_to_string(bin.join("child"))
                .unwrap()
                .trim()
                .parse()
                .unwrap();
            assert!(
                gone(child) || !cfg!(target_os = "linux"),
                "the sleep it started still runs"
            );

            let ran = tools
                .run(&bin.join("missing"), &[], Duration::from_secs(1))
                .await;
            assert!(ran.failed.is_some(), "{ran:?}");
        }
    }
}
