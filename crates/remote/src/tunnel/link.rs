//! The tunnel's own ssh connection to a host: `ssh -N`, kept running while the connector lives.
//!
//! - **Unix:** it is a ControlMaster (`ControlMaster=yes`, a socket in the connector's private
//!   directory, `ControlPersist=no`): every call of the tunnel (the endpoint check, each stdio
//!   bridge, a forward) is a channel of it, so a machine is logged in to once per (re)connection,
//!   whatever the number of connections. It is ready once its control socket exists.
//! - **Windows** (or without connection reuse): OpenSSH there has no ControlMaster, so it is
//!   only a heartbeat: a connection whose keepalives notice a lost network. It is ready once ssh
//!   logs that it authenticated.
//!
//! **Keepalives** (`ServerAliveInterval` [`KEEPALIVE_INTERVAL`]) end it after a silence that
//! depends on what else watches the network:
//! - a *patient* link, whose forwarded socket the connector probes ([`KEEPALIVE_COUNT_PATIENT`]):
//!   the probe reports a silence within ten seconds, and the link waits 30 s before giving up,
//!   so a short outage costs no new login (no new one-time code). A link starts patient only
//!   where a forward worked before, and not for srun; one that ends up carrying the bridge is
//!   started again impatient;
//! - otherwise ([`KEEPALIVE_COUNT`]): the keepalives are the only sign of a silent network (a
//!   probe of its own would cost a session, which sshd's `MaxSessions` limits, or a refused
//!   forward, which sshd logs), so the link gives up after 8 s.
//!
//! Agent and X11 forwarding, local commands and the user's configured forwardings stay off, as
//! for every call; so does `ForkAfterAuthentication` where the user's config turns it on (the
//! link would leave the connector's watch). Its log (`-E`, at `INFO`, so that "Timeout, server
//! not responding." is in it) gives the reason when it ends.

use crate::askpass::server::AskpassServer;
use crate::quote::validate_host;
use crate::ssh::{Running, SshLog, classify_failure, expire, last_line};
use crate::{Ssh, SshError};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::AsyncReadExt as _;

/// Seconds between keepalives when the server is silent (`ServerAliveInterval`).
pub const KEEPALIVE_INTERVAL: u32 = 2;
/// Unanswered keepalives before a link without a probed forward gives up
/// (`ServerAliveCountMax`): after `(KEEPALIVE_COUNT + 1) × KEEPALIVE_INTERVAL` = 8 seconds of
/// silence.
pub const KEEPALIVE_COUNT: u32 = 3;
/// The same for a patient link, whose forward is probed: after 30 seconds of silence.
pub const KEEPALIVE_COUNT_PATIENT: u32 = 14;

// The brief's bound: a lost connection is noticed within ten seconds (the monitor polls once a
// second); a patient link notices through its probe instead.
const _: () = assert!((KEEPALIVE_COUNT + 1) * KEEPALIVE_INTERVAL + 1 < 10);
const _: () = assert!((KEEPALIVE_COUNT_PATIENT + 1) * KEEPALIVE_INTERVAL == 30);

/// How a link reaches its host when that is a compute node.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Via<'a> {
    /// Unix: through the login link's ControlMaster (`ProxyCommand` running
    /// `ssh -W '[%h]:%p'` as a client of that master), so the login node is not logged in to
    /// again.
    Master {
        /// The login link's control socket.
        control: &'a Path,
        /// The login node, as given to ssh.
        login: &'a str,
    },
    /// Windows: `-J <login>` (ssh's own jump, which logs in to the login node itself).
    Jump(&'a str),
}

/// What a link is started with.
#[derive(Clone, Copy, Debug)]
pub(crate) struct LinkSpec<'a> {
    /// How to call ssh (with the prompt handler, if any, and the minimal environment).
    pub(crate) ssh: &'a Ssh,
    /// The connector's private directory.
    pub(crate) dir: &'a Path,
    /// The control socket's file name in `dir` (Unix).
    pub(crate) name: &'a str,
    /// The host, as given to ssh.
    pub(crate) host: &'a str,
    /// For a compute node, the way through the login node.
    pub(crate) via: Option<Via<'a>>,
    /// A ControlMaster (Unix, with connection reuse), or a heartbeat.
    pub(crate) master: bool,
    /// Wait 30 s of silence before giving up (a probed forward watches), else 8 s.
    pub(crate) patient: bool,
}

/// A running link.
#[derive(Debug)]
pub(crate) struct Link {
    #[cfg_attr(not(unix), allow(dead_code))]
    host: String,
    proc: Running,
    log: SshLog,
    /// Kept for the link's life: ssh may ask again (an updated host key).
    askpass: Option<AskpassServer>,
    control: Option<PathBuf>,
    stderr: Arc<Mutex<Vec<u8>>>,
    patient: bool,
}

impl Link {
    /// Starts the link and waits up to `wait` (not counting time spent on prompts) for it to
    /// be ready.
    ///
    /// # Errors
    /// ssh failed (its error, from its log), a prompt was cancelled, or `wait` passed.
    pub(crate) async fn start(spec: LinkSpec<'_>, wait: Duration) -> Result<Self, SshError> {
        spec.ssh.validate_destination(spec.host)?;
        let log = SshLog::new(spec.dir)?;
        let control = spec.master.then(|| spec.dir.join(spec.name));
        let args = if spec.ssh.is_wsl() {
            // A distro exit ends this heartbeat and triggers the existing reconnect ladder.
            let command = crate::quote::remote_command(&[
                "sh",
                "-c",
                "printf 'pitcrew-wsl-ready\\n' >&2; while :; do sleep 2; done",
            ])?;
            spec.ssh
                .args(spec.dir, log.path(), spec.host, command, &[])?
        } else {
            let fork = knows_fork_after_authentication(spec.ssh, spec.host).await;
            link_args(&spec, log.path(), control.as_deref(), fork)?
        };
        if let Some(control) = &control {
            let _ = std::fs::remove_file(control);
        }
        // ssh runs a ProxyCommand with `$SHELL -c "exec …"`; ours is written for sh.
        let env: &[(&str, &str)] = match spec.via {
            Some(Via::Master { .. }) => &[("SHELL", "/bin/sh")],
            _ => &[],
        };
        let (mut proc, askpass) = spec.ssh.spawn(spec.dir, spec.host, args, false, env)?;
        let stderr = drain(&mut proc);
        let mut link = Self {
            host: spec.host.to_owned(),
            proc,
            log,
            askpass,
            control,
            stderr,
            patient: spec.patient,
        };
        if spec.ssh.is_wsl() {
            tokio::time::timeout(wait, async {
                loop {
                    if text(&link.stderr)
                        .lines()
                        .any(|line| line == "pitcrew-wsl-ready")
                    {
                        return Ok(());
                    }
                    if !link.alive() {
                        return Err(SshError::UnexpectedOutput("WSL heartbeat exited".into()));
                    }
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
            })
            .await
            .map_err(|_| SshError::TimedOut(wait))??;
        } else {
            link.ready(wait).await?;
        }
        Ok(link)
    }

    /// Whether it waits 30 s of silence before giving up (see the module docs).
    pub(crate) fn patient(&self) -> bool {
        self.patient
    }

    async fn ready(&mut self, wait: Duration) -> Result<(), SshError> {
        let open = self.askpass.as_ref().map(AskpassServer::open_prompts);
        let deadline = expire(wait, open);
        tokio::pin!(deadline);
        let stderr = self.stderr.clone();
        loop {
            if self.is_ready() {
                return Ok(());
            }
            let Self {
                proc, askpass, log, ..
            } = &mut *self;
            let stopped = async {
                match askpass {
                    Some(server) => server.wait_stopped().await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                status = proc.child.wait() => {
                    let code = status.ok().and_then(|s| s.code());
                    return Err(failure(&log.read(), &text(&stderr), code));
                }
                why = stopped => {
                    proc.kill().await;
                    return Err(why.error());
                }
                () = &mut deadline => {
                    proc.kill().await;
                    return Err(SshError::TimedOut(wait));
                }
                () = tokio::time::sleep(Duration::from_millis(50)) => {}
            }
        }
    }

    /// Unix: the control socket is there. Windows: ssh logged that it authenticated.
    fn is_ready(&self) -> bool {
        match &self.control {
            Some(control) => is_socket(control),
            None => self.log.read().lines().any(|line| {
                line.starts_with("Authenticated to ")
                    || line.starts_with("Authentication succeeded")
            }),
        }
    }

    /// The host, as given to ssh.
    #[cfg_attr(not(unix), allow(dead_code))]
    pub(crate) fn host(&self) -> &str {
        &self.host
    }

    /// The control socket (Unix).
    pub(crate) fn control(&self) -> Option<&Path> {
        self.control.as_deref()
    }

    /// Completes when ssh exits, with the reason, from its log (a keepalive timeout, the server
    /// closing the connection, …). Cancel safe.
    pub(crate) async fn exited(&mut self) -> String {
        let status = self.proc.child.wait().await;
        let code = status.ok().and_then(|s| s.code());
        self.reason(code)
    }

    /// Whether ssh still runs.
    pub(crate) fn alive(&mut self) -> bool {
        matches!(self.proc.child.try_wait(), Ok(None))
    }

    /// Stops ssh, and with it every channel through it.
    pub(crate) async fn stop(mut self) {
        self.proc.kill().await;
        if let Some(control) = &self.control {
            let _ = std::fs::remove_file(control);
        }
    }

    /// Why the link ended, in a line for people.
    fn reason(&self, code: Option<i32>) -> String {
        let log = self.log.read_from(self.log.len().saturating_sub(16 * 1024));
        let said = log
            .lines()
            .rev()
            .map(|l| l.trim_end_matches('\r'))
            .find(|l| !l.trim().is_empty() && !l.starts_with("Transferred: "));
        match said {
            Some(line) => crate::helper::script::clean(line),
            None => match code {
                Some(code) => format!("ssh exited with code {code}"),
                None => "ssh was stopped".to_owned(),
            },
        }
    }

    /// What ssh's log says from byte `from` on, and how long it is now.
    #[cfg_attr(not(unix), allow(dead_code))]
    pub(crate) fn log_since(&self, from: u64) -> String {
        self.log.read_from(from)
    }

    /// The length of ssh's log so far.
    #[cfg_attr(not(unix), allow(dead_code))]
    pub(crate) fn log_len(&self) -> u64 {
        self.log.len()
    }
}

/// What was kept of a stream.
pub(crate) fn text(kept: &Mutex<Vec<u8>>) -> String {
    kept.lock()
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default()
}

/// The error for an ssh that ended before it was ready: the kind from its log, else its exit.
fn failure(log: &str, stderr: &str, code: Option<i32>) -> SshError {
    let mut detail = log.to_owned();
    detail.push_str(stderr);
    classify_failure(log, detail.clone()).unwrap_or_else(|| SshError::Ssh {
        code: code.unwrap_or(-1),
        stderr: if detail.trim().is_empty() {
            "ssh ended before it was connected".to_owned()
        } else {
            last_line(&detail)
        },
    })
}

/// Reads ssh's stdout to its end, and its stderr keeping the last 4 KiB: a pipe nobody reads
/// would block it.
pub(crate) fn drain(proc: &mut Running) -> Arc<Mutex<Vec<u8>>> {
    let kept = Arc::new(Mutex::new(Vec::new()));
    if let Some(mut out) = proc.child.stdout.take() {
        tokio::spawn(async move {
            let mut buf = [0u8; 4096];
            while matches!(out.read(&mut buf).await, Ok(n) if n > 0) {}
        });
    }
    if let Some(mut err) = proc.child.stderr.take() {
        let kept = kept.clone();
        tokio::spawn(async move {
            let mut buf = [0u8; 4096];
            loop {
                let n = match err.read(&mut buf).await {
                    Ok(n) if n > 0 => n,
                    _ => break,
                };
                if let (Ok(mut kept), Some(chunk)) = (kept.lock(), buf.get(..n)) {
                    kept.extend_from_slice(chunk);
                    let excess = kept.len().saturating_sub(4096);
                    kept.drain(..excess);
                }
            }
        });
    }
    kept
}

/// Whether `path` is a socket (not following a link).
pub(crate) fn is_socket(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileTypeExt as _;
        std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_socket())
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        false
    }
}

/// Whether the user's config, for `host`, sets `ForkAfterAuthentication` at all: `ssh -G` prints
/// the option only where ssh knows it (OpenSSH 8.7 and newer), and only there may the link pass
/// it, as `no`. When `ssh -G` fails, `false`: the link then fails visibly if ssh forks.
async fn knows_fork_after_authentication(ssh: &Ssh, host: &str) -> bool {
    let mut command = ssh.command();
    command.args(["-G", "--", host]);
    match crate::ssh::drive(command, None, crate::RESOLVE_LIMITS, None).await {
        Ok((status, stdout, _)) if status.success() => {
            knows_fork(&String::from_utf8_lossy(&stdout))
        }
        _ => false,
    }
}

/// Whether `ssh -G`'s output names `ForkAfterAuthentication`.
fn knows_fork(resolved: &str) -> bool {
    resolved.lines().any(|line| {
        line.split_once(' ')
            .is_some_and(|(key, _)| key.eq_ignore_ascii_case("forkafterauthentication"))
    })
}

/// The link's arguments: `-N` and the options in the module docs. `fork`: ssh knows
/// `ForkAfterAuthentication` (see [`knows_fork_after_authentication`]).
pub(crate) fn link_args(
    spec: &LinkSpec<'_>,
    log: &Path,
    control: Option<&Path>,
    fork: bool,
) -> Result<Vec<String>, SshError> {
    let log = log.to_str().ok_or_else(|| {
        SshError::InvalidArgument("the runtime directory is not valid UTF-8".to_owned())
    })?;
    let mut args: Vec<String> = ["-N", "-T", "-E", log].map(str::to_owned).to_vec();
    let mut options: Vec<String> = [
        // Without a control socket to watch for, "Authenticated to" (a VERBOSE line) says ready.
        if control.is_some() {
            "LogLevel=INFO"
        } else {
            "LogLevel=VERBOSE"
        },
        "ForwardAgent=no",
        "ForwardX11=no",
        "PermitLocalCommand=no",
        "ClearAllForwardings=yes",
        "RemoteCommand=none",
        "StrictHostKeyChecking=ask",
        "ExitOnForwardFailure=yes",
    ]
    .map(str::to_owned)
    .to_vec();
    if fork {
        options.push("ForkAfterAuthentication=no".to_owned());
    }
    let count = if spec.patient {
        KEEPALIVE_COUNT_PATIENT
    } else {
        KEEPALIVE_COUNT
    };
    options.push(format!("ServerAliveInterval={KEEPALIVE_INTERVAL}"));
    options.push(format!("ServerAliveCountMax={count}"));
    options.push(format!(
        "ConnectTimeout={}",
        spec.ssh.connect_timeout_secs()
    ));
    if !spec.ssh.has_prompts() {
        options.push("BatchMode=yes".to_owned());
    }
    if let Some(control) = control {
        options.push("ControlMaster=yes".to_owned());
        options.push(crate::ssh::control_socket(control)?);
        options.push("ControlPersist=no".to_owned());
        // Forwards added later (`-O forward`) bind in the private directory, mode 0600.
        options.push("StreamLocalBindMask=0177".to_owned());
        options.push("StreamLocalBindUnlink=yes".to_owned());
    }
    if let Some(Via::Master { control, login }) = spec.via {
        options.push(proxy_through(spec.ssh.program(), control, login)?);
    }
    if spec.via.is_some() {
        // The node's name as the cluster gave it: not completed into another host's (a
        // `CanonicalDomains` of the user's would make it one of their own `Host`s).
        options.push("CanonicalizeHostname=no".to_owned());
    }
    for option in options {
        args.push("-o".to_owned());
        args.push(option);
    }
    if let Some(Via::Jump(login)) = spec.via {
        validate_host(login)?;
        args.push("-J".to_owned());
        args.push(login.to_owned());
    }
    args.push("--".to_owned());
    args.push(spec.host.to_owned());
    Ok(args)
}

/// `ProxyCommand=exec <ssh> … -W '[%h]:%p' -- <login>`: the node's connection as a channel of
/// the login link. ssh runs it with `$SHELL -c`, so the link sets `SHELL=/bin/sh` for it; every
/// part is quoted for sh and has its `%` doubled for ssh, apart from the `%h` and `%p` ssh fills
/// in.
pub(crate) fn proxy_through(
    program: &Path,
    control: &Path,
    login: &str,
) -> Result<String, SshError> {
    validate_host(login)?;
    let program = program.to_str().ok_or_else(|| {
        SshError::InvalidArgument("the ssh program's path is not valid UTF-8".to_owned())
    })?;
    let control = crate::ssh::control_socket(control)?;
    let word = |w: &str| crate::quote::sh_quote(w).replace('%', "%%");
    let mut words: Vec<String> = vec!["exec".to_owned(), word(program)];
    for w in [
        "-F",
        "none",
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
        "BatchMode=yes",
        "-o",
        "ControlMaster=no",
        "-o",
        "ProxyCommand=false",
        "-o",
        &control,
    ] {
        words.push(word(w));
    }
    words.push("-W".to_owned());
    words.push("'[%h]:%p'".to_owned());
    words.push("--".to_owned());
    words.push(word(login));
    Ok(format!("ProxyCommand={}", words.join(" ")))
}

/// Runs `ssh -O <op> [extra] -- <host>` against the master at `control`, reading no config.
///
/// # Errors
/// ssh failed, or said no (exit code and what it said).
#[cfg(unix)]
pub(crate) async fn control(
    ssh: &Ssh,
    dir: &Path,
    master: &Path,
    host: &str,
    op: &str,
    extra: &[String],
) -> Result<(), SshError> {
    validate_host(host)?;
    let log = SshLog::new(dir)?;
    let log_path = log.path().to_str().ok_or_else(|| {
        SshError::InvalidArgument("the runtime directory is not valid UTF-8".to_owned())
    })?;
    let mut args: Vec<String> = ["-F", "none", "-E", log_path, "-o", "LogLevel=ERROR", "-o"]
        .map(str::to_owned)
        .to_vec();
    args.push(crate::ssh::control_socket(master)?);
    args.push("-O".to_owned());
    args.push(op.to_owned());
    args.extend(extra.iter().cloned());
    args.push("--".to_owned());
    args.push(host.to_owned());
    let mut command = ssh.command();
    command.args(args);
    let limits = crate::Limits {
        max_output: Some(64 * 1024),
        timeout: Some(Duration::from_secs(10)),
    };
    let (status, _, stderr) = crate::ssh::drive(command, None, limits, None).await?;
    if status.success() {
        return Ok(());
    }
    let mut said = log.read();
    said.push_str(&String::from_utf8_lossy(&stderr));
    Err(SshError::Ssh {
        code: status.code().unwrap_or(-1),
        stderr: said,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec<'a>(ssh: &'a Ssh, dir: &'a Path, via: Option<Via<'a>>) -> LinkSpec<'a> {
        LinkSpec {
            ssh,
            dir,
            name: "login",
            host: "hpc-login",
            via,
            master: true,
            patient: false,
        }
    }

    fn option<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
        args.windows(2)
            .filter(|w| w[0] == "-o")
            .find_map(|w| w[1].strip_prefix(&format!("{name}=")))
    }

    /// Unix only: a link with a control socket (`ControlMaster`), and a node link through the
    /// login's master, exist only where ssh has connection reuse. Windows' OpenSSH has none:
    /// there the supervisor starts every link without a master, and a node link jumps through the
    /// login (`-J`), which the next test checks on every platform.
    #[cfg(unix)]
    #[test]
    fn links_keep_forwarding_off_and_notice_loss() {
        let ssh = Ssh::new("ssh");
        let dir = Path::new("/run/user/1000/pitcrew-ssh/t0123abcd");
        let control = dir.join("login");
        let args = link_args(
            &spec(&ssh, dir, None),
            &dir.join("log-1"),
            Some(&control),
            false,
        )
        .unwrap();
        assert_eq!(
            args[..4],
            [
                "-N",
                "-T",
                "-E",
                "/run/user/1000/pitcrew-ssh/t0123abcd/log-1"
            ]
        );
        for (name, value) in [
            ("ForwardAgent", "no"),
            ("ForwardX11", "no"),
            ("PermitLocalCommand", "no"),
            ("ClearAllForwardings", "yes"),
            ("RemoteCommand", "none"),
            ("StrictHostKeyChecking", "ask"),
            ("BatchMode", "yes"),
            ("ControlMaster", "yes"),
            ("ControlPersist", "no"),
            ("StreamLocalBindMask", "0177"),
            ("ServerAliveInterval", "2"),
            ("ServerAliveCountMax", "3"),
        ] {
            assert_eq!(option(&args, name), Some(value), "{name}");
        }
        assert_eq!(
            option(&args, "ControlPath"),
            Some("/run/user/1000/pitcrew-ssh/t0123abcd/login")
        );
        assert_eq!(args[args.len() - 2..], ["--", "hpc-login"]);
        assert_eq!(option(&args, "ForkAfterAuthentication"), None);
        // Patient, where ssh knows ForkAfterAuthentication.
        let patient = LinkSpec {
            patient: true,
            ..spec(&ssh, dir, None)
        };
        let args = link_args(&patient, &dir.join("log-1"), Some(&control), true).unwrap();
        assert_eq!(option(&args, "ServerAliveCountMax"), Some("14"));
        assert_eq!(option(&args, "ForkAfterAuthentication"), Some("no"));

        // Through the login link: the hop is a client of its master, never a login.
        let login_control = dir.join("login");
        let args = link_args(
            &spec(
                &ssh,
                dir,
                Some(Via::Master {
                    control: &login_control,
                    login: "hpc-login",
                }),
            ),
            &dir.join("log-2"),
            Some(&dir.join("node")),
            false,
        )
        .unwrap();
        assert_eq!(
            option(&args, "ProxyCommand"),
            Some(
                "exec ssh -F none -T -o 'ForwardAgent=no' -o 'ForwardX11=no' -o \
                 'PermitLocalCommand=no' -o 'ClearAllForwardings=yes' -o 'BatchMode=yes' -o \
                 'ControlMaster=no' -o 'ProxyCommand=false' -o \
                 'ControlPath=/run/user/1000/pitcrew-ssh/t0123abcd/login' -W '[%h]:%p' -- \
                 hpc-login"
            )
        );
    }

    /// Without a master (always on Windows): no control socket, readiness from the log's
    /// "Authenticated to", forwarding still off, and a node reached with ssh's own jump.
    #[test]
    fn without_a_master_a_node_link_jumps_through_the_login() {
        let ssh = Ssh::new("ssh");
        // A runtime directory in the platform's form (made up; nothing is created).
        let dir = if cfg!(windows) {
            PathBuf::from(r"C:\Temp\pitcrew-ssh\t0123abcd")
        } else {
            PathBuf::from("/run/user/1000/pitcrew-ssh/t0123abcd")
        };
        let log = dir.join("log-3");
        let args = link_args(
            &LinkSpec {
                master: false,
                ..spec(&ssh, &dir, Some(Via::Jump("hpc-login")))
            },
            &log,
            None,
            false,
        )
        .unwrap();
        assert_eq!(args[..4], ["-N", "-T", "-E", log.to_str().unwrap()]);
        for (name, value) in [
            ("LogLevel", "VERBOSE"),
            ("ForwardAgent", "no"),
            ("ForwardX11", "no"),
            ("PermitLocalCommand", "no"),
            ("ClearAllForwardings", "yes"),
            ("RemoteCommand", "none"),
            ("CanonicalizeHostname", "no"),
        ] {
            assert_eq!(option(&args, name), Some(value), "{name}");
        }
        for name in [
            "ControlMaster",
            "ControlPath",
            "ControlPersist",
            "ProxyCommand",
        ] {
            assert_eq!(option(&args, name), None, "{name}");
        }
        let at = args.iter().position(|a| a == "-J").unwrap();
        assert_eq!(args[at + 1], "hpc-login");
        assert_eq!(args[args.len() - 2..], ["--", "hpc-login"]);
    }

    /// Unix only, as a node link through the login's master is (see above).
    #[cfg(unix)]
    #[test]
    fn the_proxy_command_quotes_for_sh_and_escapes_for_ssh() {
        let command = proxy_through(
            Path::new("/opt/My Tools/ssh%1"),
            Path::new("/tmp/pc/login"),
            "sam@hpc-login",
        )
        .unwrap();
        assert!(
            command.starts_with("ProxyCommand=exec '/opt/My Tools/ssh%%1' -F none"),
            "{command}"
        );
        assert!(
            command.ends_with("-W '[%h]:%p' -- sam@hpc-login"),
            "{command}"
        );
        // An IPv6 scope is a % too, and brackets are quoted.
        let command = proxy_through(
            Path::new("ssh"),
            Path::new("/tmp/pc/login"),
            "[fe80::1%eth0]",
        )
        .unwrap();
        assert!(command.ends_with("-- '[fe80::1%%eth0]'"), "{command}");
        for bad in ["-oProxyCommand=x", "a b", "a;b"] {
            assert!(proxy_through(Path::new("ssh"), Path::new("/tmp/pc/login"), bad).is_err());
        }
        // A control path ssh would expand.
        assert!(proxy_through(Path::new("ssh"), Path::new("/tmp/%d/login"), "h").is_err());
    }

    #[test]
    fn fork_after_authentication_is_read_from_ssh_g() {
        assert!(knows_fork("hostname h\nforkafterauthentication yes\n"));
        assert!(knows_fork("ForkAfterAuthentication no\n"));
        assert!(!knows_fork("hostname h\nport 22\n"));
    }

    #[test]
    fn failures_come_from_the_log() {
        let err = failure(
            "someone@hpc-login: Permission denied (publickey,password).\n",
            "",
            Some(255),
        );
        assert!(matches!(err, SshError::AuthFailed { .. }), "{err:?}");
        let err = failure("", "", Some(255));
        assert!(matches!(err, SshError::Ssh { code: 255, .. }), "{err:?}");
    }
}
