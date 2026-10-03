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
//! ssh together with everything it started (its askpass programs, `ProxyJump` hops, `Match
//! exec` commands), before any of them can send an empty credential:
//! - Unix: ssh runs in its own process group, and the group is killed. Output is read to the
//!   end before ssh is reaped, so the group id stays reserved (and the kill safe) as long as a
//!   member can still hold the call up.
//! - Windows: ssh runs in a Job Object (see `job.rs`, the crate's only unsafe code), which is
//!   terminated; the OS also ends it if PitCrew exits or crashes. ssh joins the job right after
//!   it starts (std cannot create it suspended), so a child started in the first microseconds
//!   would escape it. ssh starts none that early: it first loads and reads its config.
//!
//! If PitCrew quits or crashes while a prompt is open, `pitcrew-askpass` stops the ssh that
//! asked instead of failing (see [`crate::askpass`]).
//!
//! **Errors.** ssh's own messages go to a log file (`-E`) in the private runtime directory, apart
//! from the remote command's stderr, at `LogLevel=ERROR`: informational lines ("Permanently
//! added…") and server-sent keyboard-interactive text never reach it. Exit 255 is ssh's failure
//! code; it is an [`SshError`] only when that log shows ssh failing, and the kind of error comes
//! only from ssh's own message formats, matched whole lines at a time. ssh logs server text
//! without escaping newlines, so reading stops at ssh's terminal message (its last words before
//! giving up, such as "Permission denied (…)." whose method list is the server's), and at any
//! line that carries server text (a disconnect reason, the algorithm offer, a refused channel):
//! nothing after either is read. A 255 without such a log is the remote command's own and comes
//! back as an [`Output`].
//!
//! **Input.** [`Ssh::run`] gives the command an empty stdin; [`Ssh::run_with_input`] streams
//! bytes to it (the helper upload uses this), with progress, while the output is read.
//!
//! **Resolving** with `ssh -G` may run `Match exec` commands, so it is bounded by
//! [`RESOLVE_LIMITS`].

use crate::askpass::PromptHandler;
use crate::askpass::server::AskpassServer;
use crate::quote::{remote_command, validate_host};
use std::fmt;
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWriteExt as _};
use tokio::sync::watch;

/// Default for `ConnectTimeout`.
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// How much of ssh's log is read to explain a failure.
const MAX_LOG: u64 = 64 * 1024;

/// Bounds for [`Ssh::resolve`]: 1 MiB of output and 10 seconds.
pub const RESOLVE_LIMITS: Limits = Limits {
    max_output: Some(1024 * 1024),
    timeout: Some(Duration::from_secs(10)),
};

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
    /// The prompt bridge failed during the call (an askpass program failed the handshake, or
    /// got no answer); ssh was stopped before it could send anything.
    #[error("the prompt bridge failed: {0}")]
    Bridge(String),
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
    /// The server refused another session on a shared connection (sshd's `MaxSessions`, 10 by
    /// default and lower on some sites): the connection itself is fine, and a session ends
    /// before another can open.
    #[error("the server refused another session on the connection (its MaxSessions)")]
    SessionRefused {
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
    /// The host's login shell cannot carry commands safely (see [`crate::quote`]).
    #[error("the login shell {0:?} is not supported")]
    UnsupportedShell(String),
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
    /// Exit code; `None` if ssh itself was killed by a signal, in a call without prompts. (A
    /// remote command killed by a signal makes ssh exit 255. With prompts, ssh killed by a
    /// signal is [`SshError::Bridge`]: askpass stops it when it gets no answer.)
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

/// Bytes for a remote command's standard input (see [`Ssh::run_with_input`]): one or more
/// parts sent back to back, then end of file.
pub struct Input<'a> {
    parts: Vec<&'a [u8]>,
    progress: Option<&'a (dyn Fn(u64) + Send + Sync)>,
}

impl<'a> Input<'a> {
    /// Sends `bytes`.
    #[must_use]
    pub fn new(bytes: &'a [u8]) -> Self {
        Self {
            parts: vec![bytes],
            progress: None,
        }
    }

    /// Sends `bytes` after what is already there.
    #[must_use]
    pub fn then(mut self, bytes: &'a [u8]) -> Self {
        self.parts.push(bytes);
        self
    }

    /// Calls `progress` with the number of bytes handed to ssh so far, after each chunk of up to
    /// 64 KiB. ssh buffers a little ahead of the network.
    #[must_use]
    pub fn with_progress(mut self, progress: &'a (dyn Fn(u64) + Send + Sync)) -> Self {
        self.progress = Some(progress);
        self
    }

    /// The number of bytes in all parts.
    #[must_use]
    pub fn len(&self) -> u64 {
        self.parts
            .iter()
            .map(|p| u64::try_from(p.len()).unwrap_or(u64::MAX))
            .fold(0, u64::saturating_add)
    }

    /// Whether there is nothing to send.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl fmt::Debug for Input<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Input")
            .field("len", &self.len())
            .field("progress", &self.progress.is_some())
            .finish()
    }
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

/// A handler that counts the prompts it passes on (see [`Ssh::counting_prompts`]).
struct Counted {
    inner: Arc<dyn PromptHandler>,
    count: Arc<AtomicU64>,
}

impl PromptHandler for Counted {
    fn prompt(
        &self,
        request: crate::askpass::PromptRequest,
        cancel: crate::askpass::PromptCancel,
    ) -> crate::askpass::PromptFuture<'_> {
        self.count.fetch_add(1, Ordering::Relaxed);
        self.inner.prompt(request, cancel)
    }
}

/// The environment variables ssh needs, which [`Ssh::with_env_passthrough`] (and every tunnel
/// call) keeps; everything else is left out. The user's own config can still send variables to
/// the server (`SendEnv`), so the rest of the app's environment never reaches ssh.
/// - Unix: the home, user and path; locale and time zone; the agent's socket; Kerberos'
///   credentials cache and config; a security-key provider; and the temporary and runtime
///   directories some agents and `ProxyCommand`s keep their sockets in.
/// - Windows: what any program needs to start and find the user's profile, and the agent's
///   pipe.
#[cfg(not(windows))]
pub const MINIMAL_ENV: &[&str] = &[
    "HOME",
    "USER",
    "LOGNAME",
    "PATH",
    "SHELL",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "LC_MESSAGES",
    "TZ",
    "TMPDIR",
    "XDG_RUNTIME_DIR",
    "SSH_AUTH_SOCK",
    "SSH_SK_PROVIDER",
    "KRB5CCNAME",
    "KRB5_CONFIG",
];

/// See the Unix version.
#[cfg(windows)]
pub const MINIMAL_ENV: &[&str] = &[
    "SystemRoot",
    "SystemDrive",
    "windir",
    "ComSpec",
    "PATH",
    "PATHEXT",
    "TEMP",
    "TMP",
    "USERPROFILE",
    "USERNAME",
    "USERDOMAIN",
    "HOMEDRIVE",
    "HOMEPATH",
    "APPDATA",
    "LOCALAPPDATA",
    "ProgramData",
    "ProgramFiles",
    "ProgramFiles(x86)",
    "CommonProgramFiles",
    "ALLUSERSPROFILE",
    "COMPUTERNAME",
    "PROCESSOR_ARCHITECTURE",
    "NUMBER_OF_PROCESSORS",
    "OS",
    "HOME",
    "SSH_AUTH_SOCK",
];

/// Which environment ssh gets.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
enum EnvPolicy {
    /// The app's whole environment (the default of [`Ssh::new`]).
    #[default]
    Inherit,
    /// [`MINIMAL_ENV`] and these names.
    Minimal(Vec<String>),
}

/// The ssh program and how to call it. Cheap to clone.
#[derive(Clone)]
pub struct Ssh {
    wsl: bool,
    wsl_home: Option<String>,
    program: PathBuf,
    prompts: Option<Prompts>,
    /// `None`: the first usable of [`crate::private::default_runtime_dirs`].
    runtime_dir: Option<PathBuf>,
    connect_timeout: Duration,
    multiplex: bool,
    env: EnvPolicy,
    /// Only as a client of the ControlMaster listening here: no login of its own, ever.
    through: Option<PathBuf>,
}

impl fmt::Debug for Ssh {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Ssh")
            .field("program", &self.program)
            .field("askpass", &self.prompts.as_ref().map(|p| &p.program))
            .field("runtime_dir", &self.runtime_dir)
            .field("connect_timeout", &self.connect_timeout)
            .field("multiplex", &self.multiplex)
            .field("env", &self.env)
            .field("through", &self.through)
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
            wsl: false,
            wsl_home: None,
            prompts: None,
            runtime_dir: None,
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            multiplex: cfg!(unix),
            env: EnvPolicy::Inherit,
            through: None,
        }
    }

    /// Uses WSL as the command transport, without SSH configuration or prompts.
    #[must_use]
    pub fn wsl(program: impl Into<PathBuf>) -> Self {
        let mut transport = Self::new(program).with_multiplex(false).minimal_env();
        transport.wsl = true;
        transport
    }

    /// Whether this command transport is WSL.
    #[must_use]
    pub fn is_wsl(&self) -> bool {
        self.wsl
    }

    /// Overrides HOME inside WSL, for isolated integration tests or a separate helper home.
    ///
    /// # Errors
    /// The transport is not WSL or the home is not an absolute, safe layout path.
    pub fn with_wsl_home(mut self, home: &str) -> Result<Self, SshError> {
        if !self.wsl {
            return Err(SshError::InvalidArgument(
                "HOME override requires WSL".into(),
            ));
        }
        crate::Layout::in_home(home).map_err(|e| SshError::InvalidArgument(e.to_string()))?;
        self.wsl_home = Some(home.to_owned());
        Ok(self)
    }

    pub(crate) fn validate_destination(&self, host: &str) -> Result<(), SshError> {
        if self.wsl {
            crate::wsl::validate_distro(host)
        } else {
            validate_host(host)
        }
    }

    /// Gives ssh only [`MINIMAL_ENV`] and the variables named here (for a `ProxyCommand` or a
    /// `Match exec` in the user's config that needs one, such as a cloud CLI's profile),
    /// instead of the app's whole environment. The tunnel ([`crate::tunnel`]) always does
    /// this, keeping these names.
    #[must_use]
    pub fn with_env_passthrough<I, S>(mut self, names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.env = EnvPolicy::Minimal(names.into_iter().map(Into::into).collect());
        self
    }

    /// This, giving ssh only [`MINIMAL_ENV`] and any names already passed through.
    pub(crate) fn minimal_env(&self) -> Self {
        let mut ssh = self.clone();
        if ssh.env == EnvPolicy::Inherit {
            ssh.env = EnvPolicy::Minimal(Vec::new());
        }
        ssh
    }

    /// This, as a client of the ControlMaster listening at `control` only: every call becomes
    /// a channel of that connection. With no master there, a call fails at once rather than
    /// logging in (`ProxyCommand=false`), and it never prompts (`BatchMode`). Reads no ssh
    /// config (`-F none`): the master already has the user's settings.
    pub(crate) fn through_master(&self, control: &Path) -> Self {
        let mut ssh = self.minimal_env();
        ssh.prompts = None;
        ssh.through = Some(control.to_path_buf());
        ssh
    }

    /// The ssh program.
    pub(crate) fn program(&self) -> &Path {
        &self.program
    }

    /// Whether prompts go to a handler (else ssh runs in `BatchMode`).
    pub(crate) fn has_prompts(&self) -> bool {
        self.prompts.is_some()
    }

    /// This, with every prompt it shows counted into `count` (the tunnel's: whether its
    /// attempts ask the person).
    pub(crate) fn counting_prompts(&self, count: Arc<AtomicU64>) -> Self {
        let mut ssh = self.clone();
        if let Some(prompts) = &mut ssh.prompts {
            prompts.handler = Arc::new(Counted {
                inner: prompts.handler.clone(),
                count,
            });
        }
        ssh
    }

    /// Whether connections are reused (a ControlMaster; Unix).
    pub(crate) fn multiplexes(&self) -> bool {
        self.multiplex
    }

    /// `ConnectTimeout`, in whole seconds (at least 1).
    pub(crate) fn connect_timeout_secs(&self) -> u64 {
        let secs =
            self.connect_timeout.as_secs() + u64::from(self.connect_timeout.subsec_nanos() > 0);
        secs.max(1)
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
    /// `%TEMP%\pitcrew-ssh` on Windows; the first that suits the call wins). It is created 0700
    /// if missing, and refused if it exists and is not private, or if its name does not suit
    /// the call: see `private::check_dir_name` (with connection reuse: no blanks, quotes, `%`
    /// or `$`, and short enough for a socket path).
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

    /// The runtime directory for this call, created if needed. With `multiplex`, its name must
    /// suit a control socket (see `private::check_dir_name`).
    pub(crate) fn runtime_dir_for(&self, multiplex: bool) -> Result<PathBuf, SshError> {
        let candidates = match &self.runtime_dir {
            Some(dir) => vec![dir.clone()],
            None => crate::private::default_runtime_dirs(),
        };
        crate::private::pick_runtime_dir(&candidates, multiplex).map_err(SshError::Setup)
    }

    /// The runtime directory for this call, created if needed.
    fn runtime_dir(&self) -> Result<PathBuf, SshError> {
        self.runtime_dir_for(self.multiplex)
    }

    /// The full argument list, without the program itself. `extra` goes just before `--`.
    pub(crate) fn args(
        &self,
        dir: &Path,
        log: &Path,
        host: &str,
        command: String,
        extra: &[String],
    ) -> Result<Vec<String>, SshError> {
        if self.wsl {
            self.validate_destination(host)?;
            if !extra.is_empty() {
                return Err(SshError::InvalidArgument(
                    "SSH options cannot be used with WSL".into(),
                ));
            }
            let command = if let Some(home) = &self.wsl_home {
                format!("export HOME={}; {command}", crate::quote::sh_quote(home))
            } else {
                command
            };
            // `--cd ~`: start in the distro user's home, not a translation of the app's Windows
            // working directory (which wsl.exe warns about when it cannot translate it).
            return Ok(vec![
                "-d".into(),
                host.into(),
                "--cd".into(),
                "~".into(),
                "--exec".into(),
                "/bin/sh".into(),
                "-c".into(),
                command,
            ]);
        }
        let log = log.to_str().ok_or_else(|| {
            SshError::InvalidArgument("the runtime directory is not valid UTF-8".to_owned())
        })?;
        if let Some(control) = &self.through {
            return through_args(log, control, host, command, extra);
        }
        let mut args: Vec<String> = vec!["-T".into(), "-E".into(), log.into()];
        for option in [
            // Only ssh's own errors in the log: no server text, no informational lines.
            "LogLevel=ERROR",
            "ForwardAgent=no",
            "ForwardX11=no",
            "PermitLocalCommand=no",
            "ClearAllForwardings=yes",
            "RemoteCommand=none",
            // A `~.` at the start of a line in the data must not end the session.
            "EscapeChar=none",
            "ServerAliveInterval=15",
            "ServerAliveCountMax=3",
            "StrictHostKeyChecking=ask",
        ] {
            args.push("-o".into());
            args.push(option.into());
        }
        args.push("-o".into());
        args.push(format!("ConnectTimeout={}", self.connect_timeout_secs()));
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
        args.extend(extra.iter().cloned());
        args.push("--".into());
        args.push(host.to_owned());
        args.push(command);
        Ok(args)
    }

    /// Starts ssh with `args` (built for `host`), its stdin piped when `stdin` is set, with the
    /// prompt bridge in its environment when there is a handler. The caller owns what comes
    /// back: dropping the [`Running`] kills ssh and everything it started, dropping the server
    /// ends its prompts.
    pub(crate) fn spawn(
        &self,
        dir: &Path,
        host: &str,
        args: Vec<String>,
        stdin: bool,
        env: &[(&str, &str)],
    ) -> Result<(Running, Option<AskpassServer>), SshError> {
        self.check_askpass()?;
        let mut command = self.command();
        command.args(args).env_remove("SSH_ASKPASS_PROMPT");
        for (name, value) in env {
            command.env(name, value);
        }
        if stdin {
            command.stdin(Stdio::piped());
        }
        let server = self.prompt_env(&mut command, dir, host)?;
        Ok((Running::spawn(command)?, server))
    }

    /// Refuses an askpass program that ssh could not run as given.
    fn check_askpass(&self) -> Result<(), SshError> {
        let Some(prompts) = &self.prompts else {
            return Ok(());
        };
        // ssh would look a relative name up on PATH. And a missing program means askpass fails
        // at every prompt, so ssh would send empty passwords: refuse both up front.
        let problem = if !prompts.program.is_absolute() {
            Some("is not an absolute path")
        } else if !prompts.program.is_file() {
            Some("does not exist")
        } else {
            None
        };
        match problem {
            Some(problem) => Err(SshError::InvalidArgument(format!(
                "the askpass program {} {problem}",
                prompts.program.display()
            ))),
            None => Ok(()),
        }
    }

    /// Puts the prompt bridge (a new server, returned) in `command`'s environment, or keeps
    /// any `SSH_ASKPASS` of ours out of it when there is no handler.
    fn prompt_env(
        &self,
        command: &mut tokio::process::Command,
        dir: &Path,
        host: &str,
    ) -> Result<Option<AskpassServer>, SshError> {
        match &self.prompts {
            Some(prompts) => {
                let server = AskpassServer::start(dir, host, prompts.handler.clone())
                    .map_err(SshError::Setup)?;
                command
                    .env("SSH_ASKPASS", &prompts.program)
                    .env("SSH_ASKPASS_REQUIRE", "force");
                for (name, value) in server.env() {
                    command.env(name, value);
                }
                Ok(Some(server))
            }
            None => {
                command
                    .env_remove("SSH_ASKPASS")
                    .env_remove("SSH_ASKPASS_REQUIRE");
                Ok(None)
            }
        }
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
        self.run_inner(host, argv, None, limits).await
    }

    /// [`Ssh::run_limited`], sending `input` to the command's standard input and then closing
    /// it. Writing and reading go on together, so a command may answer before it has read
    /// everything. If it stops reading (it exited), the rest is dropped quietly: its exit code
    /// and output say what happened. `limits` bound the whole call, writing included.
    ///
    /// # Errors
    /// As [`Ssh::run_limited`], plus [`SshError::Io`] if writing fails other than by the
    /// command closing its input.
    pub async fn run_with_input<S: AsRef<str>>(
        &self,
        host: &str,
        argv: &[S],
        input: Input<'_>,
        limits: Limits,
    ) -> Result<Output, SshError> {
        self.run_inner(host, argv, Some(input), limits).await
    }

    async fn run_inner<S: AsRef<str>>(
        &self,
        host: &str,
        argv: &[S],
        input: Option<Input<'_>>,
        limits: Limits,
    ) -> Result<Output, SshError> {
        self.validate_destination(host)?;
        let remote = remote_command(argv)?;
        self.check_askpass()?;
        let dir = self.runtime_dir()?;
        let log = SshLog::new(&dir)?;
        let mut command = self.command();
        command
            .args(self.args(&dir, &log.0, host, remote, &[])?)
            .env_remove("SSH_ASKPASS_PROMPT");
        if input.is_some() {
            command.stdin(Stdio::piped());
        }
        let server = self.prompt_env(&mut command, &dir, host)?;
        let (status, stdout, stderr) = drive(command, server.as_ref(), limits, input).await?;
        let stopped = server.as_ref().and_then(AskpassServer::stopped);
        let prompted = server.is_some();
        drop(server);
        if let Some(why) = stopped {
            // ssh exited on its own just as the call was being stopped.
            return Err(why.error());
        }
        let code = status.code();
        if code.is_none() && prompted {
            // ssh itself was killed by a signal (a remote command killed by one makes ssh exit
            // 255): askpass stops ssh when the bridge gives it no answer.
            return Err(SshError::Bridge(
                "ssh was stopped: the prompt bridge gave askpass no answer".to_owned(),
            ));
        }
        if code == Some(255) {
            let said = log.read();
            let mut detail = said.clone();
            detail.push_str(&String::from_utf8_lossy(&stderr));
            if let Some(failure) = classify_failure(&said, detail) {
                return Err(failure);
            }
        }
        Ok(Output {
            code,
            stdout,
            stderr,
        })
    }

    /// Asks ssh what `host` resolves to (`ssh -G`), honouring the user's config exactly, within
    /// [`RESOLVE_LIMITS`]: the config's `Match exec` commands run here. Does not connect.
    ///
    /// # Errors
    /// The host is refused, ssh fails or breaks the limits, or its output lacks a host name.
    pub async fn resolve(&self, host: &str) -> Result<ResolvedHost, SshError> {
        self.resolve_with(host, RESOLVE_LIMITS).await
    }

    /// [`Ssh::resolve`] with other limits.
    ///
    /// # Errors
    /// As [`Ssh::resolve`].
    pub async fn resolve_with(&self, host: &str, limits: Limits) -> Result<ResolvedHost, SshError> {
        if self.wsl {
            return Err(SshError::InvalidArgument(
                "WSL has no SSH configuration to resolve".into(),
            ));
        }
        self.validate_destination(host)?;
        let mut command = self.command();
        command.args(["-G", "--", host]);
        let (status, stdout, stderr) = drive(command, None, limits, None).await?;
        if !status.success() {
            return Err(SshError::Ssh {
                code: status.code().unwrap_or(-1),
                stderr: String::from_utf8_lossy(&stderr).into_owned(),
            });
        }
        parse_resolved(&String::from_utf8_lossy(&stdout))
    }

    pub(crate) fn command(&self) -> tokio::process::Command {
        let mut command = tokio::process::Command::new(&self.program);
        if let EnvPolicy::Minimal(extra) = &self.env {
            command.env_clear().envs(minimal_env(extra));
        }
        if self.wsl {
            // wsl.exe's own messages (errors, warnings) in UTF-8 rather than UTF-16LE, so they
            // reach people readable. Transport calls only: the distribution listing is parsed
            // as UTF-16LE, and the distro's own output is passed through unchanged either way.
            command.env("WSL_UTF8", "1");
        }
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

/// The app's variables named in [`MINIMAL_ENV`] or `extra` (case-insensitively on Windows,
/// where names are).
fn minimal_env(extra: &[String]) -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
    let wanted = |name: &str| {
        MINIMAL_ENV
            .iter()
            .copied()
            .chain(extra.iter().map(String::as_str))
            .any(|w| {
                if cfg!(windows) {
                    w.eq_ignore_ascii_case(name)
                } else {
                    w == name
                }
            })
    };
    std::env::vars_os()
        .filter(|(name, _)| name.to_str().is_some_and(wanted))
        .collect()
}

/// The arguments of a call through a ControlMaster (see [`Ssh::through_master`]).
fn through_args(
    log: &str,
    control: &Path,
    host: &str,
    command: String,
    extra: &[String],
) -> Result<Vec<String>, SshError> {
    let mut args: Vec<String> = ["-F", "none", "-T", "-E", log].map(str::to_owned).to_vec();
    for option in [
        "LogLevel=ERROR",
        "ForwardAgent=no",
        "ForwardX11=no",
        "PermitLocalCommand=no",
        "ClearAllForwardings=yes",
        "RemoteCommand=none",
        "EscapeChar=none",
        "BatchMode=yes",
        "ControlMaster=no",
        // Without the master, ssh would log in itself: make that fail at once instead.
        "ProxyCommand=false",
    ] {
        args.push("-o".into());
        args.push(option.into());
    }
    args.push("-o".into());
    args.push(control_socket(control)?);
    args.extend(extra.iter().cloned());
    args.push("--".into());
    args.push(host.to_owned());
    args.push(command);
    Ok(args)
}

/// `ControlPath=<path>` for a socket of ours, whose directory was picked with the checks of
/// `private::check_dir_name`; refused if anything in it would be expanded by ssh.
pub(crate) fn control_socket(path: &Path) -> Result<String, SshError> {
    let text = path.to_str().ok_or_else(|| {
        SshError::InvalidArgument("the control socket's path is not valid UTF-8".to_owned())
    })?;
    if !path.is_absolute()
        || text
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || matches!(c, '%' | '$' | '"' | '\''))
    {
        return Err(SshError::InvalidArgument(format!(
            "the control socket's path {text:?} cannot be passed to ssh"
        )));
    }
    Ok(format!("ControlPath={text}"))
}

/// Runs `command` to the end, feeding it `input`: its exit status, stdout and stderr. Stops it,
/// with everything it started, when `server` stops the call (the user cancelled a prompt, or a
/// client failed the handshake) or the call breaks `limits`.
pub(crate) async fn drive(
    command: tokio::process::Command,
    server: Option<&AskpassServer>,
    limits: Limits,
    input: Option<Input<'_>>,
) -> Result<(ExitStatus, Vec<u8>, Vec<u8>), SshError> {
    let mut child = Running::spawn(command)?;
    let stdin = child.child.stdin.take();
    let outcome = {
        let work = async {
            let fed = async {
                match (stdin, input) {
                    (Some(pipe), Some(input)) => feed(pipe, input).await,
                    _ => Ok(()),
                }
            };
            // The first error ends both: too much output must not wait on a blocked writer.
            let ((), collected) =
                tokio::try_join!(fed, collect(&mut child.child, limits.max_output))?;
            Ok(collected)
        };
        let stopped = async {
            match server {
                Some(server) => server.wait_stopped().await,
                None => std::future::pending().await,
            }
        };
        let expired = async {
            match limits.timeout {
                Some(after) => {
                    expire(after, server.map(AskpassServer::open_prompts)).await;
                    after
                }
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            done = work => done,
            why = stopped => Err(why.error()),
            after = expired => Err(SshError::TimedOut(after)),
        }
    };
    if outcome.is_err() {
        // Kill before the server goes: a cancelled prompt is answered only by its connection
        // closing, when nothing is left to act on it. On Windows this ends the job, and with it
        // an askpass that is waiting there for want of an answer.
        child.kill().await;
    }
    outcome
}

/// A running ssh. Dropping it kills ssh (tokio's `kill_on_drop`) and first everything it
/// started: its process group on Unix, its Job Object on Windows.
pub(crate) struct Running {
    pub(crate) child: tokio::process::Child,
    #[cfg(windows)]
    job: crate::job::Job,
}

impl fmt::Debug for Running {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Running")
            .field("pid", &self.child.id())
            .finish_non_exhaustive()
    }
}

impl Running {
    #[cfg(unix)]
    fn spawn(mut command: tokio::process::Command) -> Result<Self, SshError> {
        // Its own group, so everything it starts can be killed with it. ssh's askpass and
        // ProxyJump children stay in it; a ControlPersist master leaves it (it calls setsid).
        command.process_group(0);
        let child = command.spawn().map_err(SshError::Spawn)?;
        Ok(Self { child })
    }

    #[cfg(windows)]
    fn spawn(mut command: tokio::process::Command) -> Result<Self, SshError> {
        // Created first, so a failure leaves nothing running. Without a job ssh does not run:
        // a cancel could not then stop a ProxyJump hop.
        let job = crate::job::Job::new().map_err(SshError::Setup)?;
        let mut child = command.spawn().map_err(SshError::Spawn)?;
        let assigned = crate::job::handle_of(&child)
            .ok_or_else(|| io::Error::other("ssh exited at once"))
            .and_then(|handle| job.assign(handle));
        if let Err(e) = assigned {
            let _ = child.start_kill();
            return Err(SshError::Setup(e));
        }
        Ok(Self { child, job })
    }

    #[cfg(not(any(unix, windows)))]
    fn spawn(mut command: tokio::process::Command) -> Result<Self, SshError> {
        let child = command.spawn().map_err(SshError::Spawn)?;
        Ok(Self { child })
    }

    /// Kills everything ssh started, and ssh.
    /// - Unix: the process group, but only while ssh is not yet reaped (tokio's `id()` is
    ///   `None` after that): until then the group id cannot belong to anyone else.
    /// - Windows: the whole job.
    fn kill_all(&mut self) {
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
        self.job.terminate();
    }

    /// Kills everything ssh started, and ssh, and reaps it.
    pub(crate) async fn kill(&mut self) {
        self.kill_all();
        let _ = self.child.kill().await;
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.kill_all();
    }
}

/// Reads stdout and stderr to the end, then waits for ssh. In that order: until `wait()` reaps
/// ssh, its pid (and so its process group's id) stays reserved, so if the call is cut short
/// while a member of the group still holds a pipe, the group kill still reaches it.
async fn collect(
    child: &mut tokio::process::Child,
    max_output: Option<usize>,
) -> Result<(ExitStatus, Vec<u8>, Vec<u8>), SshError> {
    let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
        return Err(SshError::Io(io::Error::other("ssh's output is not piped")));
    };
    let (stdout, stderr) = tokio::try_join!(
        read_capped(stdout, max_output),
        read_capped(stderr, max_output),
    )?;
    let status = child.wait().await.map_err(SshError::Io)?;
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

/// Writes `input` to ssh's standard input in chunks, then closes it. A closed pipe means the
/// command stopped reading; that is not an error here.
async fn feed(mut pipe: tokio::process::ChildStdin, input: Input<'_>) -> Result<(), SshError> {
    const CHUNK: usize = 64 * 1024;
    let mut sent: u64 = 0;
    for part in &input.parts {
        for chunk in part.chunks(CHUNK) {
            match pipe.write_all(chunk).await {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::BrokenPipe => return Ok(()),
                Err(e) => return Err(SshError::Io(e)),
            }
            sent = sent.saturating_add(u64::try_from(chunk.len()).unwrap_or(u64::MAX));
            if let Some(progress) = input.progress {
                progress(sent);
            }
        }
    }
    // The pipe closes when dropped here, and the command sees end of file.
    match pipe.flush().await {
        Err(e) if e.kind() != io::ErrorKind::BrokenPipe => Err(SshError::Io(e)),
        _ => Ok(()),
    }
}

/// Completes once `after` has passed with no prompt open. A prompt stops the clock; when the
/// last one closes, the clock starts again from zero.
pub(crate) async fn expire(after: Duration, open: Option<watch::Receiver<usize>>) {
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
#[derive(Debug)]
pub(crate) struct SshLog(pub(crate) PathBuf);

impl SshLog {
    pub(crate) fn new(dir: &Path) -> Result<Self, SshError> {
        let tag = crate::askpass::random::<8>().map_err(SshError::Setup)?;
        Ok(Self(
            dir.join(format!("log-{}", crate::askpass::to_hex(&tag))),
        ))
    }

    /// The path, for `-E`.
    pub(crate) fn path(&self) -> &Path {
        &self.0
    }

    /// The first 64 KiB of what ssh logged.
    pub(crate) fn read(&self) -> String {
        let mut text = Vec::new();
        if let Ok(file) = std::fs::File::open(&self.0) {
            let _ = file.take(MAX_LOG).read_to_end(&mut text);
        }
        String::from_utf8_lossy(&text).into_owned()
    }

    /// What ssh logged from byte `from` on, at most 64 KiB of it (a long-lived ssh's log grows).
    pub(crate) fn read_from(&self, from: u64) -> String {
        use std::io::Seek as _;
        let mut text = Vec::new();
        if let Ok(mut file) = std::fs::File::open(&self.0)
            && file.seek(io::SeekFrom::Start(from)).is_ok()
        {
            let _ = file.take(MAX_LOG).read_to_end(&mut text);
        }
        String::from_utf8_lossy(&text).into_owned()
    }

    /// How many bytes ssh has logged so far.
    pub(crate) fn len(&self) -> u64 {
        std::fs::metadata(&self.0).map_or(0, |m| m.len())
    }
}

impl Drop for SshLog {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// `ControlPath=<dir>/%C`. `%C` is a 40-character hash of the connection, which keeps the path
/// short. The directory was picked with the same check, so this only fails for a directory
/// given with [`Ssh::with_runtime_dir`] that cannot work.
fn control_path(dir: &Path) -> Result<String, SshError> {
    crate::private::check_dir_name(dir, true)
        .map_err(|e| SshError::InvalidArgument(e.to_string()))?;
    let dir = dir.to_str().ok_or_else(|| {
        SshError::InvalidArgument("the runtime directory is not valid UTF-8".to_owned())
    })?;
    Ok(format!("ControlPath={dir}/%C"))
}

/// What a line of ssh's log says, read only as one of ssh's own message formats.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Said {
    HostKeyChanged,
    HostKeyRejected,
    ConnectTimeout,
    Unreachable,
    AuthFailed,
    /// A ControlMaster's server refused a session (`MaxSessions`).
    SessionRefused,
    /// Some other error of ssh's.
    Other,
    /// Informational, even at `LogLevel=ERROR`: not a failure.
    Notice,
    /// A message with server-controlled text in it. It shows a failure, but it and everything
    /// after it are not read any further.
    ServerText,
}

/// `"<progname>: <rest>"` with a one-word program name (`ssh`, or `ssh.exe`).
fn after_progname<'a>(line: &'a str, rest: &str) -> Option<&'a str> {
    let (name, tail) = line.split_once(": ")?;
    (!name.is_empty() && !name.contains(char::is_whitespace))
        .then_some(tail)
        .and_then(|tail| tail.strip_prefix(rest))
}

/// What `line` says, and whether it is one of ssh's terminal messages: the last thing ssh logs
/// before giving up. ssh logs server-sent text without escaping newlines, so anything after a
/// terminal line may be the server's (e.g. lines hidden in the method list of "Permission
/// denied (…)."); it is never read.
fn read_line(line: &str) -> (Said, bool) {
    // Messages that embed server text: a disconnect reason, the server's algorithm offer or
    // version string, why the server refused a channel.
    const SERVER_TEXT: [&str; 3] = [
        "Received disconnect from ",
        "Unable to negotiate with ",
        "Bad remote protocol version identification: ",
    ];
    let channel_refused = line
        .strip_prefix("channel ")
        .and_then(|rest| rest.split_once(": open failed: "))
        .is_some_and(|(id, _)| !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()));
    if channel_refused || SERVER_TEXT.iter().any(|p| line.starts_with(p)) {
        return (Said::ServerText, true);
    }
    // The changed-key banner comes before the terminal "Host key verification failed.".
    if line == "@    WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!     @"
        || (line.starts_with("Host key for ")
            && line.ends_with(" has changed and you have requested strict checking."))
    {
        return (Said::HostKeyChanged, false);
    }
    // Batch mode, unknown key: also followed by "Host key verification failed.".
    if line.starts_with("No ")
        && line.contains(" host key is known for ")
        && line.ends_with(" and you have requested strict checking.")
    {
        return (Said::HostKeyRejected, false);
    }
    if line == "Host key verification failed." {
        return (Said::HostKeyRejected, true);
    }
    if let Some(error) = line
        .strip_prefix("ssh: connect to host ")
        .and_then(|rest| rest.rsplit_once(": "))
        .map(|(_, error)| error)
    {
        let said = match error {
            "Connection timed out" | "Operation timed out" => Said::ConnectTimeout,
            _ => Said::Unreachable,
        };
        return (said, true);
    }
    if line == "Connection timed out during banner exchange" {
        return (Said::ConnectTimeout, true);
    }
    if after_progname(line, "Could not resolve hostname ").is_some()
        || line.starts_with("kex_exchange_identification: ")
    {
        return (Said::Unreachable, true);
    }
    // "user@host: Permission denied (publickey,password)." or, older, without the prefix. The
    // method list is the server's, so only the start is matched, and the line ends the read.
    if line.starts_with("Permission denied (")
        || after_progname(line, "Permission denied (").is_some()
    {
        return (Said::AuthFailed, true);
    }
    if [
        "Connection closed by ",
        "Connection reset by ",
        "Disconnected from ",
    ]
    .iter()
    .any(|p| line.starts_with(p))
    {
        return (Said::Other, true);
    }
    // A mux client whose master's server said no to the session: "<function>: session request
    // failed: Session open refused by peer". The text is the master's own (the server's reason is
    // not in it). Not terminal: ssh then tries to log in itself, which PitCrew's calls through a
    // master make fail at once.
    if line
        .strip_suffix(": session request failed: Session open refused by peer")
        .is_some_and(|name| !name.is_empty() && !name.contains(char::is_whitespace))
    {
        return (Said::SessionRefused, false);
    }
    if line.starts_with("Warning: ")
        || (line.starts_with("ControlSocket ")
            && line.ends_with(" already exists, disabling multiplexing"))
    {
        return (Said::Notice, false);
    }
    (Said::Other, false)
}

/// Maps what ssh logged before exiting 255 to an error, or `None` when the log does not show ssh
/// failing: the 255 is then the remote command's. `detail` is what the error carries.
pub(crate) fn classify_failure(logged: &str, detail: String) -> Option<SshError> {
    let mut failed = false;
    let mut kinds = Vec::new();
    for line in logged
        .lines()
        .map(|l| l.trim_end_matches('\r'))
        .filter(|l| !l.trim().is_empty())
    {
        let (said, terminal) = read_line(line);
        match said {
            Said::Notice => {}
            Said::ServerText => failed = true,
            said => {
                failed = true;
                kinds.push(said);
            }
        }
        if terminal {
            break;
        }
    }
    if !failed {
        return None;
    }
    let stderr = detail;
    let has = |said: Said| kinds.contains(&said);
    Some(if has(Said::SessionRefused) {
        SshError::SessionRefused { stderr }
    } else if has(Said::HostKeyChanged) {
        SshError::HostKeyChanged { stderr }
    } else if has(Said::HostKeyRejected) {
        SshError::HostKeyRejected { stderr }
    } else if has(Said::ConnectTimeout) {
        SshError::ConnectTimeout { stderr }
    } else if has(Said::Unreachable) {
        SshError::Unreachable { stderr }
    } else if has(Said::AuthFailed) {
        SshError::AuthFailed { stderr }
    } else {
        SshError::Ssh { code: 255, stderr }
    })
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

    fn kind(logged: &str) -> Option<&'static str> {
        classify_failure(logged, logged.to_owned()).map(|e| match e {
            SshError::HostKeyChanged { .. } => "changed",
            SshError::HostKeyRejected { .. } => "rejected",
            SshError::ConnectTimeout { .. } => "timeout",
            SshError::Unreachable { .. } => "unreachable",
            SshError::AuthFailed { .. } => "auth",
            SshError::SessionRefused { .. } => "sessions",
            SshError::Ssh { code: 255, .. } => "ssh",
            _ => "other",
        })
    }

    /// Real OpenSSH lines (as `-E` writes them, with CRLF).
    #[test]
    fn failures_are_classified() {
        let cases = [
            (
                "ssh: connect to host h port 22: Connection timed out\r\n",
                "timeout",
            ),
            ("Connection timed out during banner exchange\r\n", "timeout"),
            (
                "ssh: Could not resolve hostname nope: Name or service not known\r\n",
                "unreachable",
            ),
            (
                "ssh: Could not resolve hostname nope: nodename nor servname provided, or not \
                 known\r\n",
                "unreachable",
            ),
            (
                "ssh: connect to host 192.0.2.1 port 22: Connection refused\r\n",
                "unreachable",
            ),
            (
                "kex_exchange_identification: Connection closed by remote host\r\n",
                "unreachable",
            ),
            ("Host key verification failed.\r\n", "rejected"),
            (
                "@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@\r\n\
                 @    WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!     @\r\n\
                 Host key verification failed.\r\n",
                "changed",
            ),
            (
                "u@h: Permission denied (publickey,gssapi-with-mic,password).\r\n",
                "auth",
            ),
            ("Permission denied (publickey).\r\n", "auth"),
            (
                "mux_client_request_session: session request failed: Session open refused by \
                 peer\r\nkex_exchange_identification: Connection closed by remote host\r\n",
                "sessions",
            ),
            (
                "mux_client_request_session: read from master failed: Broken pipe\r\n",
                "ssh",
            ),
        ];
        for (logged, want) in cases {
            assert_eq!(kind(logged), Some(want), "{logged:?}");
        }
    }

    /// Nothing that means ssh failed: the 255 belongs to the remote command.
    #[test]
    fn informational_lines_are_not_failures() {
        for logged in [
            "",
            "\r\n",
            "Warning: Permanently added 'h' (ED25519) to the list of known hosts.\r\n",
            "ControlSocket /tmp/x/abc already exists, disabling multiplexing\r\n",
        ] {
            assert_eq!(kind(logged), None, "{logged:?}");
        }
    }

    /// Text inside ssh's messages that a server controls cannot pick the error.
    #[test]
    fn server_text_cannot_steer_the_error() {
        for logged in [
            // A disconnect reason with a line of its own.
            "Received disconnect from 192.0.2.1 port 22:2: bye\r\nHost key verification \
             failed.\r\nDisconnected from 192.0.2.1 port 22\r\n",
            // The server's algorithm offer, with a newline in it.
            "Unable to negotiate with 192.0.2.1 port 22: no matching host key type found. \
             Their offer: x\nu@h: Permission denied (publickey).\r\n",
            // Why the server refused the session channel.
            "channel 0: open failed: administratively prohibited: x\n@    WARNING: REMOTE \
             HOST IDENTIFICATION HAS CHANGED!     @\r\n",
            "Bad remote protocol version identification: 'SSH-2.0-x\nHost key verification \
             failed.'\r\n",
        ] {
            assert_eq!(kind(logged), Some("ssh"), "{logged:?}");
        }
        // Formats are matched whole, not as substrings.
        for logged in [
            "note: Host key verification failed.\r\n",
            "something about timed out\r\n",
            "channel x: open failed: Host key verification failed.\r\n",
        ] {
            assert_eq!(kind(logged), Some("ssh"), "{logged:?}");
        }
    }

    /// ssh logs the server's method list without escaping newlines, so a server can put whole
    /// lines after "Permission denied (". Reading stops at that terminal line.
    #[test]
    fn lines_injected_after_a_terminal_line_are_ignored() {
        for logged in [
            "u@h: Permission denied (publickey).\n@    WARNING: REMOTE HOST IDENTIFICATION HAS \
             CHANGED!     @\nHost key verification failed.\n).\r\n",
            "u@h: Permission denied (a b).\n@    WARNING: REMOTE HOST IDENTIFICATION HAS \
             CHANGED!     @\n).\r\n",
            "Permission denied (x).\nssh: connect to host h port 22: Connection timed out\r\n",
        ] {
            assert_eq!(kind(logged), Some("auth"), "{logged:?}");
        }
        // Other terminal lines end the read too.
        assert_eq!(
            kind(
                "Host key verification failed.\r\n@    WARNING: REMOTE HOST IDENTIFICATION HAS \
                 CHANGED!     @\r\n"
            ),
            Some("rejected")
        );
        assert_eq!(
            kind("Connection closed by 192.0.2.1 port 22\r\nHost key verification failed.\r\n"),
            Some("ssh")
        );
        // The real changed-key report is read through: its banner comes before the terminal
        // line.
        assert_eq!(
            kind(
                "@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@\r\n\
                 @    WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!     @\r\n\
                 @@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@\r\n\
                 IT IS POSSIBLE THAT SOMEONE IS DOING SOMETHING NASTY!\r\n\
                 Host key for h has changed and you have requested strict checking.\r\n\
                 Host key verification failed.\r\n"
            ),
            Some("changed")
        );
        // Batch mode, unknown key.
        assert_eq!(
            kind(
                "No ED25519 host key is known for h and you have requested strict checking.\r\n\
                 Host key verification failed.\r\n"
            ),
            Some("rejected")
        );
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

    #[cfg(unix)]
    #[test]
    fn control_paths_are_checked() {
        assert_eq!(
            control_path(Path::new("/run/user/1000/pitcrew-ssh")).unwrap(),
            "ControlPath=/run/user/1000/pitcrew-ssh/%C"
        );
        for bad in ["/tmp/a b", "/tmp/50%", "/tmp/$X", "/tmp/a'b"] {
            assert!(
                matches!(
                    control_path(Path::new(bad)),
                    Err(SshError::InvalidArgument(_))
                ),
                "{bad}"
            );
        }
        assert!(control_path(Path::new(&format!("/tmp/{}", "x".repeat(60)))).is_err());
    }

    /// WSL calls: the distro as one argument, the home as the working directory, wsl.exe's own
    /// messages in UTF-8; none of it for ssh, nor for the distribution listing.
    #[test]
    fn wsl_calls_start_at_home_with_readable_messages() {
        let dir = Path::new("/run/user/1000/pitcrew-ssh");
        let wsl = Ssh::wsl("wsl.exe");
        for distro in [
            "Lab 'quoted' distro",
            "a \"b\" c\\",
            "trailing\\",
            "& ^ % !",
        ] {
            let args = wsl
                .args(dir, &dir.join("log"), distro, "true".into(), &[])
                .unwrap();
            assert_eq!(
                args,
                ["-d", distro, "--cd", "~", "--exec", "/bin/sh", "-c", "true"]
            );
        }
        assert!(
            wsl.args(dir, &dir.join("log"), "-x", "true".into(), &[])
                .is_err()
        );
        let utf8 = |ssh: &Ssh| {
            ssh.command()
                .as_std()
                .get_envs()
                .find(|(name, _)| *name == "WSL_UTF8")
                .and_then(|(_, value)| value.map(ToOwned::to_owned))
        };
        assert_eq!(utf8(&wsl), Some("1".into()));
        assert_eq!(utf8(&Ssh::new("ssh").minimal_env()), None);
        assert_eq!(utf8(&Ssh::new("wsl.exe").minimal_env()), None);
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
