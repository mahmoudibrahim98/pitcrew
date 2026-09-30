//! Running commands on a host with the user's own OpenSSH.
//!
//! **Connection reuse.** On Unix every call passes `ControlMaster=auto` with a `ControlPath` in
//! a private 0700 directory and `ControlPersist=10m`, so the first call authenticates and later
//! ones reuse its connection. Windows OpenSSH has no ControlMaster: each call connects and
//! authenticates anew (with keys that is quick; with passwords or one-time codes the user is
//! asked each time). A persistent channel on Windows comes with the tunnel work.
//!
//! **Prompts** need OpenSSH 8.4 or newer on this computer (for `SSH_ASKPASS_REQUIRE=force`).
//! Without a [`PromptHandler`], calls run with `BatchMode=yes` and fail rather than prompt.
//!
//! **Exit 255** is ssh's own failure code; it is mapped to an [`SshError`] by what ssh printed.
//! A remote command that itself exits 255 with none of those messages is reported as
//! [`SshError::Ssh`].

use crate::askpass::PromptHandler;
use crate::askpass::server::AskpassServer;
use crate::quote::{remote_command, validate_host};
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

/// Default for `ConnectTimeout`.
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

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
    /// The user cancelled a prompt.
    #[error("cancelled")]
    Cancelled,
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
    /// Any other failure with ssh's exit code 255, or of `ssh -G`.
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

fn last_line(text: &str) -> &str {
    text.lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
}

/// The result of a remote command that ran.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Output {
    /// Exit code; `None` if it was killed by a signal.
    pub code: Option<i32>,
    /// Standard output.
    pub stdout: Vec<u8>,
    /// Standard error, including anything ssh printed.
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
    runtime_dir: PathBuf,
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
            runtime_dir: default_runtime_dir(),
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            multiplex: cfg!(unix),
        }
    }

    /// Sends prompts to `handler` through the askpass program at `askpass` (the
    /// `pitcrew-askpass` binary).
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

    /// Where control sockets and askpass sockets go (Unix). It is created 0700 if missing, and
    /// refused if it exists and is not private.
    #[must_use]
    pub fn with_runtime_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.runtime_dir = dir.into();
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

    /// The full argument list for running `argv` on `host`, without the program itself.
    ///
    /// # Errors
    /// The host or an argument is refused, or the runtime directory is unusable.
    pub fn args<S: AsRef<str>>(&self, host: &str, argv: &[S]) -> Result<Vec<String>, SshError> {
        validate_host(host)?;
        let command = remote_command(argv)?;
        let secs =
            self.connect_timeout.as_secs() + u64::from(self.connect_timeout.subsec_nanos() > 0);
        let mut args: Vec<String> = [
            "-T",
            "-o",
            "ForwardAgent=no",
            "-o",
            "ForwardX11=no",
            "-o",
            "PermitLocalCommand=no",
            "-o",
            "ClearAllForwardings=yes",
            "-o",
            "ServerAliveInterval=15",
            "-o",
            "ServerAliveCountMax=3",
            "-o",
            "StrictHostKeyChecking=ask",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
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
            args.push(control_path(&self.runtime_dir)?);
            args.push("-o".into());
            args.push("ControlPersist=10m".into());
        }
        args.push("--".into());
        args.push(host.to_owned());
        args.push(command);
        Ok(args)
    }

    /// Runs `argv` on `host`: `ssh [options] -- <host> <quoted argv>`. Stdin is empty.
    ///
    /// The child is killed if this future is dropped, so a caller can bound the whole call with
    /// `tokio::time::timeout`.
    ///
    /// # Errors
    /// See [`SshError`]. A remote command that exits non-zero (other than 255) is not an error:
    /// check [`Output::code`].
    pub async fn run<S: AsRef<str>>(&self, host: &str, argv: &[S]) -> Result<Output, SshError> {
        let args = self.args(host, argv)?;
        if self.multiplex {
            crate::private::ensure_private_dir(&self.runtime_dir).map_err(SshError::Setup)?;
        }
        let mut command = self.command();
        command.args(&args);
        let server = match &self.prompts {
            Some(prompts) => {
                let server = AskpassServer::start(&self.runtime_dir, host, prompts.handler.clone())
                    .map_err(SshError::Setup)?;
                command
                    .env("SSH_ASKPASS", &prompts.program)
                    .env("SSH_ASKPASS_REQUIRE", "force");
                for (name, value) in server.env() {
                    command.env(name, value);
                }
                Some(server)
            }
            None => None,
        };
        let out = command.output().await.map_err(SshError::Spawn)?;
        let cancelled = server.as_ref().is_some_and(AskpassServer::cancelled);
        drop(server);
        let output = Output {
            code: out.status.code(),
            stdout: out.stdout,
            stderr: out.stderr,
        };
        if output.code == Some(255) {
            return Err(classify_failure(
                &String::from_utf8_lossy(&output.stderr),
                cancelled,
            ));
        }
        Ok(output)
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

#[cfg(unix)]
fn default_runtime_dir() -> PathBuf {
    crate::private::default_runtime_dir()
}

#[cfg(not(unix))]
fn default_runtime_dir() -> PathBuf {
    std::env::temp_dir().join("pitcrew-ssh")
}

/// `ControlPath=<dir>/%C`. `%C` is a 40-character hash of the connection, which keeps the path
/// short; ssh adds a 17-character suffix while creating it, and unix socket paths are limited to
/// about 104 bytes.
fn control_path(dir: &Path) -> Result<String, SshError> {
    let dir = dir.to_str().ok_or_else(|| {
        SshError::InvalidArgument("the runtime directory is not valid UTF-8".to_owned())
    })?;
    if dir
        .chars()
        .any(|c| c.is_whitespace() || c.is_control() || c == '"' || c == '\'')
    {
        return Err(SshError::InvalidArgument(format!(
            "the runtime directory {dir:?} contains spaces, quotes or control characters"
        )));
    }
    if dir.len() + 1 + 40 + 17 > 100 {
        return Err(SshError::InvalidArgument(format!(
            "the runtime directory {dir:?} is too long for a socket path"
        )));
    }
    Ok(format!("ControlPath={}/%C", dir.replace('%', "%%")))
}

/// Maps ssh's exit 255 to an error by what it printed.
pub(crate) fn classify_failure(stderr: &str, cancelled: bool) -> SshError {
    let text = stderr.to_lowercase();
    let has = |needles: &[&str]| needles.iter().any(|n| text.contains(n));
    let stderr = stderr.to_owned();
    if cancelled {
        SshError::Cancelled
    } else if has(&["remote host identification has changed", "host key for"]) && has(&["changed"])
    {
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
        for (stderr, check) in cases {
            let err = classify_failure(stderr, false);
            assert!(check(&err), "{stderr:?} -> {err:?}");
        }
        assert!(matches!(
            classify_failure("Permission denied (keyboard-interactive).", true),
            SshError::Cancelled
        ));
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
        assert_eq!(
            control_path(Path::new("/tmp/50%")).unwrap(),
            "ControlPath=/tmp/50%%/%C"
        );
        assert!(control_path(Path::new("/tmp/a b")).is_err());
        assert!(control_path(Path::new(&format!("/tmp/{}", "x".repeat(60)))).is_err());
    }
}
