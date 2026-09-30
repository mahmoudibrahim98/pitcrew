//! Running commands on a host with the user's own OpenSSH.
//!
//! **The command** travels in a shell-neutral wrapper (see [`crate::quote`]), so it means the
//! same under any login shell.
//!
//! **Options.** Every call passes `-T`, agent and X11 forwarding off, `PermitLocalCommand=no`,
//! `ClearAllForwardings=yes`, `RemoteCommand=none`, keep-alives, `StrictHostKeyChecking=ask` and
//! `ConnectTimeout`. Two limits to know:
//! - `ClearAllForwardings=yes` also clears `-L`, `-R` and `-D`, so tunnels (a later brief) need
//!   their own option set.
//! - `-o` options reach the target host only, not `ProxyJump` hops: each hop is a separate ssh
//!   that reads only the user's config. Prompts from hops still reach the askpass bridge, since
//!   they inherit its environment.
//!
//! **Connection reuse.** On Unix every call passes `ControlMaster=auto` with a `ControlPath` in
//! a private 0700 directory and `ControlPersist=10m`, so the first call authenticates and later
//! ones reuse its connection. Windows OpenSSH has no ControlMaster: each call connects and
//! authenticates anew (with keys that is quick; with passwords or one-time codes the user is
//! asked each time). A persistent channel on Windows comes with the tunnel work.
//!
//! **Prompts** need OpenSSH 8.4 or newer on this computer (for `SSH_ASKPASS_REQUIRE=force`).
//! Without a [`PromptHandler`], calls run with `BatchMode=yes` and without any `SSH_ASKPASS`
//! from the environment, so they fail rather than prompt.
//!
//! **Stopping.** A cancelled prompt, a [`Limits`] breach, or dropping the call's future stops
//! ssh. On Unix ssh runs in its own process group, and the whole group is killed: ssh, its
//! askpass programs and any `ProxyJump` ssh, before any of them can send an empty credential.
//! On Windows only ssh itself is killed.
//!
//! **Errors.** ssh's own messages go to a log file (`-E`) in the private runtime directory, apart
//! from the remote command's stderr. Exit 255 is ssh's failure code, and it is mapped to an
//! [`SshError`] by what ssh logged, never by what the remote command printed. When ssh logged
//! nothing, the 255 is the remote command's own and comes back as an [`Output`]. (With
//! `LogLevel QUIET` in the user's config, ssh's own failures look like that too.)

use crate::askpass::PromptHandler;
use crate::askpass::server::AskpassServer;
use crate::quote::{remote_command, validate_host};
use std::fmt;
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt as _};
use tokio::sync::watch;

/// Default for `ConnectTimeout`.
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// How much of ssh's log is read to explain a failure.
const MAX_LOG: u64 = 64 * 1024;

/// Why an ssh call failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SshError {
    /// The host name was refused before running ssh.
    #[error("invalid host name {0}")]
    InvalidHost(String),
    /// The command, or a setting, cannot be passed safely.
    #[error("invalid argument: {0}")]
    InvalidArgument(String),
    /// ssh could not be started.
    #[error("could not start ssh: {0}")]
    Spawn(#[source] io::Error),
    /// The askpass listener or runtime directory could not be set up.
    #[error("could not set up the prompt bridge: {0}")]
    Setup(#[source] io::Error),
    /// Reading ssh's output failed.
    #[error("could not read from ssh: {0}")]
    Io(#[source] io::Error),
    /// The user cancelled a prompt. ssh was stopped before it could answer anything.
    #[error("cancelled")]
    Cancelled,
    /// The call ran longer than its [`Limits::timeout`]; ssh was stopped.
    #[error("no result within {0:?}")]
    TimedOut(Duration),
    /// The command printed more than [`Limits::max_output`]; ssh was stopped.
    #[error("more than {limit} bytes of output")]
    OutputTooLarge {
        /// The limit.
        limit: usize,
    },
    /// The connection attempt timed out.
    #[error("the connection timed out")]
    ConnectTimeout {
        /// What ssh printed.
        stderr: String,
    },
    /// The host name did not resolve, or the host refused or could not be routed to.
    #[error("the host could not be reached")]
    Unreachable {
        /// What ssh printed.
        stderr: String,
    },
    /// The host key was not accepted (unknown and declined, or no way to ask).
    #[error("the host key was not accepted")]
    HostKeyRejected {
        /// What ssh printed.
        stderr: String,
    },
    /// The host key differs from the one in `known_hosts`. ssh refuses; only the user can fix
    /// this, after checking why.
    #[error("the host key has changed")]
    HostKeyChanged {
        /// What ssh printed.
        stderr: String,
    },
    /// Every authentication method failed.
    #[error("authentication failed")]
    AuthFailed {
        /// What ssh printed.
        stderr: String,
    },
    /// Any other failure that ssh logged with exit code 255, or a failure of `ssh -G`.
    #[error("ssh failed with exit code {code}: {}", last_line(stderr))]
    Ssh {
        /// Exit code.
        code: i32,
        /// What ssh printed.
        stderr: String,
    },
    /// The remote command ran but printed something unexpected.
    #[error("unexpected output: {0}")]
    UnexpectedOutput(String),
}

/// The last non-empty line, with control characters (e.g. terminal escapes a server sent)
/// replaced, since error messages end up on screens and in logs.
pub(crate) fn last_line(text: &str) -> String {
    text.lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .chars()
        .map(|c| if c.is_control() { '?' } else { c })
        .collect()
}

/// The result of a remote command that ran.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Output {
    /// Exit code; `None` if it was killed by a signal.
    pub code: Option<i32>,
    /// Standard output.
    pub stdout: Vec<u8>,
    /// The remote command's standard error. ssh's own messages are logged apart and only
    /// explain failures; a `ProxyJump` hop's messages do land here.
    pub stderr: Vec<u8>,
}

impl Output {
    /// Whether the command exited 0.
    #[must_use]
    pub fn success(&self) -> bool {
        self.code == Some(0)
    }

    /// Standard output as text, with invalid UTF-8 replaced.
    #[must_use]
    pub fn stdout_text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }
}

/// Bounds for [`Ssh::run_limited`]. The default is no bounds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Limits {
    /// The most bytes kept of stdout, and of stderr. ssh is stopped when either has more.
    pub max_output: Option<usize>,
    /// How long the call may run. The clock stops while a prompt waits for the user, and
    /// starts again from zero once it is answered.
    pub timeout: Option<Duration>,
}

/// What `ssh -G` says a host resolves to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedHost {
    /// The real host name to connect to.
    pub hostname: String,
    /// The remote user, if ssh knows it.
    pub user: Option<String>,
    /// The port.
    pub port: u16,
    /// Jump hosts, as written in the config (`ProxyJump`).
    pub proxy_jump: Option<String>,
    /// A `ProxyCommand`, if set.
    pub proxy_command: Option<String>,
}

#[derive(Clone)]
struct Prompts {
    program: PathBuf,
    handler: Arc<dyn PromptHandler>,
}

/// The ssh program and how to call it. Cheap to clone.
#[derive(Clone)]
pub struct Ssh {
    program: PathBuf,
    prompts: Option<Prompts>,
    /// `None`: the first usable of [`crate::private::default_runtime_dirs`].
    runtime_dir: Option<PathBuf>,
    connect_timeout: Duration,
    multiplex: bool,
}

impl fmt::Debug for Ssh {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Ssh")
            .field("program", &self.program)
            .field("askpass", &self.prompts.as_ref().map(|p| &p.program))
            .field("runtime_dir", &self.runtime_dir)
            .field("connect_timeout", &self.connect_timeout)
            .field("multiplex", &self.multiplex)
            .finish()
    }
}

impl Default for Ssh {
    fn default() -> Self {
        Self::new("ssh")
    }
}

impl Ssh {
    /// Uses `program` as ssh (a name on `PATH`, or a path). Connection reuse is on for Unix.
    #[must_use]
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            prompts: None,
            runtime_dir: None,
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            multiplex: cfg!(unix),
        }
    }

    /// Sends prompts to `handler` through the askpass program at `askpass`, the
    /// `pitcrew-askpass` binary. It must be an absolute path, or calls fail with
    /// [`SshError::InvalidArgument`]: ssh would otherwise look it up on `PATH`.
    #[must_use]
    pub fn with_prompts(
        mut self,
        askpass: impl Into<PathBuf>,
        handler: Arc<dyn PromptHandler>,
    ) -> Self {
        self.prompts = Some(Prompts {
            program: askpass.into(),
            handler,
        });
        self
    }

    /// Where control sockets, askpass sockets and ssh's logs go, instead of the defaults
    /// (`$XDG_RUNTIME_DIR/pitcrew-ssh`, `/tmp/pitcrew-ssh-<uid>`, then `~/.pitcrew/s` on Unix;
    /// `%TEMP%\pitcrew-ssh` on Windows). It is created 0700 if missing, and refused if it exists
    /// and is not private, or if its name has `%`, `$`, quotes or control characters.
    #[must_use]
    pub fn with_runtime_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.runtime_dir = Some(dir.into());
        self
    }

    /// Sets `ConnectTimeout` (rounded up to whole seconds, at least 1).
    #[must_use]
    pub fn with_connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = timeout;
        self
    }

    /// Turns connection reuse on or off. Windows OpenSSH does not support it.
    #[must_use]
    pub fn with_multiplex(mut self, on: bool) -> Self {
        self.multiplex = on;
        self
    }

    /// The runtime directory for this call, created if needed.
    fn runtime_dir(&self) -> Result<PathBuf, SshError> {
        let candidates = match &self.runtime_dir {
            Some(dir) => vec![dir.clone()],
            None => crate::private::default_runtime_dirs(),
        };
        crate::private::pick_runtime_dir(&candidates).map_err(SshError::Setup)
    }

    /// The full argument list, without the program itself.
    fn args(
        &self,
        dir: &Path,
        log: &Path,
        host: &str,
        command: String,
    ) -> Result<Vec<String>, SshError> {
        let log = log.to_str().ok_or_else(|| {
            SshError::InvalidArgument("the runtime directory is not valid UTF-8".to_owned())
        })?;
        let secs =
            self.connect_timeout.as_secs() + u64::from(self.connect_timeout.subsec_nanos() > 0);
        let mut args: Vec<String> = vec!["-T".into(), "-E".into(), log.into()];
        for option in [
            "ForwardAgent=no",
            "ForwardX11=no",
            "PermitLocalCommand=no",
            "ClearAllForwardings=yes",
            "RemoteCommand=none",
            "ServerAliveInterval=15",
            "ServerAliveCountMax=3",
            "StrictHostKeyChecking=ask",
        ] {
            args.push("-o".into());
            args.push(option.into());
        }
        args.push("-o".into());
        args.push(format!("ConnectTimeout={}", secs.max(1)));
        if self.prompts.is_none() {
            args.push("-o".into());
            args.push("BatchMode=yes".into());
        }
        if self.multiplex {
            args.push("-o".into());
            args.push("ControlMaster=auto".into());
            args.push("-o".into());
            args.push(control_path(dir)?);
            args.push("-o".into());
            args.push("ControlPersist=10m".into());
        }
        args.push("--".into());
        args.push(host.to_owned());
        args.push(command);
        Ok(args)
    }

    /// Runs `argv` on `host`: `ssh [options] -- <host> <wrapped argv>`. Stdin is empty.
    ///
    /// ssh is stopped if this future is dropped, so a caller can also bound the call with
    /// `tokio::time::timeout`; [`Ssh::run_limited`] bounds it without counting the time a
    /// prompt waits for the user.
    ///
    /// # Errors
    /// See [`SshError`]. A remote command that exits non-zero is not an error: check
    /// [`Output::code`].
    pub async fn run<S: AsRef<str>>(&self, host: &str, argv: &[S]) -> Result<Output, SshError> {
        self.run_limited(host, argv, Limits::default()).await
    }

    /// [`Ssh::run`], stopping ssh when the call breaks `limits`.
    ///
    /// # Errors
    /// As [`Ssh::run`], plus [`SshError::TimedOut`] and [`SshError::OutputTooLarge`].
    pub async fn run_limited<S: AsRef<str>>(
        &self,
        host: &str,
        argv: &[S],
        limits: Limits,
    ) -> Result<Output, SshError> {
        validate_host(host)?;
        let remote = remote_command(argv)?;
        if let Some(prompts) = &self.prompts
            && !prompts.program.is_absolute()
        {
            return Err(SshError::InvalidArgument(format!(
                "the askpass program {} is not an absolute path",
                prompts.program.display()
            )));
        }
        let dir = self.runtime_dir()?;
        let log = SshLog::new(&dir)?;
        let mut command = self.command();
        command
            .args(self.args(&dir, &log.0, host, remote)?)
            .env_remove("SSH_ASKPASS_PROMPT");
        let server = match &self.prompts {
            Some(prompts) => {
                let server = AskpassServer::start(&dir, host, prompts.handler.clone())
                    .map_err(SshError::Setup)?;
                command
                    .env("SSH_ASKPASS", &prompts.program)
                    .env("SSH_ASKPASS_REQUIRE", "force");
                for (name, value) in server.env() {
                    command.env(name, value);
                }
                Some(server)
            }
            None => {
                command
                    .env_remove("SSH_ASKPASS")
                    .env_remove("SSH_ASKPASS_REQUIRE");
                None
            }
        };
        let mut child = Running::spawn(command)?;
        let outcome = {
            let refused = async {
                match &server {
                    Some(server) => server.wait_refused().await,
                    None => std::future::pending().await,
                }
            };
            let expired = async {
                match limits.timeout {
                    Some(after) => {
                        expire(after, server.as_ref().map(AskpassServer::open_prompts)).await;
                        after
                    }
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                done = collect(&mut child.0, limits.max_output) => done,
                () = refused => Err(SshError::Cancelled),
                after = expired => Err(SshError::TimedOut(after)),
            }
        };
        let (status, stdout, stderr) = match outcome {
            Ok(done) => done,
            Err(e) => {
                // Kill before the server goes: a cancelled prompt is answered only by its
                // connection closing, when nothing is left to act on it.
                child.kill().await;
                return Err(e);
            }
        };
        let refused = server.as_ref().is_some_and(AskpassServer::refused);
        drop(server);
        let code = status.code();
        if code == Some(255) {
            if refused {
                return Err(SshError::Cancelled);
            }
            let said = log.read();
            if !said.trim().is_empty() {
                let mut detail = said.clone();
                detail.push_str(&String::from_utf8_lossy(&stderr));
                return Err(classify_failure(&said, detail));
            }
        }
        Ok(Output {
            code,
            stdout,
            stderr,
        })
    }

    /// Asks ssh what `host` resolves to (`ssh -G`), honouring the user's config exactly. Does
    /// not connect.
    ///
    /// # Errors
    /// The host is refused, ssh fails, or its output lacks a host name.
    pub async fn resolve(&self, host: &str) -> Result<ResolvedHost, SshError> {
        validate_host(host)?;
        let mut command = self.command();
        command.args(["-G", "--", host]);
        let out = command.output().await.map_err(SshError::Spawn)?;
        if !out.status.success() {
            return Err(SshError::Ssh {
                code: out.status.code().unwrap_or(-1),
                stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            });
        }
        parse_resolved(&String::from_utf8_lossy(&out.stdout))
    }

    fn command(&self) -> tokio::process::Command {
        let mut command = tokio::process::Command::new(&self.program);
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(windows)]
        {
            // CREATE_NO_WINDOW: a desktop app must not flash a console for each call.
            command.creation_flags(0x0800_0000);
        }
        command
    }
}

/// A running ssh. Dropping it kills ssh (tokio's `kill_on_drop`) and, on Unix, first its whole
/// process group.
struct Running(tokio::process::Child);

impl Running {
    fn spawn(mut command: tokio::process::Command) -> Result<Self, SshError> {
        // Its own group, so everything it starts can be killed with it. ssh's askpass and
        // ProxyJump children stay in it; a ControlPersist master leaves it (it calls setsid).
        #[cfg(unix)]
        command.process_group(0);
        command.spawn().map(Self).map_err(SshError::Spawn)
    }

    /// Kills ssh's process group. Only while ssh is not yet reaped (tokio's `id()` is `None`
    /// after that): until then the group id cannot belong to anyone else.
    fn kill_group(&mut self) {
        #[cfg(unix)]
        if let Some(pid) = self
            .0
            .id()
            .and_then(|id| i32::try_from(id).ok())
            .and_then(rustix::process::Pid::from_raw)
        {
            let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
        }
    }

    async fn kill(&mut self) {
        self.kill_group();
        let _ = self.0.kill().await;
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.kill_group();
    }
}

/// Reads stdout and stderr to the end and waits for ssh.
async fn collect(
    child: &mut tokio::process::Child,
    max_output: Option<usize>,
) -> Result<(ExitStatus, Vec<u8>, Vec<u8>), SshError> {
    let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
        return Err(SshError::Io(io::Error::other("ssh's output is not piped")));
    };
    let (stdout, stderr, status) = tokio::try_join!(
        read_capped(stdout, max_output),
        read_capped(stderr, max_output),
        async { child.wait().await.map_err(SshError::Io) },
    )?;
    Ok((status, stdout, stderr))
}

async fn read_capped(
    mut from: impl AsyncRead + Unpin,
    limit: Option<usize>,
) -> Result<Vec<u8>, SshError> {
    let mut buf = Vec::new();
    match limit {
        None => {
            from.read_to_end(&mut buf).await.map_err(SshError::Io)?;
        }
        Some(limit) => {
            let most = u64::try_from(limit).unwrap_or(u64::MAX).saturating_add(1);
            (&mut from)
                .take(most)
                .read_to_end(&mut buf)
                .await
                .map_err(SshError::Io)?;
            if buf.len() > limit {
                return Err(SshError::OutputTooLarge { limit });
            }
        }
    }
    Ok(buf)
}

/// Completes once `after` has passed with no prompt open. A prompt stops the clock; when the
/// last one closes, the clock starts again from zero.
async fn expire(after: Duration, open: Option<watch::Receiver<usize>>) {
    if let Some(mut open) = open {
        loop {
            if open.wait_for(|n| *n == 0).await.is_err() {
                break;
            }
            let opened = async { open.wait_for(|n| *n > 0).await.is_ok() };
            tokio::select! {
                () = tokio::time::sleep(after) => return,
                still_open = opened => if !still_open { break },
            }
        }
    }
    tokio::time::sleep(after).await;
}

/// ssh's log file for one call (`-E`), removed when dropped.
struct SshLog(PathBuf);

impl SshLog {
    fn new(dir: &Path) -> Result<Self, SshError> {
        let tag = crate::askpass::random::<8>().map_err(SshError::Setup)?;
        Ok(Self(
            dir.join(format!("log-{}", crate::askpass::to_hex(&tag))),
        ))
    }

    fn read(&self) -> String {
        let mut text = Vec::new();
        if let Ok(file) = std::fs::File::open(&self.0) {
            let _ = file.take(MAX_LOG).read_to_end(&mut text);
        }
        String::from_utf8_lossy(&text).into_owned()
    }
}

impl Drop for SshLog {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// `ControlPath=<dir>/%C`. `%C` is a 40-character hash of the connection, which keeps the path
/// short; ssh adds a 17-character suffix while creating it, and unix socket paths are limited to
/// about 104 bytes. The directory's name was checked by
/// [`crate::private::check_dir_name`]; the config parser also splits on blanks.
fn control_path(dir: &Path) -> Result<String, SshError> {
    let dir = dir.to_str().ok_or_else(|| {
        SshError::InvalidArgument("the runtime directory is not valid UTF-8".to_owned())
    })?;
    if dir.chars().any(char::is_whitespace) {
        return Err(SshError::InvalidArgument(format!(
            "the runtime directory {dir:?} contains spaces"
        )));
    }
    if dir.len() + 1 + 40 + 17 > 100 {
        return Err(SshError::InvalidArgument(format!(
            "the runtime directory {dir:?} is too long for a socket path"
        )));
    }
    Ok(format!("ControlPath={dir}/%C"))
}

/// Maps what ssh logged before exiting 255 to an error. `detail` is what the error carries.
pub(crate) fn classify_failure(logged: &str, detail: String) -> SshError {
    let text = logged.to_lowercase();
    let has = |needles: &[&str]| needles.iter().any(|n| text.contains(n));
    let stderr = detail;
    if has(&["remote host identification has changed", "host key for"]) && has(&["changed"]) {
        SshError::HostKeyChanged { stderr }
    } else if has(&["host key verification failed"]) {
        SshError::HostKeyRejected { stderr }
    } else if has(&["timed out", "connection timeout"]) {
        SshError::ConnectTimeout { stderr }
    } else if has(&[
        "could not resolve hostname",
        "name or service not known",
        "nodename nor servname",
        "connection refused",
        "no route to host",
        "network is unreachable",
        "kex_exchange_identification",
    ]) {
        SshError::Unreachable { stderr }
    } else if has(&["permission denied (", "too many authentication failures"]) {
        SshError::AuthFailed { stderr }
    } else {
        SshError::Ssh { code: 255, stderr }
    }
}

fn parse_resolved(text: &str) -> Result<ResolvedHost, SshError> {
    let mut hostname = None;
    let mut user = None;
    let mut port = 22;
    let mut proxy_jump = None;
    let mut proxy_command = None;
    for line in text.lines() {
        let Some((key, value)) = line.trim().split_once(' ') else {
            continue;
        };
        let value = value.trim();
        let set = (!value.is_empty() && value != "none").then(|| value.to_owned());
        match key.to_ascii_lowercase().as_str() {
            "hostname" => hostname = set,
            "user" => user = set,
            "port" => port = value.parse().unwrap_or(22),
            "proxyjump" => proxy_jump = set,
            "proxycommand" => proxy_command = set,
            _ => {}
        }
    }
    let hostname = hostname
        .ok_or_else(|| SshError::UnexpectedOutput("ssh -G printed no hostname".to_owned()))?;
    Ok(ResolvedHost {
        hostname,
        user,
        port,
        proxy_jump,
        proxy_command,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failures_are_classified() {
        type Check = fn(&SshError) -> bool;
        let cases: [(&str, Check); 7] = [
            (
                "ssh: connect to host h port 22: Connection timed out\r\n",
                |e| matches!(e, SshError::ConnectTimeout { .. }),
            ),
            (
                "ssh: Could not resolve hostname nope: Name or service not known\n",
                |e| matches!(e, SshError::Unreachable { .. }),
            ),
            ("Host key verification failed.\n", |e| {
                matches!(e, SshError::HostKeyRejected { .. })
            }),
            (
                "@@@\n@    WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!     @\n\
                 Host key verification failed.\n",
                |e| matches!(e, SshError::HostKeyChanged { .. }),
            ),
            ("u@h: Permission denied (publickey,password).\n", |e| {
                matches!(e, SshError::AuthFailed { .. })
            }),
            (
                "mux_client_request_session: read from master failed\n",
                |e| matches!(e, SshError::Ssh { code: 255, .. }),
            ),
            ("", |e| matches!(e, SshError::Ssh { .. })),
        ];
        for (logged, check) in cases {
            let err = classify_failure(logged, logged.to_owned());
            assert!(check(&err), "{logged:?} -> {err:?}");
        }
    }

    #[test]
    fn error_lines_lose_control_characters() {
        let err = SshError::Ssh {
            code: 255,
            stderr: "first\n\x1b[31mred\x07 al\rert\r\n\n".to_owned(),
        };
        assert_eq!(
            err.to_string(),
            "ssh failed with exit code 255: ?[31mred? al?ert"
        );
    }

    #[test]
    fn resolved_hosts_parse() {
        let text = "user someone\nhostname login.example.org\nport 2222\n\
                    proxyjump bastion\nforwardagent no\n";
        let host = parse_resolved(text).unwrap();
        assert_eq!(
            host,
            ResolvedHost {
                hostname: "login.example.org".into(),
                user: Some("someone".into()),
                port: 2222,
                proxy_jump: Some("bastion".into()),
                proxy_command: None,
            }
        );
        assert!(parse_resolved("port 22\n").is_err());
        let host = parse_resolved("hostname h\nproxyjump none\n").unwrap();
        assert_eq!(host.proxy_jump, None);
    }

    #[test]
    fn control_paths_are_checked() {
        assert_eq!(
            control_path(Path::new("/run/user/1000/pitcrew-ssh")).unwrap(),
            "ControlPath=/run/user/1000/pitcrew-ssh/%C"
        );
        assert!(control_path(Path::new("/tmp/a b")).is_err());
        assert!(control_path(Path::new(&format!("/tmp/{}", "x".repeat(60)))).is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn the_clock_stops_while_a_prompt_is_open() {
        let (open, rx) = watch::channel(0usize);
        let started = tokio::time::Instant::now();
        let timer = tokio::spawn(expire(Duration::from_secs(10), Some(rx)));
        tokio::time::sleep(Duration::from_secs(8)).await;
        open.send_replace(1);
        // A user who takes a minute to answer does not trip it.
        tokio::time::sleep(Duration::from_secs(60)).await;
        assert!(!timer.is_finished());
        open.send_replace(0);
        tokio::time::sleep(Duration::from_secs(9)).await;
        assert!(!timer.is_finished());
        timer.await.unwrap();
        assert_eq!(started.elapsed(), Duration::from_secs(8 + 60 + 10));

        let started = tokio::time::Instant::now();
        expire(Duration::from_secs(3), None).await;
        assert_eq!(started.elapsed(), Duration::from_secs(3));
    }
}
