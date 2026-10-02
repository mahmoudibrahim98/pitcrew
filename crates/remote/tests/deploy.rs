//! Deploying and launching the helper against a fake `ssh` whose "remote" side is this
//! machine's own `/bin/sh` in a temporary `HOME`.
//!
//! The fake is this test binary: each case links it into a temporary directory as `ssh`, next
//! to a `remote.json`. Like sshd, it runs the command it gets as `/bin/sh -c <command>` in the
//! remote `HOME`, with the remote `PATH` (a sandbox holding only the tools the script needs),
//! and relays stdin, stdout and stderr. It can cut the connection partway through stdin
//! (leaving the remote script to see end of file, or killing it outright), pause, flip a byte,
//! or never read at all. It logs each call.
//!
//! The binary also plays `pitcrewd` (binding the socket it is told to and waiting to be killed,
//! or failing in chosen ways), and the hash tools, printing a sha256 the way `sha256sum`,
//! `shasum`, OpenSSL 1.1 or OpenSSL 3 do, or garbage. Hence `harness = false`.
//!
//! `PITCREW_TEST_SHELLS` (`:`-separated paths, as for `login_shells.rs`) adds a run of the whole
//! flow with each POSIX shell among them as the machine's `sh`.
//!
//! The SLURM launcher's cases are in `deploy/slurm.rs`, where the binary also plays SLURM; the
//! tunnel's and the stdio bridge's are in `deploy/tunnel.rs`, where the fake `ssh` also plays
//! links (ControlMasters), forwards and channels.
//!
//! Nothing a case starts outlives it: everything started on a fake machine (the fake `ssh`
//! itself included) carries this run's mark in its environment, and after each case (passed or
//! panicked) the runner waits a little, kills what still carries it, and fails the case if there
//! was any. At the end it checks that nothing is left. (Linux only: it reads `/proc`.)

// Test code; clippy's allow-unwrap-in-tests only sees `#[test]` functions.
#![allow(clippy::unwrap_used)]

use std::process::ExitCode;

#[cfg(not(unix))]
fn main() -> ExitCode {
    println!("skipped: the deploy tests run the remote side with a local Unix sh");
    ExitCode::SUCCESS
}

#[cfg(unix)]
fn main() -> ExitCode {
    unix::main()
}

#[cfg(unix)]
#[path = "deploy/slurm.rs"]
mod slurm;

#[cfg(unix)]
#[path = "deploy/tunnel.rs"]
mod tunnel;

#[cfg(unix)]
#[path = "deploy/tunnel_fake.rs"]
mod tunnel_fake;

#[cfg(unix)]
mod unix {
    use crate::slurm;
    use pitcrew_remote::helper::{HashTool, MIN_TMUX, Progress, parse_tmux_version, tmux_name};
    use pitcrew_remote::{
        DeployOptions, DirectLauncher, Endpoint, Helper, HelperError, HelperState, Input,
        LaunchOptions, Launcher, Layout, Limits, Platform, Ssh, SshError, Stopped, Target,
        TmuxLauncher, deploy,
    };
    use serde::{Deserialize, Serialize};
    use sha2::{Digest as _, Sha256};
    use std::io::{Read as _, Write as _};
    use std::os::unix::fs::{DirBuilderExt as _, FileTypeExt as _, PermissionsExt as _};
    use std::os::unix::process::CommandExt as _;
    use std::path::{Path, PathBuf};
    use std::process::{Command, ExitCode, Stdio};
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant, SystemTime};

    /// The script as the crate sends it: each call's stdin starts with exactly these bytes.
    const SCRIPT: &str = include_str!("../src/helper/helper.sh");
    const REMOTE: &str = "remote.json";
    const CALLS: &str = "calls.log";
    const PAUSED: &str = "paused";
    /// Makes the binary play `pitcrewd`: `serve`, `exit` or `nosocket`.
    const DAEMON_ENV: &str = "PITCREW_FAKE_DAEMON";
    /// Makes the binary play a hash tool (see [`act_as_tool`]).
    const TOOL_ENV: &str = "PITCREW_FAKE_TOOL";

    /// The tools the script and the probe may run, linked into each machine's `PATH`, which is
    /// also its tool path. A tool the script starts using without being listed here fails the
    /// tests.
    const TOOLS: &[&str] = &[
        "dd", "cat", "ls", "awk", "sed", "tr", "cut", "head", "tail", "wc", "mkdir", "rm", "mv",
        "ln", "chmod", "id", "uname", "date", "find", "readlink", "sleep", "setsid", "nohup",
        "tmux", "ps", "printf", "kill", "[", "test", "stat", "df", "mount", "cksum", "hostid",
        "env", "rmdir", "hostname", "timeout",
    ];

    pub fn main() -> ExitCode {
        if let Ok(tool) = std::env::var(TOOL_ENV) {
            return act_as_tool(&tool);
        }
        if let Ok(tool) = std::env::var(slurm::SLURM_ENV) {
            return slurm::act_as_slurm(&tool);
        }
        if let Ok(mode) = std::env::var(DAEMON_ENV) {
            return act_as_daemon(&mode);
        }
        if let Ok(spec) = std::env::var(crate::tunnel_fake::APP_ENV) {
            return crate::tunnel_fake::act_as_app(Path::new(&spec));
        }
        if let Some(code) = act_as_ssh() {
            return ExitCode::from(code);
        }
        run_tests()
    }

    // ─── The fakes ─────────────────────────────────────────────────────────────────────────

    /// How the fake `ssh` behaves, from `remote.json` beside it.
    #[derive(Clone, Default, Serialize, Deserialize)]
    pub(crate) struct Remote {
        #[serde(default)]
        pub(crate) home: PathBuf,
        #[serde(default)]
        pub(crate) path: String,
        #[serde(default)]
        pub(crate) env: Vec<(String, String)>,
        /// Drop the connection after this many bytes of stdin.
        #[serde(default)]
        pub(crate) cut_after: Option<u64>,
        /// ...and kill the remote command outright, rather than leave it to see end of file.
        #[serde(default)]
        pub(crate) kill: bool,
        /// Stop relaying stdin for `pause_ms` after this many bytes (once).
        #[serde(default)]
        pub(crate) pause_after: Option<u64>,
        #[serde(default)]
        pub(crate) pause_ms: u64,
        /// Flip the byte at this offset of stdin.
        #[serde(default)]
        pub(crate) corrupt_at: Option<u64>,
        /// For a call running this script command, read nothing and hang.
        #[serde(default)]
        pub(crate) hang_on: Option<String>,
        /// Swallow this many bytes of stdin before the command sees any, as a shell start-up
        /// file that reads stdin would...
        #[serde(default)]
        pub(crate) eat: Option<u64>,
        /// ...only in calls running this script command.
        #[serde(default)]
        pub(crate) eat_on: Option<String>,
        /// Run the command's interpreter (`/bin/sh` in the command line) as this shell instead,
        /// and the whole command line with it. Without the wrapper, which `login_shells.rs`
        /// covers.
        #[serde(default)]
        pub(crate) interpreter: Option<String>,
        /// The umask the remote command starts with.
        #[serde(default)]
        pub(crate) umask: Option<String>,
    }

    #[derive(Debug, Serialize, Deserialize)]
    pub(crate) struct CallLog {
        /// The command line, unwrapped.
        pub(crate) line: String,
        /// Bytes of stdin relayed.
        pub(crate) stdin: u64,
    }

    fn act_as_ssh() -> Option<u8> {
        let exe = std::env::current_exe().ok()?;
        let dir = exe.parent()?.to_path_buf();
        let remote: Remote =
            serde_json::from_str(&std::fs::read_to_string(dir.join(REMOTE)).ok()?).ok()?;
        // The fake itself carries the run's mark too, so that one left running (a tunnel's
        // link, say) is found: started without it (the tunnel gives ssh a minimal
        // environment), it starts again with it.
        if std::env::var_os(RUN_ENV).is_none()
            && let Some((_, mark)) = remote.env.iter().find(|(k, _)| k == RUN_ENV)
        {
            let err = Command::new(&exe)
                .args(std::env::args_os().skip(1))
                .env(RUN_ENV, mark)
                .exec();
            eprintln!("fake ssh: cannot mark itself: {err}");
            return Some(255);
        }
        let args: Vec<String> = std::env::args().skip(1).collect();
        if let Some(code) = crate::tunnel_fake::fake(&remote, &dir, &args) {
            return Some(code);
        }
        Some(fake_ssh(&remote, &dir))
    }

    // A dropped connection leaves the remote command running on its own: the fake exits
    // without waiting for it, as ssh would.
    #[allow(clippy::zombie_processes)]
    fn fake_ssh(remote: &Remote, dir: &Path) -> u8 {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let command = args
            .iter()
            .position(|a| a == "--")
            .and_then(|i| args.get(i + 2))
            .cloned()
            .unwrap_or_default();
        let line = decode(&command).unwrap_or_default();
        let ssh_log = args
            .iter()
            .position(|a| a == "-E")
            .and_then(|i| args.get(i + 1))
            .cloned();
        if remote
            .hang_on
            .as_deref()
            .is_some_and(|word| line.contains(&format!(" {word} ")))
        {
            log_call(dir, &line, 0);
            std::thread::sleep(Duration::from_secs(120));
            return 0;
        }
        let (shell, script) = match &remote.interpreter {
            Some(sh) => (
                sh.clone(),
                line.replacen("'/bin/sh' -c", &format!("'{sh}' -c"), 1),
            ),
            None => ("/bin/sh".to_owned(), command.clone()),
        };
        let mut child = Command::new(&shell);
        match &remote.umask {
            Some(mask) => child
                .arg("-c")
                .arg("umask \"$1\" && eval \"$2\"")
                .arg("sh")
                .arg(mask)
                .arg(&script),
            None => child.arg("-c").arg(&script),
        };
        let mut child = child
            .current_dir(&remote.home)
            .env("HOME", &remote.home)
            .env("PATH", &remote.path)
            .envs(remote.env.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()
            .unwrap();
        let out = relay(child.stdout.take().unwrap(), std::io::stdout());
        let err = relay(child.stderr.take().unwrap(), std::io::stderr());
        let mut to_remote = child.stdin.take().unwrap();
        let mut from_app = std::io::stdin().lock();
        let mut count: u64 = 0;
        let mut buf = vec![0u8; 8192];
        let mut paused = false;
        let mut cut = false;
        let eat = remote
            .eat
            .filter(|_| {
                remote
                    .eat_on
                    .as_deref()
                    .is_none_or(|word| line.contains(&format!(" {word} ")))
            })
            .unwrap_or(0);
        let mut eaten = std::io::Read::take(&mut from_app, eat);
        let swallowed = std::io::copy(&mut eaten, &mut std::io::sink()).unwrap_or(0);
        count += swallowed;
        loop {
            let mut want = buf.len() as u64;
            for mark in [remote.cut_after, remote.pause_after.filter(|_| !paused)]
                .into_iter()
                .flatten()
            {
                if mark > count {
                    want = want.min(mark - count);
                }
            }
            let n = match from_app.read(&mut buf[..usize::try_from(want).unwrap()]) {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            let chunk = &mut buf[..n];
            if let Some(at) = remote.corrupt_at
                && (count..count + n as u64).contains(&at)
            {
                chunk[usize::try_from(at - count).unwrap()] ^= 0xff;
            }
            if to_remote.write_all(chunk).is_err() {
                break;
            }
            count += n as u64;
            if !paused && remote.pause_after == Some(count) {
                paused = true;
                std::fs::write(dir.join(PAUSED), b"").unwrap();
                std::thread::sleep(Duration::from_millis(remote.pause_ms));
            }
            if remote.cut_after == Some(count) {
                cut = true;
                break;
            }
        }
        drop(to_remote);
        log_call(dir, &line, count);
        if cut {
            if remote.kill {
                let group = rustix::process::Pid::from_raw(i32::try_from(child.id()).unwrap());
                let _ = rustix::process::kill_process_group(
                    group.unwrap(),
                    rustix::process::Signal::KILL,
                );
            }
            if let Some(path) = ssh_log {
                let mut file = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)
                    .unwrap();
                writeln!(file, "Connection closed by 192.0.2.10 port 22").unwrap();
            }
            // The relays end with this process: the remote side finds its output closed.
            return 255;
        }
        let status = child.wait().unwrap();
        let _ = out.join();
        let _ = err.join();
        status
            .code()
            .and_then(|c| u8::try_from(c).ok())
            .unwrap_or(255)
    }

    fn relay(
        mut from: impl std::io::Read + Send + 'static,
        mut to: impl std::io::Write + Send + 'static,
    ) -> std::thread::JoinHandle<()> {
        std::thread::spawn(move || {
            let _ = std::io::copy(&mut from, &mut to);
            let _ = to.flush();
        })
    }

    fn log_call(dir: &Path, line: &str, stdin: u64) {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join(CALLS))
            .unwrap();
        let entry = CallLog {
            line: line.to_owned(),
            stdin,
        };
        writeln!(file, "{}", serde_json::to_string(&entry).unwrap()).unwrap();
    }

    /// Undoes the shell-neutral wrapper:
    /// `/bin/sh -c 'unset -f printf 2>/dev/null; eval "$(printf "\ooo…")"'`.
    pub(crate) fn decode(wrapped: &str) -> Option<String> {
        let escapes = wrapped
            .strip_prefix("/bin/sh -c 'unset -f printf 2>/dev/null; eval \"$(printf \"")?
            .strip_suffix("\")\"'")?;
        let bytes = escapes
            .as_bytes()
            .chunks(4)
            .map(|c| match c {
                [b'\\', rest @ ..] => u8::from_str_radix(std::str::from_utf8(rest).ok()?, 8).ok(),
                _ => None,
            })
            .collect::<Option<Vec<u8>>>()?;
        String::from_utf8(bytes).ok()
    }

    /// `pitcrewd serve --listen unix:<path>`: binds the socket, serves each connection (see
    /// [`crate::tunnel_fake::serve_connection`]) and waits to be killed. `exit` fails at once, saying
    /// why on stderr; `nosocket` never binds. `connect --socket <path>` is the real stdio bridge.
    fn act_as_daemon(mode: &str) -> ExitCode {
        if mode == "connect" {
            // `pitcrewd connect <args>`, as the daemon's CLI will run it.
            return pitcrew_remote::bridge::main(std::env::args_os().skip(2));
        }
        let socket = std::env::args()
            .skip(1)
            .find_map(|a| a.strip_prefix("unix:").map(PathBuf::from));
        match (mode, socket) {
            ("exit", _) => {
                eprintln!("fake failure: cannot serve");
                ExitCode::from(3)
            }
            ("nosocket", _) => {
                std::thread::sleep(Duration::from_secs(300));
                ExitCode::SUCCESS
            }
            (_, Some(socket)) => {
                // Its umask, for the test that it is the user's and not the script's.
                let umask = std::fs::read_to_string("/proc/self/status")
                    .unwrap_or_default()
                    .lines()
                    .find_map(|l| l.strip_prefix("Umask:").map(|v| v.trim().to_owned()))
                    .unwrap_or_default();
                println!("fake pitcrewd umask {umask}");
                let _ = std::fs::remove_file(&socket);
                let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
                println!("fake pitcrewd listening");
                for stream in listener.incoming().flatten() {
                    let socket = socket.clone();
                    std::thread::spawn(move || {
                        crate::tunnel_fake::serve_connection(stream, &socket);
                    });
                }
                ExitCode::SUCCESS
            }
            _ => ExitCode::from(2),
        }
    }

    /// A hash tool reading stdin: `sha256sum`, `shasum` (with `-a 256`), `openssl1` and
    /// `openssl3` (with `dgst -sha256`) print the sha256 their way; `garbage` prints none;
    /// anything else fails.
    fn act_as_tool(tool: &str) -> ExitCode {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let mut input = Vec::new();
        std::io::stdin().read_to_end(&mut input).unwrap();
        let hex = hex(&Sha256::digest(&input));
        match tool {
            "sha256sum" if args.is_empty() => println!("{hex}  -"),
            "shasum" if args == ["-a", "256"] => println!("{hex}  -"),
            "openssl3" if args == ["dgst", "-sha256"] => println!("SHA2-256(stdin)= {hex}"),
            "openssl1" if args == ["dgst", "-sha256"] => println!("(stdin)= {hex}"),
            "garbage" => println!("no hash here"),
            _ => return ExitCode::from(1),
        }
        ExitCode::SUCCESS
    }

    // ─── Fixtures ──────────────────────────────────────────────────────────────────────────

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    pub(crate) fn me() -> PathBuf {
        let me = std::env::current_exe().unwrap();
        assert!(!me.to_str().unwrap().contains('\''));
        me
    }

    /// The first executable file named `name` on the `PATH`, as a shell finds it. A file that is
    /// not executable is passed over: some installers leave one named `env` in `~/.local/bin`,
    /// to be sourced, and a link to it would leave the sandbox without a working `env`.
    pub(crate) fn which(name: &str) -> Option<PathBuf> {
        let path = std::env::var_os("PATH")?;
        std::env::split_paths(&path)
            .map(|dir| dir.join(name))
            .find(|p| {
                std::fs::metadata(p)
                    .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            })
    }

    /// A script named `name` in `bin`.
    pub(crate) fn shim(bin: &Path, name: &str, body: &str) {
        let path = bin.join(name);
        let _ = std::fs::remove_file(&path);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// A hash tool played by this binary (see [`act_as_tool`]).
    fn fake_tool(bin: &Path, name: &str, kind: &str) {
        shim(
            bin,
            name,
            &format!("{TOOL_ENV}={kind} exec '{}' \"$@\"", me().display()),
        );
    }

    fn link_tools(bin: &Path, sh: &Path) {
        std::os::unix::fs::symlink(sh, bin.join("sh")).unwrap();
        for tool in TOOLS {
            if let Some(real) = which(tool) {
                std::os::unix::fs::symlink(real, bin.join(tool)).unwrap();
            }
        }
        match which("sha256sum") {
            Some(real) => std::os::unix::fs::symlink(real, bin.join("sha256sum")).unwrap(),
            None => fake_tool(bin, "sha256sum", "sha256sum"),
        }
    }

    /// Deploys the stand-in helper (1.0.0) to `m` and starts it with the direct launcher.
    pub(crate) fn deploy_and_start(m: &Machine) {
        let target = m.plain();
        block_on(deploy(&target, &helper("1.0.0"), &quick())).unwrap();
        block_on(DirectLauncher::new(launch_options()).start(&target)).unwrap();
    }

    /// Stops the helper [`deploy_and_start`] started.
    pub(crate) fn stop_helper(m: &Machine) {
        block_on(DirectLauncher::new(launch_options()).stop(&m.plain())).unwrap();
    }

    /// `pitcrewd`, played by this binary: a hard link (or copy) named so that the process's
    /// name is `pitcrewd`, as the launchers check.
    pub(crate) fn daemon() -> PathBuf {
        static DIR: Mutex<Option<tempfile::TempDir>> = Mutex::new(None);
        let mut dir = DIR.lock().unwrap();
        let dir = dir.get_or_insert_with(|| {
            let dir = tempfile::Builder::new()
                .prefix("pitcrew-fake-daemon")
                .tempdir()
                .unwrap();
            let exe = dir.path().join("pitcrewd");
            if std::fs::hard_link(me(), &exe).is_err() {
                std::fs::copy(me(), &exe).unwrap();
            }
            dir
        });
        dir.path().join("pitcrewd")
    }

    fn drop_daemon() {
        // The temporary directory goes with the process otherwise left running nothing.
        let _ = std::fs::remove_dir_all(daemon().parent().unwrap());
    }

    /// A stand-in helper: a script that answers `--version` and runs the fake daemon for
    /// `serve`. `mode` is the daemon's (`serve`, `exit`, `nosocket`), or `stubborn` for one
    /// that ignores SIGTERM. `padding` bytes of comment make it bigger.
    pub(crate) fn helper_script(version: &str, mode: &str, padding: usize) -> Vec<u8> {
        let (trap, mode) = match mode {
            "stubborn" => ("trap '' TERM; ", "serve"),
            other => ("", other),
        };
        let mut script = format!(
            "#!/bin/sh\n\
             case \"$1\" in\n\
             --version) echo 'pitcrewd {version} (protocol 1)' ;;\n\
             serve) echo \"pitcrewd script: $0\" >&2; {trap}{DAEMON_ENV}={mode} exec '{daemon}' \"$@\" ;;\n\
             connect) {DAEMON_ENV}=connect exec '{daemon}' \"$@\" ;;\n\
             *) exit 2 ;;\n\
             esac\n",
            daemon = daemon().display()
        );
        if padding > 0 {
            script.push_str(&format!("# {}\n", "x".repeat(padding)));
        }
        script.into_bytes()
    }

    pub(crate) fn helper_from(version: &str, bytes: Vec<u8>) -> Helper {
        let hash = hex(&Sha256::digest(&bytes));
        Helper::new(Platform::LinuxX86_64, version, &hash, bytes).unwrap()
    }

    pub(crate) fn helper(version: &str) -> Helper {
        helper_from(version, helper_script(version, "serve", 0))
    }

    fn big_helper(version: &str, padding: usize) -> Helper {
        helper_from(version, helper_script(version, "serve", padding))
    }

    pub(crate) fn quick() -> DeployOptions {
        DeployOptions {
            timeout: Duration::from_secs(60),
            lock_wait: Duration::from_secs(5),
            stale_lock: Duration::from_secs(5 * 60),
            progress: None,
        }
    }

    pub(crate) fn launch_options() -> LaunchOptions {
        LaunchOptions {
            ready_timeout: Duration::from_secs(10),
            stop_timeout: Duration::from_secs(5),
            lock_wait: Duration::from_secs(5),
            stale_lock: Duration::from_secs(2 * 60),
            ..LaunchOptions::default()
        }
    }

    /// One "remote machine": a home, and a sandbox `PATH`.
    pub(crate) struct Machine {
        pub(crate) dir: tempfile::TempDir,
        pub(crate) home: PathBuf,
        pub(crate) bin: PathBuf,
        pub(crate) env: Vec<(String, String)>,
        /// The shell standing in for `/bin/sh` in the command line, if not `/bin/sh`.
        pub(crate) interpreter: Option<String>,
        pub(crate) fakes: AtomicU32,
    }

    /// A fake `ssh` for one machine.
    pub(crate) struct Fake {
        pub(crate) dir: PathBuf,
        pub(crate) ssh: Ssh,
    }

    impl Fake {
        pub(crate) fn calls(&self) -> Vec<CallLog> {
            std::fs::read_to_string(self.dir.join(CALLS))
                .unwrap_or_default()
                .lines()
                .map(|l| serde_json::from_str(l).unwrap())
                .collect()
        }

        pub(crate) fn paused(&self) -> bool {
            self.dir.join(PAUSED).exists()
        }
    }

    impl Machine {
        pub(crate) fn new() -> Self {
            Self::build(Path::new("/bin/sh"), |_| {})
        }

        pub(crate) fn with_tools(customize: impl FnOnce(&Path)) -> Self {
            Self::build(Path::new("/bin/sh"), customize)
        }

        /// A machine whose `/bin/sh` is `sh`, run by a link named `sh` (as `/bin/sh` is), so
        /// shells that look at their name (bash, zsh) behave as they would there.
        pub(crate) fn with_shell(sh: &Path) -> Self {
            let mut m = Self::build(sh, |_| {});
            m.interpreter = Some(m.bin.join("sh").to_str().unwrap().to_owned());
            m
        }

        pub(crate) fn build(sh: &Path, customize: impl FnOnce(&Path)) -> Self {
            // Short: the machine's sockets (ssh's, the helper's, a node-local one) live under it.
            let dir = pitcrew_fixtures::temp::short_tempdir().unwrap();
            let home = dir.path().join("home");
            std::fs::create_dir(&home).unwrap();
            let bin = dir.path().join("bin");
            std::fs::create_dir(&bin).unwrap();
            link_tools(&bin, sh);
            customize(&bin);
            Self {
                dir,
                home,
                bin,
                env: Vec::new(),
                interpreter: None,
                fakes: AtomicU32::new(0),
            }
        }

        pub(crate) fn fake(&self, remote: Remote) -> Fake {
            let n = self.fakes.fetch_add(1, Ordering::SeqCst);
            let dir = self.dir.path().join(format!("f{n}"));
            std::fs::create_dir(&dir).unwrap();
            let ssh = dir.join("ssh");
            if std::fs::hard_link(me(), &ssh).is_err() {
                std::fs::copy(me(), &ssh).unwrap();
            }
            // Everything the remote command starts carries the run's mark (see `sweep`).
            let mut env = vec![(RUN_ENV.to_owned(), run_mark().to_owned())];
            env.extend(self.env.iter().cloned());
            env.extend(remote.env.iter().cloned());
            let path = if remote.path.is_empty() {
                self.bin.to_str().unwrap().to_owned()
            } else {
                remote.path.clone()
            };
            let interpreter = remote.interpreter.clone().or(self.interpreter.clone());
            let remote = Remote {
                home: self.home.clone(),
                path,
                env,
                interpreter,
                ..remote
            };
            std::fs::write(dir.join(REMOTE), serde_json::to_vec(&remote).unwrap()).unwrap();
            let ssh = Ssh::new(ssh)
                .with_runtime_dir(dir.join("rt"))
                .with_multiplex(false);
            Fake { dir, ssh }
        }

        pub(crate) fn layout(&self) -> Layout {
            Layout::in_home(self.home.to_str().unwrap()).unwrap()
        }

        pub(crate) fn target(&self, fake: &Fake) -> Target {
            self.target_at(fake, self.layout())
        }

        /// A target with another root; the tool path is the sandbox.
        pub(crate) fn target_at(&self, fake: &Fake, layout: Layout) -> Target {
            Target::with_layout(fake.ssh.clone(), "cluster", layout, Platform::LinuxX86_64)
                .unwrap()
                .with_tool_path(self.bin.to_str().unwrap())
                .unwrap()
        }

        /// A target through a fake that just relays.
        pub(crate) fn plain(&self) -> Target {
            self.target(&self.fake(Remote::default()))
        }

        pub(crate) fn root(&self) -> PathBuf {
            self.home.join(".pitcrew")
        }

        pub(crate) fn bin_dir(&self) -> PathBuf {
            self.root().join("bin")
        }

        pub(crate) fn run_dir(&self) -> PathBuf {
            self.root().join("run")
        }

        pub(crate) fn lock(&self) -> PathBuf {
            self.bin_dir().join(".lock")
        }

        pub(crate) fn link(&self, name: &str) -> Option<String> {
            std::fs::read_link(self.bin_dir().join(name))
                .ok()
                .map(|p| p.to_str().unwrap().to_owned())
        }

        /// The version directories, sorted.
        pub(crate) fn versions(&self) -> Vec<String> {
            let mut names: Vec<String> = std::fs::read_dir(self.bin_dir())
                .unwrap()
                .map(|e| e.unwrap())
                .filter(|e| e.file_type().unwrap().is_dir())
                .map(|e| e.file_name().into_string().unwrap())
                .filter(|n| !n.starts_with('.'))
                .collect();
            names.sort();
            names
        }

        /// Upload temporaries anywhere under `bin/`.
        pub(crate) fn temporaries(&self) -> Vec<PathBuf> {
            let Ok(dirs) = std::fs::read_dir(self.bin_dir()) else {
                return Vec::new();
            };
            let mut found = Vec::new();
            for dir in dirs.map(|e| e.unwrap().path()).filter(|p| p.is_dir()) {
                for entry in std::fs::read_dir(&dir).unwrap() {
                    let path = entry.unwrap().path();
                    let name = path.file_name().unwrap().to_str().unwrap().to_owned();
                    if name.starts_with("pitcrewd.tmp.") {
                        found.push(path);
                    }
                }
            }
            found
        }
    }

    pub(crate) fn mode(path: &Path) -> u32 {
        std::fs::symlink_metadata(path)
            .unwrap()
            .permissions()
            .mode()
            & 0o7777
    }

    /// Nothing under `root` is open to the group or others (links aside). The helper's socket
    /// is its own, made with the user's umask; the 0700 directory it is in keeps others out.
    pub(crate) fn assert_private(root: &Path) {
        let mut stack = vec![root.to_path_buf()];
        while let Some(path) = stack.pop() {
            let meta = std::fs::symlink_metadata(&path).unwrap();
            if meta.file_type().is_symlink() || meta.file_type().is_socket() {
                continue;
            }
            assert_eq!(meta.permissions().mode() & 0o077, 0, "{}", path.display());
            if meta.is_dir() {
                assert_eq!(
                    meta.permissions().mode() & 0o777,
                    0o700,
                    "{}",
                    path.display()
                );
                stack.extend(std::fs::read_dir(&path).unwrap().map(|e| e.unwrap().path()));
            }
        }
    }

    pub(crate) fn private_dir(path: &Path) {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)
            .unwrap();
    }

    pub(crate) fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap()
    }

    pub(crate) fn block_on<F: std::future::Future>(future: F) -> F::Output {
        runtime().block_on(future)
    }

    /// DIAG (temporary, macOS CI): what a machine holds and which processes run.
    pub(crate) fn diag(m: &Machine) -> String {
        let run = |args: &[&str]| {
            Command::new(args[0])
                .args(&args[1..])
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
                .unwrap_or_else(|e| e.to_string())
        };
        format!(
            "DIAG temporaries={:?} lock={} owner={:?}\nls:\n{}\nps:\n{}",
            m.temporaries(),
            m.lock().exists(),
            std::fs::read_to_string(m.lock().join("owner")).ok(),
            run(&["ls", "-laR", m.root().to_str().unwrap()]),
            run(&["ps", "-axo", "pid,ppid,pgid,stat,etime,command"])
                .lines()
                .filter(|l| l.contains(m.dir.path().to_str().unwrap())
                    || l.contains(" sh ")
                    || l.contains("cat"))
                .collect::<Vec<_>>()
                .join("\n"),
        )
    }

    pub(crate) fn eventually(what: &str, check: impl Fn() -> bool) {
        let start = Instant::now();
        while !check() {
            assert!(
                start.elapsed() < Duration::from_secs(15),
                "timed out: {what}"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    pub(crate) fn alive(pid: u32) -> bool {
        i32::try_from(pid)
            .ok()
            .and_then(rustix::process::Pid::from_raw)
            .is_some_and(|pid| rustix::process::test_kill_process(pid).is_ok())
            && !std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|s| {
                s.rsplit(')')
                    .next()
                    .is_some_and(|r| r.trim_start().starts_with('Z'))
            })
    }

    pub(crate) fn comm(pid: u32) -> String {
        if let Ok(name) = std::fs::read_to_string(format!("/proc/{pid}/comm")) {
            return name.trim().to_owned();
        }
        // No /proc (macOS): ps names the command, there by its path.
        Command::new("ps")
            .args(["-o", "comm=", "-p", &pid.to_string()])
            .output()
            .map(|out| {
                let name = String::from_utf8_lossy(&out.stdout);
                name.trim()
                    .rsplit('/')
                    .next()
                    .unwrap_or_default()
                    .to_owned()
            })
            .unwrap_or_default()
    }

    /// This process's umask, as /proc prints it: `0022`.
    pub(crate) fn own_umask() -> String {
        use rustix::fs::Mode;
        // Read by setting it, then put back at once (the cases run one at a time).
        let mask = rustix::process::umask(Mode::empty());
        rustix::process::umask(mask);
        format!("{:04o}", mask.bits())
    }

    /// Another process's umask, as /proc prints it (`0022`), where there is a /proc to read it
    /// from (Linux); `None` elsewhere.
    pub(crate) fn umask_of(pid: u32) -> Option<String> {
        let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
        status
            .lines()
            .find_map(|l| l.strip_prefix("Umask:").map(|v| v.trim().to_owned()))
    }

    fn uname_n() -> String {
        let out = Command::new("uname").arg("-n").output().unwrap();
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    }

    fn plain(c: char) -> char {
        if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
            c
        } else {
            '_'
        }
    }

    /// This host's name as the script has it: `uname -n`, other characters made `_`, at most 40.
    pub(crate) fn this_name() -> String {
        let name: String = uname_n().chars().map(plain).take(40).collect();
        if name.is_empty() {
            "host".to_owned()
        } else {
            name
        }
    }

    /// This host as the script names it: its name, `+` and the first boot-stable id there is:
    /// `hostid` (unless all zeros), else the first 12 characters of `/etc/machine-id` or
    /// `/var/lib/dbus/machine-id`; the name alone when there is none.
    pub(crate) fn this_host() -> String {
        let hostid = Command::new("hostid")
            .output()
            .ok()
            .map(|out| String::from_utf8(out.stdout).unwrap().trim().to_owned())
            .filter(|id| id.chars().any(|c| c != '0'));
        let id = hostid.or_else(|| {
            ["/etc/machine-id", "/var/lib/dbus/machine-id"]
                .iter()
                .filter_map(|file| std::fs::read_to_string(file).ok())
                .map(|text| text.lines().next().unwrap_or("").chars().take(12).collect())
                .find(|id: &String| !id.is_empty())
        });
        match id {
            Some(id) => format!(
                "{}+{}",
                this_name(),
                id.chars().map(plain).collect::<String>()
            ),
            None => this_name(),
        }
    }

    fn now_ms() -> i64 {
        let since = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap();
        i64::try_from(since.as_millis()).unwrap()
    }

    /// The pid of a process that has exited.
    pub(crate) fn dead_pid() -> u32 {
        let mut child = Command::new("true").spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        pid
    }

    pub(crate) fn script_len() -> u64 {
        SCRIPT.len() as u64
    }

    // ─── Deploy ────────────────────────────────────────────────────────────────────────────

    fn full_deploy_then_idempotent_rerun() {
        let m = Machine::new();
        let fake = m.fake(Remote::default());
        let helper = helper("1.0.0");
        let done = block_on(deploy(&m.target(&fake), &helper, &quick())).unwrap();
        assert!(done.uploaded);
        assert_eq!(done.version, "1.0.0");
        assert_eq!(done.sha256, helper.sha256());
        assert_eq!(done.hash_tool, HashTool::Sha256sum);
        assert_eq!(done.previous, None);
        assert!(done.removed.is_empty());
        assert!(done.atomic);
        assert_eq!(done.version_line, "pitcrewd 1.0.0 (protocol 1)");
        let binary = m.bin_dir().join("1.0.0/pitcrewd");
        assert_eq!(done.path, binary.to_str().unwrap());

        // On the machine: the binary in place, `current` relative, everything private.
        assert_eq!(std::fs::read(&binary).unwrap(), helper.bytes());
        assert_eq!(m.link("current").as_deref(), Some("1.0.0"));
        assert_eq!(mode(&binary), 0o700);
        assert_private(&m.root());
        assert!(!m.lock().exists());
        assert!(m.temporaries().is_empty());
        let out = Command::new(m.bin_dir().join("current/pitcrewd"))
            .arg("--version")
            .output()
            .unwrap();
        assert_eq!(out.stdout, b"pitcrewd 1.0.0 (protocol 1)\n");

        // Two calls: a check with the script alone, then the upload behind the script.
        let calls = fake.calls();
        assert_eq!(calls.len(), 2, "{calls:?}");
        assert!(calls[0].line.contains(" check "), "{}", calls[0].line);
        // The interpreter by its path, and the tool path behind the script's length.
        assert!(
            calls[0].line.starts_with("'/bin/sh' -c "),
            "{}",
            calls[0].line
        );
        let lengths = format!(
            " sh {} {} {} check ",
            SCRIPT.len(),
            SCRIPT.trim_end_matches('\n').len(),
            m.bin.display()
        );
        assert!(calls[0].line.contains(&lengths), "{}", calls[0].line);
        assert_eq!(calls[0].stdin, script_len());
        assert!(calls[1].line.contains(" install "), "{}", calls[1].line);
        assert_eq!(calls[1].stdin, script_len() + helper.len());
        // No secret and no binary on the command line: the version, its hash, its size, the
        // lock bounds.
        assert!(calls[1].line.contains(&format!(
            " 1.0.0 {} {} 5 5",
            helper.sha256(),
            helper.len()
        )));

        // The same deploy again only verifies: one call, nothing uploaded or rewritten.
        let before = std::fs::metadata(&binary).unwrap().modified().unwrap();
        let fake = m.fake(Remote::default());
        let again = block_on(deploy(&m.target(&fake), &helper, &quick())).unwrap();
        assert!(!again.uploaded);
        assert_eq!(again.sha256, helper.sha256());
        assert_eq!(again.removed, Vec::<String>::new());
        let calls = fake.calls();
        assert_eq!(calls.len(), 1, "{calls:?}");
        assert!(calls[0].line.contains(" check "));
        assert_eq!(calls[0].stdin, script_len());
        assert_eq!(
            std::fs::metadata(&binary).unwrap().modified().unwrap(),
            before
        );
    }

    fn progress_is_reported() {
        let m = Machine::new();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let options = DeployOptions {
            progress: Some(Arc::new({
                let seen = seen.clone();
                move |p: Progress| seen.lock().unwrap().push(p)
            })),
            ..quick()
        };
        let helper = big_helper("1.0.0", 400 * 1024);
        block_on(deploy(&m.plain(), &helper, &options)).unwrap();
        let seen = seen.lock().unwrap();
        assert!(seen.len() >= 6, "{}", seen.len());
        assert!(seen.windows(2).all(|w| w[0].sent <= w[1].sent));
        assert!(seen.iter().all(|p| p.total == helper.len()));
        assert_eq!(seen.last().unwrap().sent, helper.len());
    }

    fn a_hash_mismatch_removes_the_upload() {
        let m = Machine::new();
        block_on(deploy(&m.plain(), &helper("1.0.0"), &quick())).unwrap();
        // A byte flipped on the way.
        let fake = m.fake(Remote {
            corrupt_at: Some(script_len() + 10),
            ..Remote::default()
        });
        let second = helper("2.0.0");
        let err = block_on(deploy(&m.target(&fake), &second, &quick())).unwrap_err();
        match &err {
            HelperError::HashMismatch { expected, actual } => {
                assert_eq!(expected, second.sha256());
                assert_eq!(actual.len(), 64);
                assert_ne!(actual, expected);
            }
            other => panic!("{other:?}"),
        }
        assert!(err.to_string().contains("deleted"));
        assert!(!m.bin_dir().join("2.0.0/pitcrewd").exists());
        assert!(m.temporaries().is_empty());
        assert!(!m.lock().exists());
        assert_eq!(m.link("current").as_deref(), Some("1.0.0"));
    }

    fn an_interrupted_upload_leaves_nothing_in_place() {
        let m = Machine::new();
        let helper = big_helper("1.0.0", 256 * 1024);
        let cut = script_len() + 100_000;

        // The connection drops: the remote script sees end of file, and cleans up.
        let fake = m.fake(Remote {
            cut_after: Some(cut),
            ..Remote::default()
        });
        let err = block_on(deploy(&m.target(&fake), &helper, &quick())).unwrap_err();
        assert!(
            matches!(err, HelperError::Ssh(SshError::Ssh { code: 255, .. })),
            "{err:?}"
        );
        // DIAG (temporary): what is left when the clean-up does not come.
        let start = Instant::now();
        while !(m.temporaries().is_empty() && !m.lock().exists()) {
            assert!(
                start.elapsed() < Duration::from_secs(15),
                "timed out: the remote script to clean up\n{}",
                diag(&m)
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(!m.bin_dir().join("1.0.0/pitcrewd").exists());
        assert_eq!(m.link("current"), None);

        // The remote script is killed outright: its partial file and its lock stay behind,
        // but nothing is in place...
        let fake = m.fake(Remote {
            cut_after: Some(cut),
            kill: true,
            ..Remote::default()
        });
        let err = block_on(deploy(&m.target(&fake), &helper, &quick())).unwrap_err();
        assert!(
            matches!(err, HelperError::Ssh(SshError::Ssh { code: 255, .. })),
            "{err:?}"
        );
        std::thread::sleep(Duration::from_millis(300));
        let left = m.temporaries();
        assert_eq!(left.len(), 1, "{left:?}");
        assert!(std::fs::metadata(&left[0]).unwrap().len() < helper.len());
        assert_eq!(mode(&left[0]), 0o600);
        assert!(m.lock().exists());
        assert!(!m.bin_dir().join("1.0.0/pitcrewd").exists());
        assert_eq!(m.link("current"), None);
        assert_private(&m.root());

        // ...and the next deploy breaks the dead process's lock at once and sweeps the file.
        let start = Instant::now();
        let done = block_on(deploy(&m.plain(), &helper, &quick())).unwrap();
        assert!(done.uploaded);
        assert!(
            start.elapsed() < Duration::from_secs(4),
            "{:?}",
            start.elapsed()
        );
        assert!(m.temporaries().is_empty());
        assert!(!m.lock().exists());
        assert_eq!(
            std::fs::read(m.bin_dir().join("1.0.0/pitcrewd")).unwrap(),
            helper.bytes()
        );
    }

    fn concurrent_deploys_take_turns() {
        let m = Machine::new();
        let first = big_helper("1.0.0", 200 * 1024);
        let slow = m.fake(Remote {
            pause_after: Some(script_len() + 1000),
            pause_ms: 2500,
            ..Remote::default()
        });
        let target = m.target(&slow);
        let a = std::thread::spawn({
            let first = first.clone();
            move || block_on(deploy(&target, &first, &quick()))
        });
        eventually("the first upload to pause", || slow.paused());
        // The remote side reads the script byte by byte before it gets to the upload.
        eventually("the first upload to begin", || m.temporaries().len() == 1);
        assert!(m.lock().exists());
        let start = Instant::now();
        let second = helper("2.0.0");
        // DIAG (temporary): the state when the wait gives up.
        let b = block_on(deploy(&m.plain(), &second, &quick())).unwrap_or_else(|e| {
            panic!(
                "{e:?} after {:?}; first finished: {}\n{}",
                start.elapsed(),
                a.is_finished(),
                diag(&m)
            )
        });
        let waited = start.elapsed();
        let a = a.join().unwrap().unwrap();
        assert!(a.uploaded && b.uploaded);
        assert!(waited >= Duration::from_secs(1), "{waited:?}");
        assert_eq!(m.link("current").as_deref(), Some("2.0.0"));
        assert_eq!(m.link("previous").as_deref(), Some("1.0.0"));
        assert_eq!(
            std::fs::read(m.bin_dir().join("1.0.0/pitcrewd")).unwrap(),
            first.bytes()
        );
        assert_eq!(
            std::fs::read(m.bin_dir().join("2.0.0/pitcrewd")).unwrap(),
            second.bytes()
        );

        // A deploy that will not wait long enough fails, and changes nothing.
        let slow = m.fake(Remote {
            pause_after: Some(script_len() + 1000),
            pause_ms: 3000,
            ..Remote::default()
        });
        let target = m.target(&slow);
        let third = big_helper("3.0.0", 200 * 1024);
        let a = std::thread::spawn({
            let third = third.clone();
            move || block_on(deploy(&target, &third, &quick()))
        });
        eventually("the third upload to pause", || slow.paused());
        eventually("the third upload to begin", || m.temporaries().len() == 1);
        let impatient = DeployOptions {
            lock_wait: Duration::from_secs(1),
            ..quick()
        };
        let err = block_on(deploy(&m.plain(), &helper("4.0.0"), &impatient)).unwrap_err();
        assert!(matches!(&err, HelperError::Busy(_)), "{err:?}");
        assert!(err.to_string().contains(&uname_n()), "{err}");
        a.join().unwrap().unwrap();
        assert_eq!(m.link("current").as_deref(), Some("3.0.0"));
        assert_eq!(m.link("previous").as_deref(), Some("2.0.0"));
        assert_eq!(m.versions(), ["2.0.0", "3.0.0"]);
        assert!(!m.lock().exists());
    }

    fn stale_locks_are_broken_and_live_ones_waited_for() {
        let m = Machine::new();
        private_dir(&m.bin_dir());
        let lock = m.lock();
        let impatient = DeployOptions {
            lock_wait: Duration::from_secs(1),
            ..quick()
        };

        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        // Sets the lock directory's time to `minutes` ago (from here, not `touch`: GNU's and
        // BSD's take different date forms).
        let touch = |minutes: u64| {
            let when = SystemTime::now() - Duration::from_secs(minutes * 60);
            std::fs::File::open(&lock)
                .unwrap()
                .set_modified(when)
                .unwrap();
        };

        // A fresh lock from another host (a login node sharing this home): it is waited for,
        // even though the time it records is old, since that host's clock is not this one.
        private_dir(&lock);
        std::fs::write(lock.join("owner"), "elsewhere 1 00ff 946684800\n").unwrap();
        let err = block_on(deploy(&m.plain(), &helper("1.0.0"), &impatient)).unwrap_err();
        assert!(
            matches!(&err, HelperError::Busy(d) if d.contains("elsewhere 1")),
            "{err:?}"
        );
        assert!(lock.exists());

        // Its directory a little older than stale_lock (5 minutes) is not enough: clocks may
        // differ by up to 10 minutes...
        touch(8);
        let err = block_on(deploy(&m.plain(), &helper("1.0.0"), &impatient)).unwrap_err();
        assert!(matches!(err, HelperError::Busy(_)), "{err:?}");

        // ...beyond that it is broken.
        touch(16);
        block_on(deploy(&m.plain(), &helper("1.0.0"), &impatient)).unwrap();
        assert!(!lock.exists());

        // A lock taken on this host by a process that is gone is broken at once.
        private_dir(&lock);
        std::fs::write(
            lock.join("owner"),
            format!("{} {} 00ff {now}\n", this_host(), dead_pid()),
        )
        .unwrap();
        block_on(deploy(&m.plain(), &helper("2.0.0"), &impatient)).unwrap();
        assert!(!lock.exists());
        let leftovers: Vec<_> = std::fs::read_dir(m.bin_dir())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .filter(|n| n.starts_with(".lock"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");

        // A live process of this host keeps its lock...
        private_dir(&lock);
        let live = format!("{} {} 00ff {now}\n", this_host(), std::process::id());
        std::fs::write(lock.join("owner"), &live).unwrap();
        let err = block_on(deploy(&m.plain(), &helper("3.0.0"), &impatient)).unwrap_err();
        assert!(matches!(err, HelperError::Busy(_)), "{err:?}");
        assert_eq!(std::fs::read_to_string(lock.join("owner")).unwrap(), live);

        // ...until it has held it longer than stale_lock by this host's own clock, whatever
        // the directory's time says.
        std::fs::write(
            lock.join("owner"),
            format!(
                "{} {} 00ff {}\n",
                this_host(),
                std::process::id(),
                now - 600
            ),
        )
        .unwrap();
        touch(0);
        block_on(deploy(&m.plain(), &helper("3.0.0"), &impatient)).unwrap();
        assert!(!lock.exists());

        // This host's name with another id (a stateless node makes a new machine id at every
        // boot), or with none, is this host: a lock whose process is gone is broken at once.
        for host in [format!("{}+0123456789ab", this_name()), this_name()] {
            private_dir(&lock);
            let owner = format!("{host} {} 00ff {now}\n", dead_pid());
            std::fs::write(lock.join("owner"), &owner).unwrap();
            block_on(deploy(&m.plain(), &helper("4.0.0"), &impatient)).unwrap();
            assert!(!lock.exists(), "{owner}");
        }
        // Another name, even with this host's id, is another host: judged by the directory's
        // age, though the pid it names is gone here.
        private_dir(&lock);
        let id = this_host()
            .split_once('+')
            .map(|(_, id)| format!("+{id}"))
            .unwrap_or_default();
        let other = format!("another-{}{id} {} 00ff {now}\n", this_name(), dead_pid());
        std::fs::write(lock.join("owner"), &other).unwrap();
        let err = block_on(deploy(&m.plain(), &helper("4.0.0"), &impatient)).unwrap_err();
        assert!(matches!(err, HelperError::Busy(_)), "{err:?}");
        std::fs::remove_dir_all(&lock).unwrap();

        // This host's lock whose pid or time cannot be read: judged by the directory's age too,
        // waited for while fresh and broken once old.
        for owner in [
            format!("{} {} 00ff not-a-time\n", this_host(), std::process::id()),
            format!("{} not-a-pid 00ff {now}\n", this_host()),
        ] {
            private_dir(&lock);
            std::fs::write(lock.join("owner"), &owner).unwrap();
            let err = block_on(deploy(&m.plain(), &helper("4.0.0"), &impatient)).unwrap_err();
            assert!(matches!(err, HelperError::Busy(_)), "{owner}: {err:?}");
            touch(16);
            block_on(deploy(&m.plain(), &helper("4.0.0"), &impatient)).unwrap();
            assert!(!lock.exists(), "{owner}");
        }
    }

    fn gc_keeps_exactly_two_versions() {
        let m = Machine::new();
        let mut last = None;
        for v in ["1.0.0", "2.0.0", "3.0.0"] {
            last = Some(block_on(deploy(&m.plain(), &helper(v), &quick())).unwrap());
        }
        let last = last.unwrap();
        assert_eq!(last.removed, ["1.0.0"]);
        assert_eq!(last.previous.as_deref(), Some("2.0.0"));
        assert_eq!(m.versions(), ["2.0.0", "3.0.0"]);
        let mut entries: Vec<String> = std::fs::read_dir(m.bin_dir())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        entries.sort();
        assert_eq!(entries, ["2.0.0", "3.0.0", "current", "previous"]);
        assert_eq!(m.link("current").as_deref(), Some("3.0.0"));
        assert_eq!(m.link("previous").as_deref(), Some("2.0.0"));

        // Rolling back to the previous version uploads nothing and keeps the newer one.
        let back = block_on(deploy(&m.plain(), &helper("2.0.0"), &quick())).unwrap();
        assert!(!back.uploaded);
        assert_eq!(back.previous.as_deref(), Some("3.0.0"));
        assert!(back.removed.is_empty());
        assert_eq!(m.link("current").as_deref(), Some("2.0.0"));
        assert_eq!(m.versions(), ["2.0.0", "3.0.0"]);

        // What is not a version directory is left alone.
        private_dir(&m.bin_dir().join("notes"));
        std::fs::write(m.bin_dir().join("9.9.9"), "a file").unwrap();
        let next = block_on(deploy(&m.plain(), &helper("4.0.0"), &quick())).unwrap();
        assert_eq!(next.removed, ["3.0.0"]);
        assert_eq!(m.versions(), ["2.0.0", "4.0.0", "notes"]);
        assert!(m.bin_dir().join("9.9.9").is_file());
    }

    fn a_damaged_install_is_replaced() {
        let m = Machine::new();
        let helper = helper("1.0.0");
        block_on(deploy(&m.plain(), &helper, &quick())).unwrap();
        let binary = m.bin_dir().join("1.0.0/pitcrewd");
        std::fs::write(&binary, "#!/bin/sh\necho tampered\n").unwrap();
        let done = block_on(deploy(&m.plain(), &helper, &quick())).unwrap();
        assert!(done.uploaded);
        assert_eq!(std::fs::read(&binary).unwrap(), helper.bytes());
        assert_eq!(mode(&binary), 0o700);
    }

    fn every_sha256_tool_works() {
        type Tools = &'static [(&'static str, &'static str)];
        let cases: [(Tools, HashTool); 5] = [
            (&[("sha256sum", "sha256sum")], HashTool::Sha256sum),
            (&[("shasum", "shasum")], HashTool::Shasum),
            (&[("openssl", "openssl3")], HashTool::Openssl),
            (&[("openssl", "openssl1")], HashTool::Openssl),
            // A tool that prints no hash, or fails, falls through to the next.
            (
                &[
                    ("sha256sum", "garbage"),
                    ("shasum", "fails"),
                    ("openssl", "openssl3"),
                ],
                HashTool::Openssl,
            ),
        ];
        for (tools, want) in cases {
            let m = Machine::with_tools(|bin| {
                std::fs::remove_file(bin.join("sha256sum")).unwrap();
                for (name, kind) in tools {
                    fake_tool(bin, name, kind);
                }
            });
            let helper = helper("1.0.0");
            let done = block_on(deploy(&m.plain(), &helper, &quick())).unwrap();
            assert_eq!(done.hash_tool, want, "{tools:?}");
            assert!(done.uploaded);
            let again = block_on(deploy(&m.plain(), &helper, &quick())).unwrap();
            assert_eq!(
                (again.hash_tool, again.uploaded),
                (want, false),
                "{tools:?}"
            );
        }
        // The real tools of this machine, each alone.
        let mut real = Vec::new();
        for (name, want) in [
            ("sha256sum", HashTool::Sha256sum),
            ("shasum", HashTool::Shasum),
            ("openssl", HashTool::Openssl),
        ] {
            let Some(path) = which(name) else {
                println!("note: no {name} on this machine");
                continue;
            };
            let m = Machine::with_tools(|bin| {
                std::fs::remove_file(bin.join("sha256sum")).unwrap();
                std::os::unix::fs::symlink(&path, bin.join(name)).unwrap();
            });
            let done = block_on(deploy(&m.plain(), &helper("1.0.0"), &quick())).unwrap();
            assert_eq!(done.hash_tool, want, "{name}");
            real.push(name);
        }
        println!("real sha256 tools checked: {real:?}");
    }

    fn no_sha256_tool_is_refused_before_uploading() {
        let m = Machine::with_tools(|bin| std::fs::remove_file(bin.join("sha256sum")).unwrap());
        let fake = m.fake(Remote::default());
        let err = block_on(deploy(&m.target(&fake), &helper("1.0.0"), &quick())).unwrap_err();
        assert!(matches!(err, HelperError::NoHashTool), "{err:?}");
        let calls = fake.calls();
        assert_eq!(calls.len(), 1, "{calls:?}");
        assert!(calls[0].line.contains(" check "));
        assert!(!m.bin_dir().join("1.0.0").exists());
        assert!(!m.lock().exists());
    }

    fn unknown_platforms_are_refused() {
        for (s, arch, os_seen, arch_seen) in [
            ("FreeBSD", "amd64", "freebsd", "x86_64"),
            ("Linux", "ppc64le", "linux", "ppc64le"),
            ("Linux", "armv7l", "linux", "armv7l"),
        ] {
            let m = Machine::with_tools(|bin| {
                std::fs::remove_file(bin.join("uname")).unwrap();
                shim(
                    bin,
                    "uname",
                    &format!(
                        "case \"$1\" in -s) echo {s} ;; -m) echo {arch} ;; *) echo box ;; esac"
                    ),
                );
            });
            let fake = m.fake(Remote::default());
            let probe = block_on(fake.ssh.probe("cluster")).unwrap();
            let err = Target::new(fake.ssh.clone(), "cluster", &probe).unwrap_err();
            match &err {
                HelperError::UnsupportedPlatform { os, arch } => {
                    assert_eq!((os.as_str(), arch.as_str()), (os_seen, arch_seen));
                }
                other => panic!("{other:?}"),
            }
            assert!(err.to_string().contains(arch_seen), "{err}");
            assert!(!m.root().exists());
        }

        // This machine itself, through the same probe.
        let m = Machine::new();
        let fake = m.fake(Remote::default());
        let probe = block_on(fake.ssh.probe("cluster")).unwrap();
        let target = Target::new(fake.ssh.clone(), "cluster", &probe).unwrap();
        assert_eq!(target.layout(), &m.layout());
        let want = match (std::env::consts::OS, std::env::consts::ARCH) {
            ("macos", _) => Platform::MacOs,
            (_, "aarch64") => Platform::LinuxAarch64,
            _ => Platform::LinuxX86_64,
        };
        assert_eq!(target.platform(), want);

        // A helper built for another platform is refused before any call.
        let fake = m.fake(Remote::default());
        let bytes = helper_script("1.0.0", "serve", 0);
        let mac = Helper::new(
            Platform::MacOs,
            "1.0.0",
            &hex(&Sha256::digest(&bytes)),
            bytes,
        )
        .unwrap();
        let err = block_on(deploy(&m.target(&fake), &mac, &quick())).unwrap_err();
        assert!(
            matches!(
                err,
                HelperError::WrongPlatform {
                    built: Platform::MacOs,
                    machine: Platform::LinuxX86_64
                }
            ),
            "{err:?}"
        );
        assert!(fake.calls().is_empty());
    }

    fn bad_helpers_are_removed() {
        let m = Machine::new();
        block_on(deploy(&m.plain(), &helper("1.0.0"), &quick())).unwrap();
        let after = |v: &str| {
            assert!(!m.bin_dir().join(v).join("pitcrewd").exists(), "{v}");
            assert!(m.temporaries().is_empty(), "{v}");
            assert!(!m.lock().exists(), "{v}");
            assert_eq!(m.link("current").as_deref(), Some("1.0.0"), "{v}");
        };

        // It names another version.
        let liar = helper_from("2.0.0", helper_script("9.9.9", "serve", 0));
        let err = block_on(deploy(&m.plain(), &liar, &quick())).unwrap_err();
        match &err {
            HelperError::VersionMismatch { expected, reported } => {
                assert_eq!(expected, "2.0.0");
                assert_eq!(reported, "pitcrewd 9.9.9 (protocol 1)");
            }
            other => panic!("{other:?}"),
        }
        after("2.0.0");

        // `--version` fails.
        let failing = helper_from(
            "3.0.0",
            b"#!/bin/sh\necho 'broken: missing library' >&2\nexit 3\n".to_vec(),
        );
        let err = block_on(deploy(&m.plain(), &failing, &quick())).unwrap_err();
        match &err {
            HelperError::NotRunnable { code, output } => {
                assert_eq!(*code, Some(3));
                assert_eq!(output, "broken: missing library");
            }
            other => panic!("{other:?}"),
        }
        after("3.0.0");

        // Not a program at all (a binary for another platform, say).
        let junk = helper_from("4.0.0", vec![0x7f, b'E', b'L', b'F', 2, 1, 1, 0, 0, 0]);
        let err = block_on(deploy(&m.plain(), &junk, &quick())).unwrap_err();
        assert!(
            matches!(&err, HelperError::NotRunnable { code: Some(c), .. } if *c != 0),
            "{err:?}"
        );
        after("4.0.0");
    }

    fn the_upload_is_never_readable_by_others() {
        let m = Machine::new();
        let helper = big_helper("1.0.0", 200 * 1024);
        let slow = m.fake(Remote {
            pause_after: Some(script_len() + 1000),
            pause_ms: 1500,
            ..Remote::default()
        });
        let target = m.target(&slow);
        let a = std::thread::spawn({
            let helper = helper.clone();
            move || block_on(deploy(&target, &helper, &quick()))
        });
        eventually("the upload to pause", || slow.paused());
        // The remote side reads the script byte by byte before it gets to the upload.
        eventually("the upload to begin", || m.temporaries().len() == 1);
        let partial = m.temporaries();
        assert_eq!(mode(&partial[0]), 0o600);
        assert_private(&m.root());
        a.join().unwrap().unwrap();
        assert_eq!(mode(&m.bin_dir().join("1.0.0/pitcrewd")), 0o700);
        assert_private(&m.root());
    }

    fn unsafe_directories_are_refused() {
        // Open to others: refused, and left as it was.
        let m = Machine::new();
        std::fs::create_dir(m.root()).unwrap();
        std::fs::set_permissions(m.root(), std::fs::Permissions::from_mode(0o755)).unwrap();
        let fake = m.fake(Remote::default());
        let err = block_on(deploy(&m.target(&fake), &helper("1.0.0"), &quick())).unwrap_err();
        assert!(
            matches!(&err, HelperError::UnsafeDirectory(d) if d.contains("drwxr-xr-x")),
            "{err:?}"
        );
        assert_eq!(fake.calls().len(), 1);
        assert_eq!(mode(&m.root()), 0o755);
        assert!(!m.bin_dir().exists());

        // A symbolic link, even to a private directory.
        let m = Machine::new();
        let elsewhere = m.dir.path().join("elsewhere");
        private_dir(&elsewhere);
        std::os::unix::fs::symlink(&elsewhere, m.root()).unwrap();
        let err = block_on(deploy(&m.plain(), &helper("1.0.0"), &quick())).unwrap_err();
        assert!(
            matches!(&err, HelperError::UnsafeDirectory(d) if d.contains("symbolic link")),
            "{err:?}"
        );

        // A version directory open to the group.
        let m = Machine::new();
        block_on(deploy(&m.plain(), &helper("1.0.0"), &quick())).unwrap();
        let dir = m.bin_dir().join("1.0.0");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o750)).unwrap();
        let err = block_on(deploy(&m.plain(), &helper("1.0.0"), &quick())).unwrap_err();
        assert!(matches!(err, HelperError::UnsafeDirectory(_)), "{err:?}");

        // The launchers check too.
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        private_dir(&m.run_dir());
        std::fs::set_permissions(m.run_dir(), std::fs::Permissions::from_mode(0o770)).unwrap();
        let launcher = DirectLauncher::new(launch_options());
        let err = block_on(launcher.start(&m.plain())).unwrap_err();
        assert!(matches!(err, HelperError::UnsafeDirectory(_)), "{err:?}");
        let err = block_on(launcher.status(&m.plain())).unwrap_err();
        assert!(matches!(err, HelperError::UnsafeDirectory(_)), "{err:?}");
    }

    fn mv_without_t_falls_back() {
        let real = which("mv").unwrap();
        let real = real.display();
        // BSD: -h instead of -T. Played over the real `mv`, with its own flag for it (macOS's is
        // BSD's already).
        let no_follow = if cfg!(target_os = "macos") {
            "-h"
        } else {
            "-T"
        };
        let bsd = format!(
            "case \"$1\" in\n\
             -T) echo 'mv: illegal option -- T' >&2; exit 64 ;;\n\
             -h) shift; exec '{real}' {no_follow} \"$@\" ;;\n\
             esac\n\
             exec '{real}' \"$@\""
        );
        // busybox: neither.
        let busybox = format!(
            "case \"$1\" in -T|-h) echo \"mv: invalid option -- '$1'\" >&2; exit 1 ;; esac\n\
             exec '{real}' \"$@\""
        );
        for (name, body, atomic) in [("bsd", bsd, true), ("busybox", busybox, false)] {
            let m = Machine::with_tools(|bin| {
                std::fs::remove_file(bin.join("mv")).unwrap();
                shim(bin, "mv", &body);
            });
            for (i, v) in ["1.0.0", "2.0.0", "3.0.0"].into_iter().enumerate() {
                let done = block_on(deploy(&m.plain(), &helper(v), &quick())).unwrap();
                assert_eq!(done.atomic, atomic, "{name} {v}");
                assert_eq!(m.link("current").as_deref(), Some(v), "{name}");
                if i > 0 {
                    assert!(m.link("previous").is_some(), "{name}");
                }
            }
            assert_eq!(m.versions(), ["2.0.0", "3.0.0"], "{name}");
            let leftovers: Vec<_> = std::fs::read_dir(m.bin_dir())
                .unwrap()
                .map(|e| e.unwrap().file_name().into_string().unwrap())
                .filter(|n| n.contains(".tmp."))
                .collect();
            assert!(leftovers.is_empty(), "{name}: {leftovers:?}");
        }
    }

    fn a_stalled_upload_times_out() {
        let m = Machine::new();
        let fake = m.fake(Remote {
            hang_on: Some("install".to_owned()),
            ..Remote::default()
        });
        let helper = big_helper("1.0.0", 1024 * 1024);
        let options = DeployOptions {
            timeout: Duration::from_secs(2),
            lock_wait: Duration::from_secs(1),
            stale_lock: Duration::from_secs(60),
            progress: None,
        };
        let start = Instant::now();
        let err = block_on(deploy(&m.target(&fake), &helper, &options)).unwrap_err();
        assert!(
            matches!(err, HelperError::Ssh(SshError::TimedOut(_))),
            "{err:?}"
        );
        assert!(start.elapsed() < Duration::from_secs(15));
        assert_eq!(fake.calls().len(), 2);
        assert!(!m.lock().exists());
        assert!(!m.bin_dir().join("1.0.0/pitcrewd").exists());
    }

    /// Every directory on the way to the root must belong to root or the user, and be
    /// writable by no one else unless sticky; symbolic links on the way are followed.
    fn the_way_to_the_root_is_checked() {
        let m = Machine::new();
        // Canonical: macOS's /var is a link to /private/var, which the walk resolves, and the
        // stand-in `ls` below must be asked about the paths it names.
        let base = m.dir.path().canonicalize().unwrap();
        let at = |root: &Path| {
            m.target_at(
                &m.fake(Remote::default()),
                Layout::at(root.to_str().unwrap()).unwrap(),
            )
        };
        let dir = |path: &Path, mode: u32| {
            std::fs::create_dir_all(path).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
        };
        let unsafe_way = |root: &Path, why: &str| {
            let err = block_on(deploy(&at(root), &helper("1.0.0"), &quick())).unwrap_err();
            assert!(
                matches!(&err, HelperError::UnsafeDirectory(d) if d.contains(why)),
                "{}: {err:?}",
                root.display()
            );
            assert!(!root.exists(), "{}", root.display());
        };

        // Writable by the group (a shared project directory, set-group-ID or not) or by
        // everyone, without the sticky bit: anyone there could swap the tree after the checks.
        for (name, mode) in [("group", 0o775), ("project", 0o2770), ("open", 0o777)] {
            dir(&base.join(name), mode);
            dir(&base.join(name).join("u"), 0o755);
            unsafe_way(&base.join(name).join("u/.pitcrew"), "writable by others");
        }
        // Sticky and world-writable, as /tmp: no one can rename what is not theirs. (Every
        // test machine's home is under /tmp too.)
        dir(&base.join("sticky"), 0o1777);
        dir(&base.join("sticky/u"), 0o755);
        let root = base.join("sticky/u/.pitcrew");
        block_on(deploy(&at(&root), &helper("1.0.0"), &quick())).unwrap();
        assert!(root.join("bin/1.0.0/pitcrewd").is_file());

        // Symbolic links on the way are followed, absolute or relative, through `..`...
        dir(&base.join("real/u"), 0o755);
        dir(&base.join("sub"), 0o755);
        std::os::unix::fs::symlink(base.join("real"), base.join("link")).unwrap();
        std::os::unix::fs::symlink("sub/../real", base.join("rel")).unwrap();
        for via in ["link", "rel"] {
            let root = base.join(via).join("u/.pitcrew");
            block_on(deploy(&at(&root), &helper("1.0.0"), &quick())).unwrap();
            assert!(
                base.join("real/u/.pitcrew/bin/1.0.0/pitcrewd").is_file(),
                "{via}"
            );
        }
        // ...and where they lead must be safe, and so must the directory holding them.
        std::os::unix::fs::symlink(base.join("open"), base.join("to-open")).unwrap();
        unsafe_way(&base.join("to-open/u/.pitcrew"), "writable by others");
        dir(&base.join("open2"), 0o777);
        dir(&base.join("real2/u"), 0o755);
        std::os::unix::fs::symlink(base.join("real2"), base.join("open2/in")).unwrap();
        unsafe_way(&base.join("open2/in/u/.pitcrew"), "writable by others");

        // The launchers check the way too.
        let launcher = DirectLauncher::new(launch_options());
        let group = at(&base.join("group/u/.pitcrew"));
        for err in [
            block_on(launcher.status(&group)).unwrap_err(),
            block_on(launcher.start(&group)).unwrap_err(),
            block_on(launcher.stop(&group)).unwrap_err(),
        ] {
            assert!(matches!(err, HelperError::UnsafeDirectory(_)), "{err:?}");
        }

        // A directory on the way that belongs to another user, and a symbolic link on the way
        // that does (an `ls` that says so): in a sticky directory, its owner could swap it.
        dir(&base.join("theirs/u"), 0o755);
        let theirs = base.join("theirs");
        dir(&base.join("real3/u"), 0o755);
        let their_link = base.join("their-link");
        std::os::unix::fs::symlink(base.join("real3"), &their_link).unwrap();
        let real = which("ls").unwrap();
        shim(
            &m.bin,
            "ls",
            &format!(
                "last=\nfor a in \"$@\"; do last=$a; done\n\
                 case $last in '{}'|'{}') '{}' \"$@\" | awk '{{ $3 = 4242; print }}' ;; \
                 *) exec '{}' \"$@\" ;; esac",
                theirs.display(),
                their_link.display(),
                real.display(),
                real.display()
            ),
        );
        unsafe_way(&theirs.join("u/.pitcrew"), "belongs to uid 4242");
        unsafe_way(
            &their_link.join("u/.pitcrew"),
            &format!("the link {}", their_link.display()),
        );
        assert!(!base.join("real3/u/.pitcrew").exists());
    }

    /// An access control list on a directory on the way is judged by what it grants where `ls`
    /// can list it (macOS's `ls -le`), and refused where it cannot be read or the system is not
    /// one whose ACLs the check knows: on macOS the mode bits alone can look safe. Here a
    /// stand-in `uname` and `ls` play each system, so this runs anywhere.
    fn an_acl_on_the_way_is_judged_by_what_it_grants() {
        let m = Machine::new();
        // Canonical, as in `the_way_to_the_root_is_checked`.
        let base = m.dir.path().canonicalize().unwrap();
        let acl = base.join("acl");
        std::fs::create_dir_all(acl.join("u")).unwrap();
        std::fs::set_permissions(&acl, std::fs::Permissions::from_mode(0o755)).unwrap();
        let root = acl.join("u/.pitcrew");
        let (ls, uname) = (which("ls").unwrap(), which("uname").unwrap());
        // `uname -s` says `os`; `acl` shows a `+`, and `ls -le` lists `entries` for it (`None`:
        // it fails).
        let system = |os: &str, entries: Option<&[&str]>| {
            shim(
                &m.bin,
                "uname",
                &format!(
                    "case $1 in -s) echo {os} ;; *) exec '{}' \"$@\" ;; esac",
                    uname.display()
                ),
            );
            let plus = format!(
                "'{}' -ldn \"$last\" | awk '{{ $1 = $1 \"+\"; print }}'",
                ls.display()
            );
            let list = match entries {
                Some(entries) => {
                    let quoted: Vec<String> = entries.iter().map(|e| format!("'{e}'")).collect();
                    format!("{plus}; printf '%s\\n' {}; exit", quoted.join(" "))
                }
                None => "exit 1".to_owned(),
            };
            shim(
                &m.bin,
                "ls",
                &format!(
                    "last=\nfor a in \"$@\"; do last=$a; done\n\
                     if [ \"$last\" = '{}' ]; then\n\
                     case $1 in -ldn) {plus}; exit ;; -lde) {list} ;; esac\n\
                     fi\n\
                     exec '{}' \"$@\"",
                    acl.display(),
                    ls.display()
                ),
            );
        };
        let deploy_here = || {
            let target = m.target_at(
                &m.fake(Remote::default()),
                Layout::at(root.to_str().unwrap()).unwrap(),
            );
            block_on(deploy(&target, &helper("1.0.0"), &quick()))
        };
        let refused = |why: &str| {
            let err = deploy_here().unwrap_err();
            assert!(
                matches!(&err, HelperError::UnsafeDirectory(d) if d.contains(why)),
                "{why}: {err:?}"
            );
            assert!(!root.exists());
        };

        // macOS: an entry that lets someone else add, remove or rename what is in it.
        system(
            "Darwin",
            Some(&[
                " 0: group:everyone deny delete",
                " 1: user:mallory allow list,add_file,delete_child",
            ]),
        );
        refused("lets others change it:  1: user:mallory allow list,add_file,delete_child");
        system("Darwin", Some(&[" 0: user:mallory allow write"]));
        refused("allow write");
        // An entry not in the form the check knows, or a list that cannot be read.
        system("Darwin", Some(&[" 0: something else"]));
        refused("lets others change it:  0: something else");
        system("Darwin", None);
        refused("cannot read the access control list");
        // A system whose ACLs the check does not know, or one whose name it cannot read.
        system("FreeBSD", Some(&[]));
        refused("which cannot be checked on FreeBSD");
        system("", Some(&[]));
        refused("which cannot be checked on this system");

        // Linux: a POSIX ACL's grants are bounded by the group bits, checked already (r-x here).
        system("Linux", None);
        deploy_here().unwrap();
        assert!(root.join("bin/1.0.0/pitcrewd").is_file());
        std::fs::remove_dir_all(&root).unwrap();
        // macOS: entries that deny, or allow only reading and searching, as on a home folder.
        system(
            "Darwin",
            Some(&[
                " 0: group:everyone deny delete",
                " 1: user:_spotlight inherited allow list,search,readattr,file_inherit,directory_inherit",
            ]),
        );
        deploy_here().unwrap();
        assert!(root.join("bin/1.0.0/pitcrewd").is_file());
    }

    /// The same with macOS's own ACLs (`chmod +a`) and its own `ls -le`.
    fn a_macos_acl_on_the_way_is_judged_by_what_it_grants() {
        if !cfg!(target_os = "macos") {
            eprintln!("skipped: needs macOS's chmod +a and ls -le");
            return;
        }
        let m = Machine::new();
        let base = m.dir.path().canonicalize().unwrap();
        let acl = base.join("acl");
        std::fs::create_dir_all(acl.join("u")).unwrap();
        std::fs::set_permissions(&acl, std::fs::Permissions::from_mode(0o755)).unwrap();
        let root = acl.join("u/.pitcrew");
        let chmod = |args: &[&str]| {
            let status = Command::new("/bin/chmod")
                .args(args)
                .arg(&acl)
                .status()
                .unwrap();
            assert!(status.success(), "chmod {args:?}");
        };
        let deploy_here = || {
            let target = m.target_at(
                &m.fake(Remote::default()),
                Layout::at(root.to_str().unwrap()).unwrap(),
            );
            block_on(deploy(&target, &helper("1.0.0"), &quick()))
        };
        // The mode bits say drwxr-xr-x; the ACL lets everyone add and remove entries.
        chmod(&[
            "+a",
            "everyone allow add_file,add_subdirectory,delete_child",
        ]);
        let err = deploy_here().unwrap_err();
        assert!(
            matches!(&err, HelperError::UnsafeDirectory(d) if d.contains("lets others change it")),
            "{err:?}"
        );
        assert!(!root.exists());
        // One that only denies, as on every macOS home folder, is fine.
        chmod(&["-N"]);
        chmod(&["+a", "everyone deny delete"]);
        deploy_here().unwrap();
        assert!(root.join("bin/1.0.0/pitcrewd").is_file());
    }

    /// A shell start-up file that reads stdin eats the start of the script: what is left must
    /// not run, even a tail that would remove things.
    fn a_partly_eaten_script_never_runs() {
        let m = Machine::new();
        // A tail starting at the first `rm -rf "$1"` would remove `$1`, the command name,
        // relative to the remote's working directory, its home.
        let decoy = m.home.join("check");
        private_dir(&decoy);
        let rm = SCRIPT.find("rm -rf \"$1\"").unwrap();
        let line = SCRIPT[..rm].rfind('\n').unwrap() + 1;
        for eat in [1, 10, line as u64] {
            let fake = m.fake(Remote {
                eat: Some(eat),
                ..Remote::default()
            });
            let err = block_on(deploy(&m.target(&fake), &helper("1.0.0"), &quick())).unwrap_err();
            assert!(
                matches!(&err, HelperError::UnexpectedOutput(d) if d.contains("did not arrive whole")),
                "{eat}: {err:?}"
            );
            assert!(decoy.is_dir(), "{eat}");
            assert!(!m.root().exists(), "{eat}");
        }

        // In an upload the helper's own bytes make up the length: still refused.
        block_on(deploy(&m.plain(), &helper("1.0.0"), &quick())).unwrap();
        let fake = m.fake(Remote {
            eat: Some(100),
            eat_on: Some("install".to_owned()),
            ..Remote::default()
        });
        let err = block_on(deploy(&m.target(&fake), &helper("2.0.0"), &quick())).unwrap_err();
        assert!(
            matches!(&err, HelperError::UnexpectedOutput(d) if d.contains("did not arrive whole")),
            "{err:?}"
        );
        assert!(!m.bin_dir().join("2.0.0/pitcrewd").exists());
        assert!(m.temporaries().is_empty());
        assert_eq!(m.link("current").as_deref(), Some("1.0.0"));
        assert!(decoy.is_dir());
    }

    /// A deploy whose lock was broken stops before its next change, and leaves the new
    /// holder's lock alone.
    fn a_lost_lock_stops_the_deploy() {
        let m = Machine::new();
        block_on(deploy(&m.plain(), &helper("1.0.0"), &quick())).unwrap();
        let slow = m.fake(Remote {
            pause_after: Some(script_len() + 1000),
            pause_ms: 1500,
            ..Remote::default()
        });
        let target = m.target(&slow);
        let second = big_helper("2.0.0", 100 * 1024);
        let a = std::thread::spawn(move || block_on(deploy(&target, &second, &quick())));
        eventually("the upload to pause", || slow.paused());
        eventually("the upload to begin", || m.temporaries().len() == 1);
        // Another run takes the lock over, as one that misjudged it stale would.
        let thief = "elsewhere 4242 00ff 0\n";
        std::fs::write(m.lock().join("owner"), thief).unwrap();
        let err = a.join().unwrap().unwrap_err();
        assert!(matches!(&err, HelperError::LockLost(_)), "{err:?}");
        assert_eq!(
            std::fs::read_to_string(m.lock().join("owner")).unwrap(),
            thief
        );
        assert!(m.temporaries().is_empty());
        assert!(!m.bin_dir().join("2.0.0/pitcrewd").exists());
        assert_eq!(m.link("current").as_deref(), Some("1.0.0"));
    }

    /// The helper runs with the umask of the user's session, not the script's 077, so files
    /// agents make in shared project directories keep their usual permissions.
    fn the_helper_gets_the_users_umask() {
        let m = Machine::new();
        let fake = m.fake(Remote {
            umask: Some("027".to_owned()),
            ..Remote::default()
        });
        let target = m.target(&fake);
        block_on(deploy(&target, &helper("1.0.0"), &quick())).unwrap();
        let launcher = DirectLauncher::new(launch_options());
        let started = block_on(launcher.start(&target)).unwrap();
        // Read from outside where /proc allows (Linux); everywhere, the helper's own report
        // below says it too.
        if let Some(mask) = umask_of(started.endpoint.pid) {
            assert_eq!(mask, "0027");
        }
        // What the script made stays private all the same.
        assert_private(&m.root());
        block_on(launcher.stop(&target)).unwrap();
        let log = std::fs::read_to_string(m.run_dir().join("pitcrewd.log")).unwrap();
        assert!(log.contains("fake pitcrewd umask 0027\n"), "{log}");
    }

    /// The tool path comes before the user's `PATH`, and functions bash imports from the
    /// environment do not stand in for tools.
    fn look_alike_tools_are_not_used() {
        let m = Machine::new();
        let evil = m.dir.path().join("evil");
        std::fs::create_dir(&evil).unwrap();
        shim(&evil, "ls", "echo 'drwx------ 1 0 0 0 Jan 1 00:00 x'");
        shim(&evil, "mkdir", "exit 1");
        shim(
            &evil,
            "sha256sum",
            "echo 0000000000000000000000000000000000000000000000000000000000000000",
        );
        let fake = m.fake(Remote {
            path: format!("{}:{}", evil.display(), m.bin.display()),
            ..Remote::default()
        });
        let done = block_on(deploy(&m.target(&fake), &helper("1.0.0"), &quick())).unwrap();
        assert!(done.uploaded);

        let Some(bash) = which("bash") else {
            println!("note: no bash here for the exported-function case");
            return;
        };
        let functions = ["dd", "mkdir", "ls", "mv", "sha256sum", "cat", "kill"].map(|f| {
            (
                format!("BASH_FUNC_{f}%%"),
                "() { echo hijacked >&2; return 1; }".to_owned(),
            )
        });
        let fake = m.fake(Remote {
            interpreter: Some(bash.to_str().unwrap().to_owned()),
            env: functions.to_vec(),
            ..Remote::default()
        });
        let target = m.target(&fake);
        let done = block_on(deploy(&target, &helper("2.0.0"), &quick())).unwrap();
        assert!(done.uploaded);
        let launcher = DirectLauncher::new(launch_options());
        block_on(launcher.start(&target)).unwrap();
        let stopped = block_on(launcher.stop(&target)).unwrap();
        assert!(stopped.pid.is_some());
    }

    /// macOS shows `@` for extended attributes, which hides an ACL's `+`: `ls -le` is asked.
    /// A set-group-ID parent (group project directories) makes new directories `drwx--S---`,
    /// which are private all the same.
    fn acls_behind_an_at_sign_and_setgid_parents() {
        let real = which("ls").unwrap();
        for acl in [true, false] {
            let m = Machine::new();
            let root = m.root();
            let entry = if acl {
                "echo ' 0: group:everyone allow list,add_file'"
            } else {
                ":"
            };
            shim(
                &m.bin,
                "ls",
                &format!(
                    "last=\nfor a in \"$@\"; do last=$a; done\n\
                     case \"$1:$last\" in\n\
                     '-ldn:{root}') '{real}' -ldn \"$last\" | awk '{{ $1 = $1 \"@\"; print }}' ;;\n\
                     '-lde:{root}') echo \"drwx------@ 2 501 20 64 Oct  1 12:00 $last\"; {entry} ;;\n\
                     *) exec '{real}' \"$@\" ;;\n\
                     esac",
                    root = root.display(),
                    real = real.display(),
                ),
            );
            let result = block_on(deploy(&m.plain(), &helper("1.0.0"), &quick()));
            if acl {
                let err = result.unwrap_err();
                assert!(
                    matches!(&err, HelperError::UnsafeDirectory(d) if d.contains("access control list")),
                    "{err:?}"
                );
            } else {
                result.unwrap();
            }
        }

        let m = Machine::new();
        std::fs::set_permissions(&m.home, std::fs::Permissions::from_mode(0o2755)).unwrap();
        // Linux gives a new directory its parent's set-group-ID bit; BSD and macOS do not (the
        // group is always the parent's there). Whichever this system does is accepted.
        let probe = m.home.join("probe");
        std::fs::create_dir(&probe).unwrap();
        let private = if mode(&probe) & 0o2000 != 0 {
            0o2700
        } else {
            0o700
        };
        std::fs::remove_dir(&probe).unwrap();
        block_on(deploy(&m.plain(), &helper("1.0.0"), &quick())).unwrap();
        assert_eq!(mode(&m.root()), private);
        assert_eq!(mode(&m.bin_dir()), private);
        let launcher = DirectLauncher::new(launch_options());
        block_on(launcher.start(&m.plain())).unwrap();
        assert_eq!(mode(&m.run_dir()), private);
        assert!(block_on(launcher.stop(&m.plain())).unwrap().pid.is_some());
    }

    /// A host name with other characters is kept readable (`odd_host_`), with the machine's
    /// id after a `+`, rather than all such hosts being `unknown`.
    fn odd_host_names_get_a_machine_id() {
        let real = which("uname").unwrap();
        let m = Machine::with_tools(|bin| {
            shim(
                bin,
                "uname",
                &format!(
                    "case \"$1\" in -n) echo 'odd host!' ;; *) exec '{}' \"$@\" ;; esac",
                    real.display()
                ),
            );
        });
        let target = m.plain();
        block_on(deploy(&target, &helper("1.0.0"), &quick())).unwrap();
        let launcher = DirectLauncher::new(launch_options());
        let host = block_on(launcher.start(&target)).unwrap().endpoint.host;
        assert!(host.starts_with("odd_host_"), "{host}");
        assert_eq!(host.split('+').next(), Some("odd_host_"), "{host}");
        assert!(
            host.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-+".contains(&b))
        );
        // The same name on every call: the helper it started is recognised as running here.
        let status = block_on(launcher.status(&target)).unwrap();
        assert_eq!(status.state, HelperState::Running);
        assert_eq!(status.endpoint.unwrap().host, host);
        assert!(block_on(launcher.stop(&target)).unwrap().pid.is_some());
    }

    /// `Ssh::run_with_input` is binary-clean, reports progress, and still honours the output
    /// limit while the remote side reads nothing.
    fn input_round_trips_and_is_bounded() {
        let m = Machine::new();
        let fake = m.fake(Remote::default());
        let bytes: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        let seen = Mutex::new(Vec::new());
        let progress = |sent: u64| seen.lock().unwrap().push(sent);
        let input = Input::new(&bytes[..1000])
            .then(&bytes[1000..])
            .with_progress(&progress);
        assert_eq!(input.len(), bytes.len() as u64);
        let out = block_on(
            fake.ssh
                .run_with_input("cluster", &["cat"], input, Limits::default()),
        )
        .unwrap();
        assert!(out.success());
        assert_eq!(out.stdout, bytes);
        let seen = seen.into_inner().unwrap();
        assert!(seen.windows(2).all(|w| w[0] < w[1]));
        assert_eq!(seen.last().copied(), Some(bytes.len() as u64));

        // A command that floods its output and never reads: the limit ends the call even
        // though the writer is blocked.
        let start = Instant::now();
        let limits = Limits {
            max_output: Some(100_000),
            timeout: Some(Duration::from_secs(60)),
        };
        let err = block_on(fake.ssh.run_with_input(
            "cluster",
            &["dd", "if=/dev/zero", "bs=65536", "count=1000"],
            Input::new(&bytes),
            limits,
        ))
        .unwrap_err();
        assert!(
            matches!(err, SshError::OutputTooLarge { limit: 100_000 }),
            "{err:?}"
        );
        assert!(start.elapsed() < Duration::from_secs(20));
    }

    // ─── Launchers ─────────────────────────────────────────────────────────────────────────

    /// Deploys, then starts, re-starts, checks and stops the helper with `launcher`.
    fn round_trip(launcher: &dyn Launcher, m: &Machine) {
        let target = m.plain();
        block_on(deploy(&target, &helper("1.0.0"), &quick())).unwrap();
        let tmux = launcher.name() == "tmux";

        let before = now_ms();
        let started = block_on(launcher.start(&target)).unwrap();
        assert!(started.started_now);
        let e = &started.endpoint;
        assert_eq!(e.version, "1.0.0");
        assert_eq!(e.launcher, launcher.name());
        assert_eq!(e.host, this_host());
        assert_eq!(e.socket, m.layout().socket());
        assert!(e.started >= before - 2000 && e.started <= now_ms() + 1000);
        assert!(alive(e.pid));
        assert_eq!(comm(e.pid), "pitcrewd");

        // endpoint.json, one line, private like everything else; the socket is there.
        let text = std::fs::read_to_string(m.run_dir().join("endpoint.json")).unwrap();
        assert_eq!(text.lines().count(), 1);
        assert_eq!(&serde_json::from_str::<Endpoint>(&text).unwrap(), e);
        assert_eq!(mode(&m.run_dir().join("endpoint.json")), 0o600);
        let socket = std::fs::symlink_metadata(&e.socket).unwrap();
        assert!(socket.file_type().is_socket());
        assert_private(&m.root());
        assert!(!m.run_dir().join(".lock").exists());

        // Starting again finds it running.
        let again = block_on(launcher.start(&target)).unwrap();
        assert!(!again.started_now);
        assert_eq!(&again.endpoint, e);

        let status = block_on(launcher.status(&target)).unwrap();
        assert_eq!(status.state, HelperState::Running);
        assert!(status.running());
        assert_eq!(status.endpoint.as_ref(), Some(e));
        assert_eq!(status.installed.as_deref(), Some("1.0.0"));
        assert_eq!(status.running_version(), Some("1.0.0"));
        assert!(status.socket_ready);
        assert_eq!(status.tmux_session, tmux.then_some(true));

        let stopped = block_on(launcher.stop(&target)).unwrap();
        assert_eq!(
            stopped,
            Stopped {
                pid: Some(e.pid),
                forced: false
            }
        );
        eventually("the helper to be gone", || !alive(e.pid));
        for gone in ["endpoint.json", "pitcrewd.pid", "pitcrewd.sock"] {
            assert!(!m.run_dir().join(gone).exists(), "{gone}");
        }
        // The log: the version's own binary was started, not `current`, with the umask the
        // user's session had rather than the script's 077. It stays private itself.
        let log = std::fs::read_to_string(m.run_dir().join("pitcrewd.log")).unwrap();
        assert!(
            log.contains("pitcrewd script: bin/1.0.0/pitcrewd\n"),
            "{log}"
        );
        assert!(
            log.contains(&format!("fake pitcrewd umask {}\n", own_umask())),
            "{log}"
        );
        assert_eq!(mode(&m.run_dir().join("pitcrewd.log")), 0o600);

        let status = block_on(launcher.status(&target)).unwrap();
        assert_eq!(status.state, HelperState::NotRunning);
        assert_eq!(status.endpoint, None);
        assert!(!status.socket_ready);
        assert_eq!(status.tmux_session, tmux.then_some(false));
        assert_eq!(status.installed.as_deref(), Some("1.0.0"));

        // Stopping again does nothing.
        let again = block_on(launcher.stop(&target)).unwrap();
        assert_eq!(
            again,
            Stopped {
                pid: None,
                forced: false
            }
        );
    }

    fn direct_launcher_starts_reports_and_stops() {
        round_trip(&DirectLauncher::new(launch_options()), &Machine::new());
    }

    fn local_tmux() -> Option<String> {
        let out = Command::new("tmux").arg("-V").output().ok()?;
        let text = String::from_utf8(out.stdout).ok()?;
        Some(text.trim().strip_prefix("tmux ")?.to_owned())
    }

    fn tmux_launcher_starts_reports_and_stops() {
        let Some(version) = local_tmux().filter(|v| parse_tmux_version(v) >= Some(MIN_TMUX)) else {
            println!("skipped: no tmux 3.2 or newer here");
            return;
        };
        let mut m = Machine::new();
        // A private tmux socket directory, so this server is the test's alone.
        let sockets = m.dir.path().join("tmux");
        private_dir(&sockets);
        m.env.push((
            "TMUX_TMPDIR".to_owned(),
            sockets.to_str().unwrap().to_owned(),
        ));
        let launcher = TmuxLauncher::new(Some(&version), launch_options()).unwrap();
        round_trip(&launcher, &m);
        // The session and its server are gone.
        let has = Command::new("tmux")
            .args(["-L", &tmux_name(&m.layout()), "has-session"])
            .env("TMUX_TMPDIR", &sockets)
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(!has.success());

        // Two roots on one host, with one tmux socket directory, do not collide.
        let other = Layout::at(m.dir.path().join("other").to_str().unwrap()).unwrap();
        assert_ne!(tmux_name(&other), tmux_name(&m.layout()));
        let here = m.plain();
        let there = m.target_at(&m.fake(Remote::default()), other);
        for target in [&here, &there] {
            block_on(deploy(target, &helper("1.0.0"), &quick())).unwrap();
        }
        let a = block_on(launcher.start(&here)).unwrap().endpoint;
        let b = block_on(launcher.start(&there)).unwrap().endpoint;
        assert_ne!(a.pid, b.pid);
        assert_ne!(a.socket, b.socket);
        block_on(launcher.stop(&here)).unwrap();
        let status = block_on(launcher.status(&there)).unwrap();
        assert_eq!(status.state, HelperState::Running);
        assert_eq!(status.tmux_session, Some(true));
        block_on(launcher.stop(&there)).unwrap();

        // A root whose path holds a #: tmux reads one in -c as the start of a format (here
        // #{pane_id}), so the helper would start elsewhere, and refuse to.
        let parent = m.dir.path().join("x#{pane_id}#y");
        std::fs::create_dir(&parent).unwrap();
        let hashed = Layout::at(parent.join(".pitcrew").to_str().unwrap()).unwrap();
        let target = m.target_at(&m.fake(Remote::default()), hashed.clone());
        block_on(deploy(&target, &helper("1.0.0"), &quick())).unwrap();
        let e = block_on(launcher.start(&target)).unwrap().endpoint;
        assert_eq!(e.socket, hashed.socket());
        assert!(alive(e.pid));
        let status = block_on(launcher.status(&target)).unwrap();
        assert_eq!(status.state, HelperState::Running);
        block_on(launcher.stop(&target)).unwrap();
        println!("tmux checked: {version}");
    }

    fn write_endpoint(m: &Machine, endpoint: &Endpoint) {
        private_dir(&m.run_dir());
        std::fs::write(
            m.run_dir().join("endpoint.json"),
            format!("{}\n", serde_json::to_string(endpoint).unwrap()),
        )
        .unwrap();
    }

    fn launcher_failures_are_clear() {
        let options = LaunchOptions {
            ready_timeout: Duration::from_secs(2),
            stop_timeout: Duration::from_secs(1),
            ..launch_options()
        };
        let launcher = DirectLauncher::new(options.clone());

        // Nothing deployed.
        let m = Machine::new();
        let err = block_on(launcher.start(&m.plain())).unwrap_err();
        assert!(matches!(err, HelperError::NotDeployed(_)), "{err:?}");

        // It exits at once: its log says why.
        let failing = helper_from("1.0.0", helper_script("1.0.0", "exit", 0));
        block_on(deploy(&m.plain(), &failing, &quick())).unwrap();
        let err = block_on(launcher.start(&m.plain())).unwrap_err();
        assert!(
            matches!(&err, HelperError::StartFailed(d) if d.contains("exited at once") && d.contains("fake failure")),
            "{err:?}"
        );
        assert!(!m.run_dir().join("endpoint.json").exists());

        // It never opens its socket: it is stopped after the wait.
        let mute = helper_from("2.0.0", helper_script("2.0.0", "nosocket", 0));
        block_on(deploy(&m.plain(), &mute, &quick())).unwrap();
        let err = block_on(launcher.start(&m.plain())).unwrap_err();
        assert!(
            matches!(&err, HelperError::StartFailed(d) if d.contains("no socket")),
            "{err:?}"
        );
        let pid: u32 = std::fs::read_to_string(m.run_dir().join("pitcrewd.pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        eventually("the mute helper to be stopped", || !alive(pid));
        assert!(!m.run_dir().join("endpoint.json").exists());

        // It ignores SIGTERM: stop kills it.
        let stubborn = helper_from("3.0.0", helper_script("3.0.0", "stubborn", 0));
        block_on(deploy(&m.plain(), &stubborn, &quick())).unwrap();
        let started = block_on(launcher.start(&m.plain())).unwrap();
        let pid = started.endpoint.pid;
        let stopped = block_on(launcher.stop(&m.plain())).unwrap();
        assert_eq!(
            stopped,
            Stopped {
                pid: Some(pid),
                forced: true
            }
        );
        eventually("the stubborn helper to be gone", || !alive(pid));

        // A record from another host sharing this home is not touched...
        block_on(deploy(&m.plain(), &helper("4.0.0"), &quick())).unwrap();
        let foreign = Endpoint {
            pid: 1,
            host: "elsewhere".to_owned(),
            version: "4.0.0".to_owned(),
            started: 1_790_000_000_000,
            launcher: "direct".to_owned(),
            socket: m.layout().socket(),
            job: None,
        };
        write_endpoint(&m, &foreign);
        let status = block_on(launcher.status(&m.plain())).unwrap();
        assert_eq!(status.state, HelperState::OtherHost("elsewhere".to_owned()));
        assert_eq!(status.endpoint.as_ref(), Some(&foreign));
        let err = block_on(launcher.start(&m.plain())).unwrap_err();
        assert!(
            matches!(&err, HelperError::OtherHost { host, launcher } if host == "elsewhere" && launcher == "direct"),
            "{err:?}"
        );
        let err = block_on(launcher.stop(&m.plain())).unwrap_err();
        assert!(matches!(err, HelperError::OtherHost { .. }), "{err:?}");
        assert!(m.run_dir().join("endpoint.json").exists());

        // ...unless the user says to take over.
        let taking = DirectLauncher::new(LaunchOptions {
            take_over: true,
            ..options.clone()
        });
        let started = block_on(taking.start(&m.plain())).unwrap();
        assert!(started.started_now);
        assert_eq!(started.endpoint.host, this_host());
        block_on(taking.stop(&m.plain())).unwrap();

        // A record of a process of this host that is gone is stale.
        write_endpoint(
            &m,
            &Endpoint {
                pid: dead_pid(),
                host: this_host(),
                ..foreign.clone()
            },
        );
        let status = block_on(launcher.status(&m.plain())).unwrap();
        assert_eq!(status.state, HelperState::NotRunning);
        assert!(status.endpoint.is_some());
        let started = block_on(launcher.start(&m.plain())).unwrap();
        assert!(started.started_now);
        block_on(launcher.stop(&m.plain())).unwrap();

        // This host's records made before its id changed (a stateless node makes a new
        // machine id at every boot) are still this host's, judged by their pid: a dead one is
        // stale, a live helper is running here and is stopped.
        let reborn = format!("{}+0123456789ab", this_name());
        write_endpoint(
            &m,
            &Endpoint {
                pid: dead_pid(),
                host: reborn.clone(),
                ..foreign.clone()
            },
        );
        let status = block_on(launcher.status(&m.plain())).unwrap();
        assert_eq!(status.state, HelperState::NotRunning);
        let started = block_on(launcher.start(&m.plain())).unwrap();
        assert!(started.started_now);
        let mut record = started.endpoint.clone();
        record.host.clone_from(&reborn);
        write_endpoint(&m, &record);
        let status = block_on(launcher.status(&m.plain())).unwrap();
        assert_eq!(status.state, HelperState::Running);
        let again = block_on(launcher.start(&m.plain())).unwrap();
        assert!(!again.started_now);
        let stopped = block_on(launcher.stop(&m.plain())).unwrap();
        assert_eq!(stopped.pid, Some(record.pid));
        eventually("the helper to be gone", || !alive(record.pid));

        // A recycled pid that is not pitcrewd is never signalled.
        let mut sleeper = Command::new("sleep")
            .arg("30")
            .env(RUN_ENV, run_mark())
            .spawn()
            .unwrap();
        write_endpoint(
            &m,
            &Endpoint {
                pid: sleeper.id(),
                host: this_host(),
                ..foreign
            },
        );
        let status = block_on(launcher.status(&m.plain())).unwrap();
        assert_eq!(status.state, HelperState::NotRunning);
        let stopped = block_on(launcher.stop(&m.plain())).unwrap();
        assert_eq!(stopped.pid, None);
        assert!(
            sleeper.try_wait().unwrap().is_none(),
            "the sleeper was killed"
        );
        sleeper.kill().unwrap();
        sleeper.wait().unwrap();

        // A socket path too long for the platform is refused before any call.
        let deep = Machine::new();
        let long = format!("{}/{}", deep.home.display(), "d".repeat(90));
        let fake = deep.fake(Remote::default());
        let target = Target::with_layout(
            fake.ssh.clone(),
            "cluster",
            Layout::at(&long).unwrap(),
            Platform::LinuxX86_64,
        )
        .unwrap();
        let err = block_on(launcher.start(&target)).unwrap_err();
        assert!(matches!(err, HelperError::InvalidArgument(_)), "{err:?}");
        assert!(fake.calls().is_empty());
    }

    // ─── Every sh ──────────────────────────────────────────────────────────────────────────

    pub(crate) fn posix_shells() -> Vec<PathBuf> {
        let listed = std::env::var_os("PITCREW_TEST_SHELLS").filter(|v| !v.is_empty());
        let shells: Vec<PathBuf> = match listed {
            Some(list) => std::env::split_paths(&list).collect(),
            None => ["sh", "bash", "dash", "zsh", "ksh", "mksh"]
                .iter()
                .filter_map(|name| which(name))
                .collect(),
        };
        shells
            .into_iter()
            .filter(|s| {
                let name = s.file_name().unwrap().to_str().unwrap();
                !matches!(name, "fish" | "tcsh" | "csh")
            })
            .collect()
    }

    /// The whole flow with each POSIX shell as the machine's `sh`.
    fn everything_under_every_posix_sh() {
        let mut checked = Vec::new();
        for shell in posix_shells() {
            assert!(shell.is_file(), "{} is missing", shell.display());
            let m = Machine::with_shell(&shell);
            for v in ["1.0.0", "2.0.0", "3.0.0"] {
                let done = block_on(deploy(&m.plain(), &helper(v), &quick()))
                    .unwrap_or_else(|e| panic!("{}: {e}", shell.display()));
                assert!(done.uploaded);
            }
            let again = block_on(deploy(&m.plain(), &helper("3.0.0"), &quick())).unwrap();
            assert!(!again.uploaded, "{}", shell.display());
            assert_eq!(m.versions(), ["2.0.0", "3.0.0"], "{}", shell.display());
            round_trip(&DirectLauncher::new(launch_options()), &m);
            checked.push(shell.display().to_string());
        }
        println!("deployed and launched with sh = {checked:?}");
    }

    // ─── Leftover processes ────────────────────────────────────────────────────────────────

    /// Every process a case starts on a fake machine carries this variable, set to
    /// [`run_mark`]: the fake `ssh` puts it in the remote command's environment, and helpers,
    /// tmux servers and fake SLURM jobs inherit it. Commands a case starts itself get it too.
    pub(crate) const RUN_ENV: &str = "PITCREW_TEST_RUN";

    /// This run's mark: unique to this test process.
    pub(crate) fn run_mark() -> &'static str {
        static MARK: std::sync::OnceLock<String> = std::sync::OnceLock::new();
        MARK.get_or_init(|| {
            let nanos = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            format!("deploy-{}-{nanos}", std::process::id())
        })
    }

    /// The live processes carrying this run's mark, with their command lines. Linux only (it
    /// reads `/proc`); empty elsewhere. A zombie has no environment, so it does not count.
    fn marked() -> Vec<(u32, String)> {
        let want = format!("{RUN_ENV}={}", run_mark());
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
                let cmd = String::from_utf8_lossy(&cmd).replace('\0', " ");
                Some((pid, cmd.trim().to_owned()))
            })
            .collect()
    }

    /// Waits up to `grace` for this run's processes to end, then kills those left (SIGKILL)
    /// and returns them.
    fn sweep(grace: Duration) -> Vec<(u32, String)> {
        let start = Instant::now();
        loop {
            let left = marked();
            if left.is_empty() || start.elapsed() >= grace {
                for (pid, _) in &left {
                    if let Some(pid) = i32::try_from(*pid)
                        .ok()
                        .and_then(rustix::process::Pid::from_raw)
                    {
                        let _ = rustix::process::kill_process(pid, rustix::process::Signal::KILL);
                    }
                }
                return left;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// Kills this run's processes when dropped: at the end of the run, or if it unwinds.
    struct Sweeper;

    impl Drop for Sweeper {
        fn drop(&mut self) {
            sweep(Duration::ZERO);
        }
    }

    // ─── Runner ────────────────────────────────────────────────────────────────────────────

    fn run_tests() -> ExitCode {
        let cases: &[(&str, fn())] = &[
            (
                "full_deploy_then_idempotent_rerun",
                full_deploy_then_idempotent_rerun,
            ),
            ("progress_is_reported", progress_is_reported),
            (
                "a_hash_mismatch_removes_the_upload",
                a_hash_mismatch_removes_the_upload,
            ),
            (
                "an_interrupted_upload_leaves_nothing_in_place",
                an_interrupted_upload_leaves_nothing_in_place,
            ),
            (
                "concurrent_deploys_take_turns",
                concurrent_deploys_take_turns,
            ),
            (
                "stale_locks_are_broken_and_live_ones_waited_for",
                stale_locks_are_broken_and_live_ones_waited_for,
            ),
            (
                "gc_keeps_exactly_two_versions",
                gc_keeps_exactly_two_versions,
            ),
            (
                "a_damaged_install_is_replaced",
                a_damaged_install_is_replaced,
            ),
            ("every_sha256_tool_works", every_sha256_tool_works),
            (
                "no_sha256_tool_is_refused_before_uploading",
                no_sha256_tool_is_refused_before_uploading,
            ),
            (
                "unknown_platforms_are_refused",
                unknown_platforms_are_refused,
            ),
            ("bad_helpers_are_removed", bad_helpers_are_removed),
            (
                "the_upload_is_never_readable_by_others",
                the_upload_is_never_readable_by_others,
            ),
            (
                "unsafe_directories_are_refused",
                unsafe_directories_are_refused,
            ),
            ("mv_without_t_falls_back", mv_without_t_falls_back),
            ("a_stalled_upload_times_out", a_stalled_upload_times_out),
            (
                "the_way_to_the_root_is_checked",
                the_way_to_the_root_is_checked,
            ),
            (
                "an_acl_on_the_way_is_judged_by_what_it_grants",
                an_acl_on_the_way_is_judged_by_what_it_grants,
            ),
            (
                "a_macos_acl_on_the_way_is_judged_by_what_it_grants",
                a_macos_acl_on_the_way_is_judged_by_what_it_grants,
            ),
            (
                "a_partly_eaten_script_never_runs",
                a_partly_eaten_script_never_runs,
            ),
            ("a_lost_lock_stops_the_deploy", a_lost_lock_stops_the_deploy),
            (
                "the_helper_gets_the_users_umask",
                the_helper_gets_the_users_umask,
            ),
            (
                "look_alike_tools_are_not_used",
                look_alike_tools_are_not_used,
            ),
            (
                "acls_behind_an_at_sign_and_setgid_parents",
                acls_behind_an_at_sign_and_setgid_parents,
            ),
            (
                "odd_host_names_get_a_machine_id",
                odd_host_names_get_a_machine_id,
            ),
            (
                "input_round_trips_and_is_bounded",
                input_round_trips_and_is_bounded,
            ),
            (
                "direct_launcher_starts_reports_and_stops",
                direct_launcher_starts_reports_and_stops,
            ),
            (
                "tmux_launcher_starts_reports_and_stops",
                tmux_launcher_starts_reports_and_stops,
            ),
            ("launcher_failures_are_clear", launcher_failures_are_clear),
            (
                "everything_under_every_posix_sh",
                everything_under_every_posix_sh,
            ),
        ];
        let cases: Vec<(&str, fn())> = cases
            .iter()
            .chain(slurm::CASES)
            .chain(crate::tunnel::CASES)
            .copied()
            .collect();
        let filters: Vec<String> = std::env::args()
            .skip(1)
            .filter(|a| !a.starts_with('-'))
            .collect();
        let list_only = std::env::args().any(|a| a == "--list");
        let mut failed = Vec::new();
        let mut ran = 0;
        // Kills whatever this run started, however the run ends.
        let sweeper = Sweeper;
        for (name, case) in &cases {
            if !filters.is_empty() && !filters.iter().any(|f| name.contains(f.as_str())) {
                continue;
            }
            if list_only {
                println!("{name}: test");
                continue;
            }
            ran += 1;
            let start = Instant::now();
            let mut ok = std::panic::catch_unwind(case).is_ok();
            // A case passes only if it leaves nothing running, and leaves nothing either way.
            let left = sweep(Duration::from_secs(5));
            if !left.is_empty() {
                println!("{name} left these running (now killed): {left:?}");
                ok = false;
            }
            println!(
                "test {name} ... {} ({:.1}s)",
                if ok { "ok" } else { "FAILED" },
                start.elapsed().as_secs_f64()
            );
            if !ok {
                failed.push(*name);
            }
        }
        drop(sweeper);
        drop_daemon();
        if list_only {
            return ExitCode::SUCCESS;
        }
        // Nothing this run started may still run.
        std::thread::sleep(Duration::from_millis(200));
        let left = marked();
        if cfg!(target_os = "linux") {
            println!("processes this run left behind: {left:?}");
        }
        if !left.is_empty() {
            failed.push("(processes left behind)");
        }
        println!(
            "\ntest result: {}. {} passed; {} failed",
            if failed.is_empty() { "ok" } else { "FAILED" },
            ran - failed.len(),
            failed.len()
        );
        if failed.is_empty() {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        }
    }
}
