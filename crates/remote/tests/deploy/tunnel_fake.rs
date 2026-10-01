//! The fake `ssh`'s side of the tunnel, the fake daemon's connections, and a crashing app.
//!
//! The fake `ssh` (`deploy.rs`) also plays the tunnel's calls, as OpenSSH does them:
//! - `ssh -G -- <host>`: what ssh resolves; it says the user's config sets
//!   `ForkAfterAuthentication yes`, so a link must turn it off.
//! - `ssh -N … -- <host>`, a link. With a `ControlPath` it is a ControlMaster: it "connects"
//!   (or fails, if the network is down), asks for a password through `SSH_ASKPASS` where the
//!   machine wants one, listens on its control socket, and serves its clients: `check`, `exit`,
//!   `forward` (it then listens on the local socket and relays each connection to the daemon's
//!   socket, or logs "open failed: administratively prohibited" and closes it where the machine
//!   forbids forwarding), `session` channels (refused beyond the machine's `MaxSessions`) and
//!   `stdio` channels (`-W`, not sessions). Without one it is a heartbeat. With a `ProxyCommand`
//!   it first runs it, as ssh does, and ends when it ends. Its keepalives time out as ssh's
//!   would: after `(ServerAliveCountMax + 1) × ServerAliveInterval` seconds of a down network.
//! - `ssh -O <op> -o ControlPath=<p> -- <host>`: asks the link.
//! - `ssh -o ControlMaster=no -o ControlPath=<p> … -- <host> <command>`: a session of the link
//!   running the command on the machine (like the plain fake: `/bin/sh -c`, in its home, with
//!   its `PATH`), stdin and stdout relayed with their ends; with no link there, or a session
//!   refused, it fails the way ssh then does. `-W` is a channel the `ProxyCommand` uses.
//! - Any other call to `cluster` without `BatchMode` (a login of its own, as without connection
//!   reuse) asks for the machine's password first, if it has one; then the plain fake plays it.
//!
//! It refuses (exit 255) a tunnel call without the options PitCrew must pass: agent and X11
//! forwarding and local commands off; for a link also the configured forwardings cleared, host
//! keys asked about, keepalives set and `ForkAfterAuthentication=no` (and for a node's
//! `ProxyCommand`, `SHELL=/bin/sh` and `CanonicalizeHostname=no`); for a session no login of its
//! own and `EscapeChar=none`.
//!
//! The machine's state is in files beside it: `net` (`down`: keepalives time out and new
//! connections fail; `frozen`: nothing answers and nothing times out, as for a laptop asleep;
//! absent: up), `no-forwarding` (`AllowStreamLocalForwarding no`; each refusal is counted in
//! `refused.log`), `forward-fail-once` (the next forward request fails), `forward-silent`
//! (forwarded connections are taken and never answered), `max-sessions`
//! (sshd's `MaxSessions`), `drop-after` (seconds a link lasts), `password` (logins to `cluster`
//! ask for it; each ask is logged to `asked.log` as `text` or `empty`, never the answer). Each
//! tunnel call is logged to `tunnel.log`.
//!
//! The fake daemon (`pitcrewd serve`) echoes every connection, half-closes like it, answers
//! `GET` with HTTP, and on `close-write\n` says `closing\n`, half-closes, and appends what it
//! still receives to `<socket>.got`.

use crate::unix::{Remote, decode};
use pitcrew_remote::{
    Connector, ConnectorOptions, Daemon, DirectLauncher, Layout, LinkState, Platform, Ssh, Target,
};
use serde::{Deserialize, Serialize};
use std::io::{BufRead as _, BufReader, Read, Write};
use std::net::Shutdown;
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

pub(crate) const TUNNEL_LOG: &str = "tunnel.log";
pub(crate) const ASKED: &str = "asked.log";
pub(crate) const NET: &str = "net";
pub(crate) const NO_FORWARDING: &str = "no-forwarding";
/// One line per forwarded channel the machine refused.
pub(crate) const REFUSED: &str = "refused.log";
pub(crate) const FORWARD_FAIL_ONCE: &str = "forward-fail-once";
pub(crate) const FORWARD_SILENT: &str = "forward-silent";
pub(crate) const MAX_SESSIONS: &str = "max-sessions";
pub(crate) const DROP_AFTER: &str = "drop-after";
pub(crate) const PASSWORD: &str = "password";
/// What the fake daemon reads to half-close first.
pub(crate) const CLOSE_WRITE: &[u8] = b"close-write\n";
/// Makes the binary play an app that starts a connector and dies (see [`act_as_app`]).
pub(crate) const APP_ENV: &str = "PITCREW_FAKE_TUNNEL_APP";

// ─── The fake ssh ──────────────────────────────────────────────────────────────────────────

/// One call's arguments, as ssh reads them.
#[derive(Debug, Default)]
struct Call {
    options: Vec<(String, String)>,
    flags: Vec<String>,
    config: Option<String>,
    log: Option<PathBuf>,
    op: Option<String>,
    forward: Option<String>,
    stdio: Option<String>,
    host: String,
    command: Option<String>,
}

impl Call {
    fn parse(args: &[String]) -> Self {
        let mut call = Self::default();
        let mut args = args.iter();
        while let Some(arg) = args.next() {
            let mut value = || args.next().cloned().unwrap_or_default();
            match arg.as_str() {
                "-o" => {
                    let option = value();
                    let (key, val) = option.split_once('=').unwrap_or((&option, ""));
                    call.options.push((key.to_owned(), val.to_owned()));
                }
                "-F" => call.config = Some(value()),
                "-E" => call.log = Some(PathBuf::from(value())),
                "-O" => call.op = Some(value()),
                "-L" => call.forward = Some(value()),
                "-W" => call.stdio = Some(value()),
                "-J" => {
                    let _ = value();
                }
                "--" => {
                    call.host = value();
                    call.command = args.next().cloned();
                    break;
                }
                flag => call.flags.push(flag.to_owned()),
            }
        }
        call
    }

    /// An option's value, the first given winning, as ssh's.
    fn option(&self, name: &str) -> Option<&str> {
        self.options
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    fn flag(&self, flag: &str) -> bool {
        self.flags.iter().any(|f| f == flag)
    }

    fn kind(&self) -> Option<&'static str> {
        if self.flag("-G") {
            Some("resolve")
        } else if self.op.is_some() {
            Some("control")
        } else if self.flag("-N") {
            Some("link")
        } else if self.stdio.is_some() {
            Some("stdio")
        } else if self.option("ControlMaster") == Some("no") {
            Some("session")
        } else {
            None
        }
    }

    /// What ssh logs (`-E`), from any thread.
    fn say(&self, line: &str) {
        say(self.log.as_deref(), line);
    }
}

fn say(log: Option<&Path>, line: &str) {
    match log {
        Some(path) => {
            if let Ok(mut file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
            {
                let _ = writeln!(file, "{line}");
            }
        }
        None => eprintln!("{line}"),
    }
}

/// One line of `tunnel.log`.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct TunnelCall {
    pub(crate) pid: u32,
    pub(crate) kind: String,
    pub(crate) host: String,
    pub(crate) op: Option<String>,
    /// For a session, the command line, unwrapped.
    pub(crate) line: Option<String>,
    pub(crate) proxy: Option<String>,
    pub(crate) args: Vec<String>,
}

/// The machine's network.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Net {
    Up,
    Down,
    Frozen,
}

fn net(machine: &Path) -> Net {
    match std::fs::read_to_string(machine.join(NET)).as_deref() {
        Ok("down") => Net::Down,
        Ok("frozen") => Net::Frozen,
        _ => Net::Up,
    }
}

/// Waits for the network to be up (for ever: a link's keepalives end the process).
fn wait_up(machine: &Path) {
    while net(machine) != Net::Up {
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// A number in a file of the machine's.
fn number(machine: &Path, file: &str) -> Option<u64> {
    std::fs::read_to_string(machine.join(file))
        .ok()
        .and_then(|n| n.trim().parse().ok())
}

/// Plays the tunnel's calls; `None` for any other call (the plain fake plays it).
pub(crate) fn fake(remote: &Remote, dir: &Path, args: &[String]) -> Option<u8> {
    let call = Call::parse(args);
    let machine = dir.parent()?.to_path_buf();
    let Some(kind) = call.kind() else {
        return plain_login(&machine, &call);
    };
    log_call(&machine, &call, kind);
    if let Err(why) = check_options(&call, kind) {
        call.say(&format!("fake ssh: refused: {why}"));
        return Some(255);
    }
    Some(match kind {
        "resolve" => {
            println!(
                "hostname {}\nport 22\nforkafterauthentication yes",
                call.host
            );
            0
        }
        "control" => control_op(&call),
        "link" => link(&machine, &call),
        "stdio" => stdio_forward(&call),
        _ => session(remote, &call),
    })
}

/// A call that logs in by itself (no connection reuse): to `cluster`, asks for the machine's
/// password first where there is one and the call may prompt. `None`: go on as the plain fake.
fn plain_login(machine: &Path, call: &Call) -> Option<u8> {
    if call.host != "cluster" || call.option("BatchMode") == Some("yes") {
        return None;
    }
    let expected = std::fs::read_to_string(machine.join(PASSWORD)).ok()?;
    log_call(machine, call, "login");
    if sign_in(machine, &call.host, expected.trim_end()) {
        None
    } else {
        call.say("someone@cluster: Permission denied (publickey,password).");
        Some(255)
    }
}

fn log_call(machine: &Path, call: &Call, kind: &str) {
    let entry = TunnelCall {
        pid: std::process::id(),
        kind: kind.to_owned(),
        host: call.host.clone(),
        op: call.op.clone(),
        line: call.command.as_deref().and_then(decode),
        proxy: call.option("ProxyCommand").map(str::to_owned),
        args: std::env::args().skip(1).collect(),
    };
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(machine.join(TUNNEL_LOG))
    {
        let _ = writeln!(file, "{}", serde_json::to_string(&entry).unwrap());
    }
}

/// The options PitCrew must pass, by kind of call.
fn check_options(call: &Call, kind: &str) -> Result<(), String> {
    let want = |name: &str, value: &str| {
        if call
            .option(name)
            .is_some_and(|v| v.eq_ignore_ascii_case(value))
        {
            Ok(())
        } else {
            Err(format!("{name}={value} is missing"))
        }
    };
    let no_config = || {
        if call.config.as_deref() == Some("none") {
            Ok(())
        } else {
            Err("-F none is missing".to_owned())
        }
    };
    match kind {
        "resolve" => return Ok(()),
        "control" => return no_config(),
        _ => {}
    }
    want("ForwardAgent", "no")?;
    want("ForwardX11", "no")?;
    want("PermitLocalCommand", "no")?;
    match kind {
        "link" => {
            want("ClearAllForwardings", "yes")?;
            want("StrictHostKeyChecking", "ask")?;
            // ssh -G said the user's config forks after authentication.
            want("ForkAfterAuthentication", "no")?;
            if call.option("ControlPath").is_some() {
                want("ControlMaster", "yes")?;
                want("ControlPersist", "no")?;
            }
            if call.option("ProxyCommand").is_some() {
                if std::env::var("SHELL").ok().as_deref() != Some("/bin/sh") {
                    return Err("a ProxyCommand for sh, but SHELL is not /bin/sh".to_owned());
                }
                // A node's name, as the cluster gave it.
                want("CanonicalizeHostname", "no")?;
            }
            for keepalive in ["ServerAliveInterval", "ServerAliveCountMax"] {
                if call
                    .option(keepalive)
                    .and_then(|v| v.parse::<u64>().ok())
                    .is_none()
                {
                    return Err(format!("{keepalive} is missing"));
                }
            }
        }
        "stdio" => {
            want("ControlMaster", "no")?;
            want("ProxyCommand", "false")?;
            want("BatchMode", "yes")?;
            no_config()?;
        }
        _ => {
            want("ControlMaster", "no")?;
            want("ProxyCommand", "false")?;
            want("BatchMode", "yes")?;
            want("EscapeChar", "none")?;
            no_config()?;
        }
    }
    Ok(())
}

/// Connects to the link at the call's `ControlPath`.
fn to_master(call: &Call) -> std::io::Result<UnixStream> {
    let path = call
        .option("ControlPath")
        .ok_or_else(|| std::io::Error::other("no ControlPath"))?;
    UnixStream::connect(path)
}

/// Sends one request to the link and reads its answer line (`None` at end of file).
fn ask_master(stream: &mut UnixStream, request: &str) -> Option<String> {
    stream.write_all(format!("{request}\n").as_bytes()).ok()?;
    let mut line = String::new();
    let mut reader = BufReader::new(stream.try_clone().ok()?);
    match reader.read_line(&mut line) {
        Ok(n) if n > 0 => Some(line.trim_end().to_owned()),
        _ => None,
    }
}

/// `ssh -O <op>`.
fn control_op(call: &Call) -> u8 {
    let Ok(mut master) = to_master(call) else {
        eprintln!(
            "Control socket connect({}): No such file or directory",
            call.option("ControlPath").unwrap_or("")
        );
        return 255;
    };
    let request = match (call.op.as_deref(), &call.forward) {
        (Some("forward"), Some(spec)) => {
            let (local, remote) = spec.split_once(':').unwrap_or((spec, ""));
            format!("forward {local} {remote}")
        }
        (Some(op), _) => op.to_owned(),
        (None, _) => return 255,
    };
    match ask_master(&mut master, &request) {
        Some(answer) if answer.starts_with("ok") => {
            if call.op.as_deref() == Some("check") {
                eprintln!("Master running (pid={})", answer.trim_start_matches("ok "));
            }
            0
        }
        Some(answer) => {
            eprintln!("{answer}");
            255
        }
        None => 255,
    }
}

/// `ssh -N`: a link, as a ControlMaster or a heartbeat.
fn link(machine: &Path, call: &Call) -> u8 {
    let host = call.host.clone();
    if net(machine) != Net::Up {
        std::thread::sleep(Duration::from_millis(200));
        call.say(&format!(
            "ssh: connect to host {host} port 22: Network is unreachable"
        ));
        return 255;
    }
    // A node through the login link: the ProxyCommand carries the connection.
    let lost = Arc::new(AtomicBool::new(false));
    let mut carrier = None;
    if let Some(proxy) = call.option("ProxyCommand") {
        let command = proxy
            .replace("%%", "\u{0}")
            .replace("%h", &host)
            .replace("%p", "22")
            .replace('\u{0}', "%");
        let mut child = Command::new("/bin/sh")
            .arg("-c")
            .arg(&command)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (banner, got) = std::sync::mpsc::channel();
        let gone = lost.clone();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();
            let ok = reader.read_line(&mut line).is_ok() && line.starts_with("SSH-2.0-");
            let _ = banner.send(ok);
            // The connection lasts as long as the command's output.
            let _ = std::io::copy(&mut reader, &mut std::io::sink());
            gone.store(true, Ordering::SeqCst);
        });
        if got.recv_timeout(Duration::from_secs(10)) != Ok(true) {
            call.say("kex_exchange_identification: Connection closed by remote host");
            let _ = child.kill();
            let _ = child.wait();
            return 255;
        }
        carrier = Some(child);
    }
    if host == "cluster"
        && let Ok(expected) = std::fs::read_to_string(machine.join(PASSWORD))
        && !sign_in(machine, &host, expected.trim_end())
    {
        call.say(&format!(
            "someone@{host}: Permission denied (publickey,password)."
        ));
        return 255;
    }
    call.say(&format!(
        "Authenticated to {host} ([192.0.2.10]:22) using \"publickey\"."
    ));
    let exit = Arc::new(AtomicBool::new(false));
    let control = call.option("ControlPath").map(PathBuf::from);
    if let Some(control) = &control {
        let _ = std::fs::remove_file(control);
        let listener = UnixListener::bind(control).unwrap();
        let mask = call
            .option("StreamLocalBindMask")
            .and_then(|m| u32::from_str_radix(m, 8).ok())
            .unwrap_or(0o177);
        let machine = machine.to_path_buf();
        let log = call.log.clone();
        let exit = exit.clone();
        let sessions = Arc::new(AtomicU32::new(0));
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let machine = machine.clone();
                let log = log.clone();
                let exit = exit.clone();
                let sessions = sessions.clone();
                std::thread::spawn(move || {
                    serve_mux(stream, &machine, log.as_deref(), mask, &exit, &sessions);
                });
            }
        });
    }
    let interval: u64 = call
        .option("ServerAliveInterval")
        .and_then(|v| v.parse().ok())
        .unwrap();
    let count: u64 = call
        .option("ServerAliveCountMax")
        .and_then(|v| v.parse().ok())
        .unwrap();
    let silence = Duration::from_secs((count + 1) * interval);
    let lasts = number(machine, DROP_AFTER).map(Duration::from_secs);
    let started = Instant::now();
    let mut down_since: Option<Instant> = None;
    let code = loop {
        std::thread::sleep(Duration::from_millis(100));
        if lost.load(Ordering::SeqCst) {
            call.say("Connection closed by UNKNOWN port 65535");
            break 255;
        }
        if exit.load(Ordering::SeqCst) {
            break 0;
        }
        if lasts.is_some_and(|l| started.elapsed() >= l) {
            call.say(&format!("Connection to {host} closed by remote host."));
            break 255;
        }
        match net(machine) {
            Net::Down => {
                if down_since.get_or_insert_with(Instant::now).elapsed() >= silence {
                    call.say(&format!("Timeout, server {host} not responding."));
                    break 255;
                }
            }
            // Asleep: nothing answers, and nothing notices.
            Net::Up | Net::Frozen => down_since = None,
        }
    };
    if let Some(control) = &control {
        let _ = std::fs::remove_file(control);
    }
    if let Some(mut child) = carrier {
        let _ = child.kill();
        let _ = child.wait();
    }
    code
}

/// Asks for the password through askpass, as ssh does: three tries, each logged as `text` or
/// `empty` (never the answer).
fn sign_in(machine: &Path, host: &str, expected: &str) -> bool {
    let Ok(program) = std::env::var("SSH_ASKPASS") else {
        return false;
    };
    for _ in 0..3 {
        let out = Command::new(&program)
            .arg(format!("someone@{host}'s password: "))
            .output();
        let answer = out
            .ok()
            .filter(|o| o.status.success())
            .map(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .trim_end_matches('\n')
                    .to_owned()
            })
            .unwrap_or_default();
        let mut asked = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(machine.join(ASKED))
            .unwrap();
        writeln!(
            asked,
            "{}",
            if answer.is_empty() { "empty" } else { "text" }
        )
        .unwrap();
        if answer == expected {
            return true;
        }
    }
    false
}

/// One client of a link.
fn serve_mux(
    mut stream: UnixStream,
    machine: &Path,
    log: Option<&Path>,
    mask: u32,
    exit: &AtomicBool,
    sessions: &AtomicU32,
) {
    let mut line = String::new();
    if BufReader::new(stream.try_clone().unwrap())
        .read_line(&mut line)
        .is_err()
    {
        return;
    }
    let words: Vec<&str> = line.split_whitespace().collect();
    match words.as_slice() {
        ["check"] => {
            let _ = writeln!(stream, "ok {}", std::process::id());
        }
        ["exit"] => {
            exit.store(true, Ordering::SeqCst);
            let _ = writeln!(stream, "ok");
        }
        ["forward", local, remote] => {
            let fail_once = machine.join(FORWARD_FAIL_ONCE);
            if fail_once.exists() {
                let _ = std::fs::remove_file(fail_once);
                say(log, "fake ssh: the forward failed, this once");
                let _ = writeln!(stream, "fail the forward failed, this once");
                return;
            }
            match listen_forward(local, remote, machine, log.map(Path::to_path_buf), mask) {
                Ok(()) => {
                    let _ = writeln!(stream, "ok");
                }
                Err(e) => {
                    say(log, &format!("fake ssh: forward failed: {e}"));
                    let _ = writeln!(stream, "fail {e}");
                }
            }
        }
        ["session", ..] => {
            // A channel opens only while the network answers; a session only within
            // MaxSessions.
            wait_up(machine);
            let max = number(machine, MAX_SESSIONS).unwrap_or(10);
            let open = sessions.fetch_add(1, Ordering::SeqCst);
            if u64::from(open) >= max {
                sessions.fetch_sub(1, Ordering::SeqCst);
                let _ = writeln!(stream, "fail Session open refused by peer");
                return;
            }
            let _ = writeln!(stream, "ok");
            // Held until the client goes.
            let _ = std::io::copy(&mut stream, &mut std::io::sink());
            sessions.fetch_sub(1, Ordering::SeqCst);
        }
        ["stdio", ..] => {
            wait_up(machine);
            let _ = writeln!(stream, "ok");
            let _ = std::io::copy(&mut stream, &mut std::io::sink());
        }
        _ => {
            let _ = writeln!(stream, "fail unknown request");
        }
    }
}

/// The master's listener for a forward: each connection is a channel to `remote`.
fn listen_forward(
    local: &str,
    remote: &str,
    machine: &Path,
    log: Option<PathBuf>,
    mask: u32,
) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    // StreamLocalBindUnlink=yes.
    let _ = std::fs::remove_file(local);
    let listener = UnixListener::bind(local)?;
    std::fs::set_permissions(local, std::fs::Permissions::from_mode(0o666 & !mask))?;
    let machine = machine.to_path_buf();
    let remote = remote.to_owned();
    std::thread::spawn(move || {
        for client in listener.incoming().flatten() {
            let machine = machine.clone();
            let remote = remote.clone();
            let log = log.clone();
            std::thread::spawn(move || {
                if machine.join(NO_FORWARDING).exists() {
                    say(
                        log.as_deref(),
                        "channel 3: open failed: administratively prohibited: open failed",
                    );
                    // Counted, for the cases.
                    if let Ok(mut refused) = std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(machine.join(REFUSED))
                    {
                        let _ = writeln!(refused, "refused");
                    }
                    drop(client);
                    return;
                }
                if machine.join(FORWARD_SILENT).exists() {
                    // Taken, never answered, until the client goes.
                    let mut client = client;
                    let _ = std::io::copy(&mut client, &mut std::io::sink());
                    return;
                }
                wait_up(&machine);
                match UnixStream::connect(&remote) {
                    Ok(daemon) => relay(client, daemon),
                    Err(_) => {
                        say(
                            log.as_deref(),
                            "channel 3: open failed: connect failed: No such file or directory",
                        );
                        drop(client);
                    }
                }
            });
        }
    });
    Ok(())
}

/// Copies both ways, passing each end of file on as a half-close.
fn relay(a: UnixStream, b: UnixStream) {
    let (mut a_in, mut b_out) = (a.try_clone().unwrap(), b.try_clone().unwrap());
    let up = std::thread::spawn(move || {
        let _ = std::io::copy(&mut a_in, &mut b_out);
        let _ = b_out.shutdown(Shutdown::Write);
    });
    let (mut b_in, mut a_out) = (b, a);
    let _ = std::io::copy(&mut b_in, &mut a_out);
    let _ = a_out.shutdown(Shutdown::Write);
    let _ = up.join();
}

/// `ssh -W`: a channel the `ProxyCommand` carries a node's connection on.
fn stdio_forward(call: &Call) -> u8 {
    let Ok(mut master) = to_master(call) else {
        eprintln!("Control socket connect: No such file or directory");
        return 255;
    };
    if ask_master(
        &mut master,
        &format!("stdio {}", call.stdio.as_deref().unwrap_or("")),
    )
    .is_none_or(|a| a != "ok")
    {
        return 255;
    }
    println!("SSH-2.0-fake");
    std::io::stdout().flush().unwrap();
    // The node's connection ends (the client stops reading) or the login link does.
    std::thread::spawn(|| {
        let _ = std::io::copy(&mut std::io::stdin(), &mut std::io::sink());
        std::process::exit(0);
    });
    let _ = std::io::copy(&mut master, &mut std::io::sink());
    255
}

/// A session of a link, running a command on the machine.
fn session(remote: &Remote, call: &Call) -> u8 {
    let Ok(mut master) = to_master(call) else {
        // ProxyCommand=false: ssh would have logged in itself; it fails instead.
        call.say("kex_exchange_identification: Connection closed by remote host");
        return 255;
    };
    match ask_master(&mut master, "session").as_deref() {
        Some("ok") => {}
        Some(_) => {
            // As ssh: the mux client's failure, then its own login, which ProxyCommand=false
            // makes fail.
            call.say(
                "mux_client_request_session: session request failed: Session open refused by \
                 peer",
            );
            call.say("kex_exchange_identification: Connection closed by remote host");
            return 255;
        }
        None => {
            call.say("mux_client_request_session: read from master failed: Broken pipe");
            return 255;
        }
    }
    let Some(command) = &call.command else {
        return 255;
    };
    // As the plain fake: another shell as the machine's `sh` runs the command line itself
    // (the wrapper is `login_shells.rs`'s).
    let (shell, script) = match (&remote.interpreter, decode(command)) {
        (Some(sh), Some(line)) => (
            sh.clone(),
            line.replacen("'/bin/sh' -c", &format!("'{sh}' -c"), 1),
        ),
        _ => ("/bin/sh".to_owned(), command.clone()),
    };
    let mut child = Command::new(&shell)
        .arg("-c")
        .arg(&script)
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
    let pid = child.id();
    // The link going away ends the channel.
    {
        let log = call.log.clone();
        std::thread::spawn(move || {
            let _ = std::io::copy(&mut master, &mut std::io::sink());
            if let Some(group) = i32::try_from(pid)
                .ok()
                .and_then(rustix::process::Pid::from_raw)
            {
                let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
            }
            say(
                log.as_deref(),
                "mux_client_read_packet: read header failed: Broken pipe",
            );
            std::process::exit(255);
        });
    }
    let mut to_remote = child.stdin.take().unwrap();
    std::thread::spawn(move || {
        let _ = std::io::copy(&mut std::io::stdin(), &mut to_remote);
    });
    let mut from_remote = child.stdout.take().unwrap();
    let out = std::thread::spawn(move || {
        let mut out = std::io::stdout();
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            match from_remote.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if out.write_all(&buf[..n]).and_then(|()| out.flush()).is_err() {
                        break;
                    }
                }
            }
        }
        // The command's end of output, passed on.
        if let Ok(null) = std::fs::OpenOptions::new().write(true).open("/dev/null") {
            let _ = rustix::stdio::dup2_stdout(&null);
        }
    });
    let mut errors = child.stderr.take().unwrap();
    let err = std::thread::spawn(move || {
        let _ = std::io::copy(&mut errors, &mut std::io::stderr());
    });
    let status = child.wait().unwrap();
    let _ = out.join();
    let _ = err.join();
    status
        .code()
        .and_then(|c| u8::try_from(c).ok())
        .unwrap_or(255)
}

// ─── The fake daemon's connections ─────────────────────────────────────────────────────────

/// See the module docs.
pub(crate) fn serve_connection(mut stream: UnixStream, socket: &Path) {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while head.len() < CLOSE_WRITE.len() {
        match stream.read(&mut byte) {
            Ok(1) => head.push(byte[0]),
            _ => break,
        }
        // A short message (no newline) is echoed at once.
        if head.len() >= 4 && !CLOSE_WRITE.starts_with(&head) && !head.starts_with(b"GET ") {
            break;
        }
    }
    if head.starts_with(b"GET ") {
        let mut request = head;
        while !request.ends_with(b"\r\n\r\n") {
            match stream.read(&mut byte) {
                Ok(1) => request.push(byte[0]),
                _ => break,
            }
        }
        let _ = stream.write_all(
            b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 2\r\n\
              connection: close\r\n\r\n{}",
        );
        return;
    }
    if head == CLOSE_WRITE {
        let _ = stream.write_all(b"closing\n");
        let _ = stream.shutdown(Shutdown::Write);
        let mut rest = Vec::new();
        let _ = stream.read_to_end(&mut rest);
        let got = PathBuf::from(format!("{}.got", socket.display()));
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(got)
        {
            let _ = file.write_all(&rest);
        }
        return;
    }
    if stream.write_all(&head).is_err() {
        return;
    }
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        match stream.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if stream.write_all(&buf[..n]).is_err() {
                    return;
                }
            }
        }
    }
    let _ = stream.shutdown(Shutdown::Write);
}

// ─── An app that dies ──────────────────────────────────────────────────────────────────────

/// What [`act_as_app`] connects to.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct AppSpec {
    pub(crate) ssh: PathBuf,
    pub(crate) runtime_dir: PathBuf,
    pub(crate) root: String,
    pub(crate) tool_path: String,
}

/// Starts a connector as `spec` says, waits until it is connected, and dies at once, without
/// running a destructor: as a crash would, it leaves its link running.
pub(crate) fn act_as_app(spec: &Path) -> ExitCode {
    let spec: AppSpec = serde_json::from_slice(&std::fs::read(spec).unwrap()).unwrap();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let ssh = Ssh::new(&spec.ssh)
        .with_runtime_dir(&spec.runtime_dir)
        .with_multiplex(true);
    let target = Target::with_layout(
        ssh,
        "cluster",
        Layout::at(&spec.root).unwrap(),
        Platform::LinuxX86_64,
    )
    .unwrap()
    .with_tool_path(&spec.tool_path)
    .unwrap();
    let connected = rt.block_on(async {
        let connector = Connector::start(
            Daemon::new(target, Arc::new(DirectLauncher::default())),
            ConnectorOptions {
                ssh_config: Some(PathBuf::from("/nonexistent/pitcrew-test-ssh-config")),
                ..ConnectorOptions::default()
            },
        )
        .unwrap();
        let mut state = connector.watch();
        let reached = tokio::time::timeout(
            Duration::from_secs(30),
            state.wait_for(|s| s.is_connected() || matches!(s, LinkState::Unreachable { .. })),
        )
        .await;
        let connected = matches!(reached, Ok(Ok(s)) if s.is_connected());
        std::mem::forget(connector);
        connected
    });
    // No destructor runs: the link and the connector's directory stay, as after a crash.
    std::process::exit(if connected { 0 } else { 1 })
}
