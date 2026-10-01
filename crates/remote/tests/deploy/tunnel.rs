//! The tunnel ([`Connector`]) and the stdio bridge, against `deploy.rs`'s fake machine (with
//! `slurm.rs`'s fake SLURM for jobs).
//!
//! The fake `ssh` also plays the tunnel's calls, as OpenSSH does them:
//! - `ssh -N … -o ControlMaster=yes -o ControlPath=<p> -- <host>`, a link: it "connects" (or
//!   fails, if the network is down), asks for a password through `SSH_ASKPASS` where the machine
//!   wants one, listens on `<p>`, and serves its clients: `check`, `forward` (it then listens on
//!   the local socket and relays each connection to the daemon's socket, or logs "open failed:
//!   administratively prohibited" and closes it where the machine forbids forwarding),
//!   `session` and `stdio` channels. With a `ProxyCommand` it first runs it, as ssh does, and
//!   ends when it ends. Its keepalives time out as ssh's would: after
//!   `(ServerAliveCountMax + 1) × ServerAliveInterval` seconds of a down network.
//! - `ssh -O <op> -o ControlPath=<p> -- <host>`: asks the link.
//! - `ssh -o ControlMaster=no -o ControlPath=<p> … -- <host> <command>`: a channel of the link
//!   running the command on the machine (like the plain fake: `/bin/sh -c`, in its home, with its
//!   `PATH`), stdin and stdout relayed with their ends; with no link there it fails the way
//!   `ProxyCommand=false` makes ssh fail. `-W` is a channel the `ProxyCommand` uses.
//!
//! It refuses (exit 255) a tunnel call without the options PitCrew must pass: agent and X11
//! forwarding and local commands off; for a link also the configured forwardings cleared,
//! host keys asked about and keepalives set; for a channel no login of its own.
//!
//! The machine's state is in files beside it: `net` (`down`: keepalives time out and new
//! connections fail; `frozen`: nothing answers and nothing times out, as for a laptop asleep;
//! absent: up), `no-forwarding` (`AllowStreamLocalForwarding no`), `password` (links to
//! `cluster` ask for it; each ask is logged to `asked.log` as `text` or `empty`, never the
//! answer). Each tunnel call is logged to `tunnel.log`.
//!
//! The fake daemon (`pitcrewd serve`) echoes every connection, half-closes like it, answers
//! `GET` with HTTP, and on `close-write\n` says `closing\n`, half-closes, and appends what it
//! still receives to `<socket>.got`.

use crate::slurm::{self as fake_slurm, Config};
use crate::unix::{
    Machine, RUN_ENV, Remote, alive, daemon, decode, deploy_and_start, launch_options, mode,
    private_dir, quick, run_mark, runtime, stop_helper,
};
use pitcrew_remote::helper::slurm::{LastHop, Site, SocketPlace};
use pitcrew_remote::{
    Connector, ConnectorOptions, Daemon, DirectLauncher, JobOptions, Launcher as _, LinkState,
    Platform, PromptCancel, PromptFuture, PromptHandler, PromptKind, PromptRequest, Reply, Secret,
    SlurmLauncher, Ssh, Target, Transport, Unreachable, WallClock, deploy,
};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::io::{BufRead as _, BufReader, Read, Write};
use std::net::Shutdown;
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

const TUNNEL_LOG: &str = "tunnel.log";
const ASKED: &str = "asked.log";
const NET: &str = "net";
const NO_FORWARDING: &str = "no-forwarding";
/// One line per forwarded channel the machine refused.
const REFUSED: &str = "refused.log";
const PASSWORD: &str = "password";
/// What the fake daemon reads to half-close first.
const CLOSE_WRITE: &[u8] = b"close-write\n";

// ─── The fake ssh, for the tunnel ──────────────────────────────────────────────────────────

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
    jump: Option<String>,
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
                "-J" => call.jump = Some(value()),
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
        if self.op.is_some() {
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
        match &self.log {
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
enum Net {
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

/// Plays the tunnel's calls; `None` for any other call (the plain fake plays it).
pub(crate) fn fake(remote: &Remote, dir: &Path, args: &[String]) -> Option<u8> {
    let call = Call::parse(args);
    let kind = call.kind()?;
    let machine = dir.parent()?.to_path_buf();
    log_call(&machine, &call, kind);
    if let Err(why) = check_options(&call, kind) {
        call.say(&format!("fake ssh: refused: {why}"));
        return Some(255);
    }
    Some(match kind {
        "control" => control_op(&call),
        "link" => link(&machine, &call),
        "stdio" => stdio_forward(&call),
        _ => session(remote, &call),
    })
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
    if kind == "control" {
        return if call.config.as_deref() == Some("none") {
            Ok(())
        } else {
            Err("-F none is missing".to_owned())
        };
    }
    want("ForwardAgent", "no")?;
    want("ForwardX11", "no")?;
    want("PermitLocalCommand", "no")?;
    if kind == "link" {
        want("ClearAllForwardings", "yes")?;
        want("StrictHostKeyChecking", "ask")?;
        want("ControlMaster", "yes")?;
        want("ControlPersist", "no")?;
        for keepalive in ["ServerAliveInterval", "ServerAliveCountMax"] {
            if call
                .option(keepalive)
                .and_then(|v| v.parse::<u64>().ok())
                .is_none()
            {
                return Err(format!("{keepalive} is missing"));
            }
        }
    } else {
        want("ControlMaster", "no")?;
        want("ProxyCommand", "false")?;
        want("BatchMode", "yes")?;
        if call.config.as_deref() != Some("none") {
            return Err("-F none is missing".to_owned());
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

/// `ssh -N`: a link, as a ControlMaster.
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
    let control = PathBuf::from(call.option("ControlPath").unwrap());
    let _ = std::fs::remove_file(&control);
    let listener = UnixListener::bind(&control).unwrap();
    let exit = Arc::new(AtomicBool::new(false));
    let mask = call
        .option("StreamLocalBindMask")
        .and_then(|m| u32::from_str_radix(m, 8).ok())
        .unwrap_or(0o177);
    {
        let machine = machine.to_path_buf();
        let log = call.log.clone();
        let exit = exit.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let machine = machine.clone();
                let log = log.clone();
                let exit = exit.clone();
                std::thread::spawn(move || serve_mux(stream, &machine, log, mask, &exit));
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
    let _ = std::fs::remove_file(&control);
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
    log: Option<PathBuf>,
    mask: u32,
    exit: &AtomicBool,
) {
    let mut line = String::new();
    if BufReader::new(stream.try_clone().unwrap())
        .read_line(&mut line)
        .is_err()
    {
        return;
    }
    let words: Vec<&str> = line.split_whitespace().collect();
    let say = |text: &str| {
        Call {
            log: log.clone(),
            ..Call::default()
        }
        .say(text);
    };
    match words.as_slice() {
        ["check"] => {
            let _ = writeln!(stream, "ok {}", std::process::id());
        }
        ["exit"] => {
            exit.store(true, Ordering::SeqCst);
            let _ = writeln!(stream, "ok");
        }
        ["forward", local, remote] => {
            match listen_forward(local, remote, machine, log.clone(), mask) {
                Ok(()) => {
                    let _ = writeln!(stream, "ok");
                }
                Err(e) => {
                    say(&format!("fake ssh: forward failed: {e}"));
                    let _ = writeln!(stream, "fail {e}");
                }
            }
        }
        ["session" | "stdio", ..] => {
            // A channel opens only while the network answers.
            wait_up(machine);
            let _ = writeln!(stream, "ok");
            // Held until the client goes.
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
            let say = {
                let log = log.clone();
                move |text: &str| {
                    Call {
                        log: log.clone(),
                        ..Call::default()
                    }
                    .say(text);
                }
            };
            std::thread::spawn(move || {
                if machine.join(NO_FORWARDING).exists() {
                    say("channel 3: open failed: administratively prohibited: open failed");
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
                wait_up(&machine);
                match UnixStream::connect(&remote) {
                    Ok(daemon) => relay(client, daemon),
                    Err(_) => {
                        say("channel 3: open failed: connect failed: No such file or directory");
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

/// A channel running a command on the machine.
fn session(remote: &Remote, call: &Call) -> u8 {
    let Ok(mut master) = to_master(call) else {
        // ProxyCommand=false: ssh would have logged in itself; it fails instead.
        call.say("kex_exchange_identification: Connection closed by remote host");
        return 255;
    };
    if ask_master(&mut master, "session").is_none() {
        call.say("mux_client_request_session: read from master failed: Broken pipe");
        return 255;
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
            Call {
                log,
                ..Call::default()
            }
            .say("mux_client_read_packet: read header failed: Broken pipe");
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

// ─── Fixtures ──────────────────────────────────────────────────────────────────────────────

/// The askpass binary.
fn askpass() -> String {
    env!("CARGO_BIN_EXE_pitcrew-askpass").to_owned()
}

/// Options that keep the cases short.
fn options() -> ConnectorOptions {
    ConnectorOptions {
        link_wait: Duration::from_secs(20),
        connect_wait: Duration::from_secs(15),
        bridge_wait: Duration::from_secs(15),
        check_every: Duration::from_secs(2),
        probe_every: Duration::from_secs(30),
        probe_timeout: Duration::from_secs(3),
        backoff_min: Duration::from_millis(200),
        backoff_max: Duration::from_secs(2),
        give_up_after: Duration::from_secs(60),
        retry_every: Duration::from_secs(2),
        ..ConnectorOptions::default()
    }
}

/// A connector started on `rt`.
fn start(rt: &tokio::runtime::Runtime, daemon: Daemon, options: ConnectorOptions) -> Connector {
    let _entered = rt.enter();
    Connector::start(daemon, options).unwrap()
}

/// Waits up to `within` for a state `pred` accepts, and returns it.
fn wait_for(
    rt: &tokio::runtime::Runtime,
    connector: &Connector,
    what: &str,
    within: Duration,
    pred: impl Fn(&LinkState) -> bool,
) -> LinkState {
    let mut rx = connector.watch();
    let found = rt.block_on(async {
        tokio::time::timeout(within, async {
            loop {
                let now = rx.borrow_and_update().clone();
                if pred(&now) {
                    return now;
                }
                if rx.changed().await.is_err() {
                    return LinkState::Closed;
                }
            }
        })
        .await
    });
    match found {
        Ok(state) if pred(&state) => state,
        _ => panic!(
            "timed out waiting for {what}: the state is {}",
            connector.state()
        ),
    }
}

fn connected(transport: Transport) -> impl Fn(&LinkState) -> bool {
    move |s| *s == LinkState::Connected { transport }
}

fn unverifiable(s: &LinkState) -> bool {
    matches!(s, LinkState::Unverifiable { .. })
}

/// Sends `data` through one connection, half-closes, and reads to the end.
fn echo(rt: &tokio::runtime::Runtime, connector: &Connector, data: &[u8]) -> Vec<u8> {
    rt.block_on(async {
        let stream = connector.connect().await.unwrap();
        exchange(stream, data.to_vec()).await
    })
}

async fn exchange(stream: pitcrew_remote::TunnelStream, data: Vec<u8>) -> Vec<u8> {
    let (mut from, mut to) = tokio::io::split(stream);
    let writing = async move {
        to.write_all(&data).await.unwrap();
        to.shutdown().await.unwrap();
    };
    let reading = async move {
        let mut got = Vec::new();
        from.read_to_end(&mut got).await.unwrap();
        got
    };
    let ((), got) = tokio::join!(writing, reading);
    got
}

/// Bytes that are not text, nor a request the fake daemon reads specially.
fn pattern(len: usize, seed: u8) -> Vec<u8> {
    let mut x = u32::from(seed).wrapping_mul(2_654_435_761).wrapping_add(1);
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x.to_le_bytes()[0]
        })
        .map(|b| if b == b'G' { 0 } else { b })
        .collect()
}

fn net_set(m: &Machine, state: Net) {
    let path = m.dir.path().join(NET);
    match state {
        Net::Up => {
            let _ = std::fs::remove_file(path);
        }
        Net::Down => std::fs::write(path, "down").unwrap(),
        Net::Frozen => std::fs::write(path, "frozen").unwrap(),
    }
}

/// The tunnel calls the machine's fake ssh saw.
fn tunnel_calls(m: &Machine) -> Vec<TunnelCall> {
    std::fs::read_to_string(m.dir.path().join(TUNNEL_LOG))
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

/// A target whose ssh is a fresh fake of `m`.
fn target(m: &Machine) -> Target {
    m.target(&m.fake(Remote::default()))
}

/// The connectors' private directories under a fake's runtime directory.
fn private_dirs(fake: &crate::unix::Fake) -> Vec<PathBuf> {
    std::fs::read_dir(fake.dir.join("rt"))
        .map(|entries| {
            entries
                .map(|e| e.unwrap().path())
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.starts_with('t'))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// A SLURM machine with the helper deployed and a job running it, reached as `last_hop` says.
fn job(site: &Site, config: Config) -> (Machine, fake_slurm::Sim, SlurmLauncher, u64) {
    let (m, sim) = fake_slurm::machine(config);
    let target = m.plain();
    pitcrew_remote_deploy(&target);
    let script = fake_slurm::render(&target, site, &JobOptions::default());
    let launcher = fake_slurm::launcher(&script);
    let started = crate::unix::block_on(launcher.start(&target)).unwrap();
    let id = started.endpoint.job.unwrap();
    (m, sim, launcher, id)
}

fn pitcrew_remote_deploy(target: &Target) {
    crate::unix::block_on(deploy(target, &crate::unix::helper("1.0.0"), &quick())).unwrap();
}

// ─── Cases ─────────────────────────────────────────────────────────────────────────────────

/// A login node whose sshd forwards unix sockets: one forwarded socket in a private directory,
/// shared by many connections at once, byte for byte; gone on close.
fn tunnel_forwarded_socket_with_many_connections() {
    let m = Machine::new();
    deploy_and_start(&m);
    let fake = m.fake(Remote::default());
    let rt = runtime();
    let launcher = Arc::new(DirectLauncher::new(launch_options()));
    let connector = start(&rt, Daemon::new(m.target(&fake), launcher), options());
    wait_for(
        &rt,
        &connector,
        "connected",
        Duration::from_secs(30),
        connected(Transport::Forwarded),
    );
    assert_eq!(connector.transport(), Some(Transport::Forwarded));

    let got = rt.block_on(async {
        let mut set = tokio::task::JoinSet::new();
        for i in 0..8u8 {
            let stream = connector.connect().await.unwrap();
            assert_eq!(stream.transport(), Transport::Forwarded);
            set.spawn(async move {
                let data = pattern(256 * 1024, i);
                (exchange(stream, data.clone()).await, data)
            });
        }
        set.join_all().await
    });
    for (back, sent) in got {
        assert_eq!(back.len(), sent.len());
        assert!(back == sent, "the echo differs");
    }

    // One link and one forward did it all; the private directory is the user's alone.
    let calls = tunnel_calls(&m);
    assert_eq!(calls.iter().filter(|c| c.kind == "link").count(), 1);
    assert_eq!(
        calls
            .iter()
            .filter(|c| c.op.as_deref() == Some("forward"))
            .count(),
        1
    );
    let dirs = private_dirs(&fake);
    assert_eq!(dirs.len(), 1, "{dirs:?}");
    assert_eq!(mode(&dirs[0]), 0o700);
    let forwarded: Vec<PathBuf> = std::fs::read_dir(&dirs[0])
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.file_name().unwrap().to_str().unwrap().starts_with('f'))
        .collect();
    assert_eq!(forwarded.len(), 1);
    assert_eq!(mode(&forwarded[0]) & 0o077, 0);

    rt.block_on(connector.close());
    assert_eq!(connector.state(), LinkState::Closed);
    assert!(!dirs[0].exists(), "the private directory is left");
    let err = rt.block_on(connector.connect()).unwrap_err();
    assert!(err.to_string().contains("closed"), "{err}");
    stop_helper(&m);
}

/// `AllowStreamLocalForwarding no`: ssh's refusal is read, remembered, and the stdio bridge
/// carries the connections instead.
fn tunnel_forwarding_refused_then_the_stdio_bridge() {
    let m = Machine::new();
    deploy_and_start(&m);
    std::fs::write(m.dir.path().join(NO_FORWARDING), "").unwrap();
    let rt = runtime();
    let launcher = Arc::new(DirectLauncher::new(launch_options()));
    let connector = start(&rt, Daemon::new(target(&m), launcher.clone()), options());
    wait_for(
        &rt,
        &connector,
        "connected through the bridge",
        Duration::from_secs(30),
        connected(Transport::Stdio),
    );
    assert_eq!(connector.transport(), Some(Transport::Stdio));
    let data = pattern(100_000, 7);
    assert!(echo(&rt, &connector, &data) == data);
    // The bridge ran in channels of the link, as `exec <helper> connect --socket <socket>`.
    let calls = tunnel_calls(&m);
    let socket = m.layout().socket();
    let bridge = pitcrew_remote::quote::posix_command(&[
        "exec",
        &m.layout().binary("1.0.0"),
        "connect",
        "--socket",
        &socket,
    ])
    .unwrap();
    assert!(
        calls
            .iter()
            .any(|c| c.kind == "session" && c.line.as_deref() == Some(bridge.as_str())),
        "{calls:?}"
    );
    let forwards = |calls: &[TunnelCall]| {
        calls
            .iter()
            .filter(|c| c.op.as_deref() == Some("forward"))
            .count()
    };
    assert_eq!(forwards(&calls), 1);

    // Reconnected, the refusal is remembered: no forward is tried again.
    net_set(&m, Net::Down);
    wait_for(
        &rt,
        &connector,
        "unverifiable",
        Duration::from_secs(15),
        unverifiable,
    );
    net_set(&m, Net::Up);
    wait_for(
        &rt,
        &connector,
        "connected again",
        Duration::from_secs(30),
        connected(Transport::Stdio),
    );
    assert!(echo(&rt, &connector, &data) == data);
    assert_eq!(forwards(&tunnel_calls(&m)), 1);
    rt.block_on(connector.close());

    // A remembered choice is used at once.
    let remembered = ConnectorOptions {
        transport: Some(Transport::Stdio),
        ..options()
    };
    std::fs::remove_file(m.dir.path().join(NO_FORWARDING)).unwrap();
    let connector = start(&rt, Daemon::new(target(&m), launcher), remembered);
    wait_for(
        &rt,
        &connector,
        "connected through the bridge",
        Duration::from_secs(30),
        connected(Transport::Stdio),
    );
    assert_eq!(forwards(&tunnel_calls(&m)), 1);
    rt.block_on(connector.close());
    stop_helper(&m);
}

/// Forwarding refused while the bridge fails too (a helper whose `connect` finds no daemon): the
/// attempts that follow use the bridge alone, never the refused forward again.
fn tunnel_a_refused_forward_is_not_tried_again() {
    let m = Machine::new();
    std::fs::write(m.dir.path().join(NO_FORWARDING), "").unwrap();
    let script = String::from_utf8(crate::unix::helper_script("1.0.0", "serve", 0)).unwrap();
    let mut broken: String = script
        .lines()
        .map(|line| {
            if line.starts_with("connect)") {
                "connect) echo 'pitcrewd connect: no daemon listens on the socket: x' >&2; exit 4 ;;"
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    broken.push('\n');
    let plain = m.plain();
    crate::unix::block_on(deploy(
        &plain,
        &crate::unix::helper_from("1.0.0", broken.into_bytes()),
        &quick(),
    ))
    .unwrap();
    crate::unix::block_on(DirectLauncher::new(launch_options()).start(&plain)).unwrap();
    let rt = runtime();
    let launcher = Arc::new(DirectLauncher::new(launch_options()));
    let connector = start(&rt, Daemon::new(target(&m), launcher), options());
    let state = wait_for(
        &rt,
        &connector,
        "not running",
        Duration::from_secs(30),
        |s| {
            matches!(
                s,
                LinkState::Unreachable {
                    why: Unreachable::NotRunning,
                    ..
                }
            )
        },
    );
    assert!(state.to_string().contains("no daemon"), "{state}");
    // It tries again every `retry_every`, with the bridge alone.
    let bridges = |m: &Machine| {
        tunnel_calls(m)
            .iter()
            .filter(|c| {
                c.kind == "session" && c.line.as_deref().is_some_and(|l| l.contains(" connect "))
            })
            .count()
    };
    crate::unix::eventually("three tries of the bridge", || bridges(&m) >= 3);
    let refused = std::fs::read_to_string(m.dir.path().join(REFUSED))
        .unwrap_or_default()
        .lines()
        .count();
    assert_eq!(refused, 1, "the refused forward was tried again");
    rt.block_on(connector.close());
    stop_helper(&m);
}

/// A job on a node that takes no ssh, with its socket on the node's own disk: the bridge runs
/// in the job (`srun --jobid <id> --overlap`), started on the login node.
fn tunnel_bridge_through_srun_to_a_node_local_socket() {
    let site = Site {
        name: "node-local".to_owned(),
        socket: SocketPlace::NodeLocal,
        last_hop: LastHop::SrunOverlap,
        ..Site::default()
    };
    let (m, sim) = fake_slurm::machine(Config::default());
    let tmp = m.dir.path().join("node-tmp");
    private_dir(&tmp);
    sim.set(|c| c.tmpdir = Some(tmp.clone()));
    let plain = m.plain();
    pitcrew_remote_deploy(&plain);
    let script = fake_slurm::render(&plain, &site, &JobOptions::default());
    let launcher = fake_slurm::launcher(&script);
    let started = crate::unix::block_on(launcher.start(&plain)).unwrap();
    let id = started.endpoint.job.unwrap();
    let socket = started.endpoint.socket.clone();
    assert!(socket.starts_with(tmp.to_str().unwrap()), "{socket}");

    let rt = runtime();
    let daemon =
        Daemon::new(target(&m), Arc::new(launcher.clone())).with_last_hop(LastHop::SrunOverlap);
    let connector = start(&rt, daemon, options());
    wait_for(
        &rt,
        &connector,
        "connected through srun",
        Duration::from_secs(30),
        connected(Transport::Stdio),
    );
    let data = pattern(200_000, 3);
    assert!(echo(&rt, &connector, &data) == data);

    let steps = sim.calls("srun");
    let want: Vec<String> = [
        format!("--jobid={id}"),
        "--overlap".to_owned(),
        "--nodes=1".to_owned(),
        "--ntasks=1".to_owned(),
        "--nodelist=node017".to_owned(),
        "--quiet".to_owned(),
        m.layout().binary("1.0.0"),
        "connect".to_owned(),
        "--socket".to_owned(),
        socket,
    ]
    .to_vec();
    assert!(steps.contains(&want), "{steps:?}");
    // No link to the node: the login node's link carried it all.
    assert!(
        tunnel_calls(&m)
            .iter()
            .all(|c| c.kind != "link" || c.host == "cluster")
    );
    rt.block_on(connector.close());
    crate::unix::block_on(launcher.cancel(&plain)).unwrap();
}

/// A job on a node that takes ssh: a link to the node, through the login node's link (its
/// `ProxyCommand` a channel of the login link, never a second login there).
fn tunnel_proxyjump_to_a_node() {
    let (m, _sim, launcher, _id) = job(&fake_slurm_generic(), Config::default());
    let rt = runtime();
    let connector = start(
        &rt,
        Daemon::new(target(&m), Arc::new(launcher.clone())),
        options(),
    );
    wait_for(
        &rt,
        &connector,
        "connected to the node",
        Duration::from_secs(30),
        connected(Transport::Forwarded),
    );
    let data = pattern(150_000, 5);
    assert!(echo(&rt, &connector, &data) == data);
    let calls = tunnel_calls(&m);
    let links: Vec<&TunnelCall> = calls.iter().filter(|c| c.kind == "link").collect();
    assert_eq!(
        links.iter().map(|c| c.host.as_str()).collect::<Vec<_>>(),
        ["cluster", "node017"]
    );
    let proxy = links[1].proxy.as_deref().unwrap();
    assert!(proxy.starts_with("exec "), "{proxy}");
    assert!(proxy.contains("ControlMaster=no") && proxy.contains("-W '[%h]:%p' -- cluster"));
    assert!(
        calls
            .iter()
            .any(|c| c.kind == "stdio" && c.host == "cluster"),
        "{calls:?}"
    );
    rt.block_on(connector.close());

    // The bridge to the node, through its link.
    let stdio = ConnectorOptions {
        transport: Some(Transport::Stdio),
        ..options()
    };
    let connector = start(
        &rt,
        Daemon::new(target(&m), Arc::new(launcher.clone())),
        stdio,
    );
    wait_for(
        &rt,
        &connector,
        "connected to the node",
        Duration::from_secs(30),
        connected(Transport::Stdio),
    );
    assert!(echo(&rt, &connector, &data) == data);
    assert!(
        tunnel_calls(&m)
            .iter()
            .any(|c| c.kind == "session" && c.host == "node017"),
    );
    rt.block_on(connector.close());
    crate::unix::block_on(launcher.cancel(&m.plain())).unwrap();
}

fn fake_slurm_generic() -> Site {
    pitcrew_remote::helper::slurm::generic()
}

/// The node squeue names must be the one the job recorded, and plain: otherwise nothing is
/// started towards it (no link to a node, no srun), and the machine is unreachable, refused.
fn tunnel_a_node_that_fails_the_check_is_refused() {
    let (m, sim, launcher, id) = job(&fake_slurm_generic(), Config::default());
    let rt = runtime();
    let calls_to_nodes = |m: &Machine| {
        tunnel_calls(m)
            .iter()
            .filter(|c| {
                c.host != "cluster" || c.line.as_deref().is_some_and(|l| l.contains("srun"))
            })
            .count()
    };

    // squeue runs the job on another node than its record names.
    let mut moved = sim.job(id);
    moved.node = "node018".to_owned();
    sim.save(&moved);
    let connector = start(
        &rt,
        Daemon::new(target(&m), Arc::new(launcher.clone())),
        options(),
    );
    let state = wait_for(&rt, &connector, "refused", Duration::from_secs(30), |s| {
        matches!(
            s,
            LinkState::Unreachable {
                why: Unreachable::Refused,
                ..
            }
        )
    });
    assert!(state.to_string().contains("node018"), "{state}");
    assert_eq!(calls_to_nodes(&m), 0);
    // Put right, it is picked up again from the record.
    moved.node = "node017".to_owned();
    sim.save(&moved);
    wait_for(
        &rt,
        &connector,
        "connected",
        Duration::from_secs(30),
        connected(Transport::Forwarded),
    );
    rt.block_on(connector.close());

    // A name that would be an option or a shell word, in both squeue and the record, with
    // either last hop.
    let endpoint = m.run_dir().join("endpoint.json");
    let record = std::fs::read_to_string(&endpoint).unwrap();
    for hostile in ["-oProxyCommand=touch", "node017;id"] {
        let before = calls_to_nodes(&m);
        let mut bad = sim.job(id);
        bad.node = hostile.to_owned();
        sim.save(&bad);
        std::fs::write(
            &endpoint,
            record.replace("\"host\":\"node017\"", &format!("\"host\":\"{hostile}\"")),
        )
        .unwrap();
        for last_hop in [LastHop::Ssh, LastHop::SrunOverlap] {
            let daemon =
                Daemon::new(target(&m), Arc::new(launcher.clone())).with_last_hop(last_hop);
            let connector = start(&rt, daemon, options());
            let state = wait_for(&rt, &connector, "refused", Duration::from_secs(30), |s| {
                matches!(
                    s,
                    LinkState::Unreachable {
                        why: Unreachable::Refused,
                        ..
                    }
                )
            });
            assert!(state.to_string().contains("node name"), "{state}");
            rt.block_on(connector.close());
        }
        assert_eq!(calls_to_nodes(&m), before, "{hostile}");
        assert!(sim.calls("srun").is_empty());
    }
    let mut restored = sim.job(id);
    restored.node = "node017".to_owned();
    sim.save(&restored);
    std::fs::write(&endpoint, record).unwrap();
    crate::unix::block_on(launcher.cancel(&m.plain())).unwrap();
}

/// The network goes: the link's keepalives notice within ten seconds, and the connector, after
/// backing off, recovers by itself once it is back.
fn tunnel_a_dropped_stream_is_unverifiable_within_ten_seconds_then_recovers() {
    let m = Machine::new();
    deploy_and_start(&m);
    let rt = runtime();
    let launcher = Arc::new(DirectLauncher::new(launch_options()));
    // Only the keepalives: no check or probe that might notice first.
    let quiet = ConnectorOptions {
        connect_wait: Duration::from_secs(3),
        check_every: Duration::from_secs(600),
        probe_every: Duration::from_secs(600),
        ..options()
    };
    let connector = start(&rt, Daemon::new(target(&m), launcher), quiet);
    wait_for(
        &rt,
        &connector,
        "connected",
        Duration::from_secs(30),
        connected(Transport::Forwarded),
    );
    let lost = Instant::now();
    net_set(&m, Net::Down);
    let state = wait_for(
        &rt,
        &connector,
        "unverifiable",
        Duration::from_secs(20),
        unverifiable,
    );
    let noticed = lost.elapsed();
    println!("the lost connection was noticed after {noticed:.1?}: {state}");
    assert!(noticed < Duration::from_secs(10), "{noticed:?}");
    assert!(state.to_string().contains("not responding"), "{state}");
    // It keeps trying: the state stays unverifiable, with the latest reason.
    std::thread::sleep(Duration::from_secs(3));
    assert!(unverifiable(&connector.state()), "{}", connector.state());
    let err = rt.block_on(async {
        tokio::time::timeout(Duration::from_secs(30), connector.connect()).await
    });
    assert!(matches!(err, Ok(Err(_))), "{err:?}");

    net_set(&m, Net::Up);
    wait_for(
        &rt,
        &connector,
        "connected again",
        Duration::from_secs(30),
        connected(Transport::Forwarded),
    );
    let data = pattern(50_000, 9);
    assert!(echo(&rt, &connector, &data) == data);
    rt.block_on(connector.close());
    stop_helper(&m);
}

/// The laptop slept: its wall clock jumped while the monotonic one stood still. The link has not
/// noticed (nothing answers, nothing times out), but the connector checks at once, finds no
/// answer, and reconnects when the network answers.
fn tunnel_a_wall_clock_jump() {
    let m = Machine::new();
    deploy_and_start(&m);
    let rt = runtime();
    let launcher = Arc::new(DirectLauncher::new(launch_options()));
    let offset = Arc::new(AtomicU64::new(0));
    let clock = {
        let offset = offset.clone();
        WallClock::new(move || {
            SystemTime::now() + Duration::from_secs(offset.load(Ordering::SeqCst))
        })
    };
    let asleep = ConnectorOptions {
        check_every: Duration::from_secs(600),
        probe_every: Duration::from_secs(600),
        probe_timeout: Duration::from_secs(2),
        wall_clock: clock,
        ..options()
    };
    let connector = start(&rt, Daemon::new(target(&m), launcher), asleep);
    wait_for(
        &rt,
        &connector,
        "connected",
        Duration::from_secs(30),
        connected(Transport::Forwarded),
    );
    net_set(&m, Net::Frozen);
    std::thread::sleep(Duration::from_secs(3));
    assert!(connector.state().is_connected(), "{}", connector.state());

    let woke = Instant::now();
    offset.store(3600, Ordering::SeqCst);
    let state = wait_for(
        &rt,
        &connector,
        "unverifiable",
        Duration::from_secs(15),
        unverifiable,
    );
    let noticed = woke.elapsed();
    println!("the jump was acted on after {noticed:.1?}: {state}");
    assert!(noticed < Duration::from_secs(6), "{noticed:?}");

    net_set(&m, Net::Up);
    wait_for(
        &rt,
        &connector,
        "connected again",
        Duration::from_secs(30),
        connected(Transport::Forwarded),
    );
    let data = pattern(10_000, 11);
    assert!(echo(&rt, &connector, &data) == data);
    rt.block_on(connector.close());
    stop_helper(&m);
}

/// The job ends (its node fails, with no time to clean up): the machine is unreachable, saying
/// why; once a new job runs, on another node, its endpoint is picked up again; and a job the
/// launcher stopped is gone too.
fn tunnel_a_job_that_ended_then_moved() {
    let (m, sim, launcher, id) = job(&fake_slurm_generic(), Config::default());
    let rt = runtime();
    let watchful = ConnectorOptions {
        probe_every: Duration::from_secs(1),
        ..options()
    };
    let connector = start(
        &rt,
        Daemon::new(target(&m), Arc::new(launcher.clone())),
        watchful,
    );
    wait_for(
        &rt,
        &connector,
        "connected",
        Duration::from_secs(30),
        connected(Transport::Forwarded),
    );
    sim.fail_node(id);
    let not_running = |s: &LinkState| {
        matches!(
            s,
            LinkState::Unreachable {
                why: Unreachable::NotRunning,
                ..
            }
        )
    };
    let state = wait_for(
        &rt,
        &connector,
        "not running",
        Duration::from_secs(30),
        not_running,
    );
    assert!(state.to_string().contains("ended (NODE_FAIL"), "{state}");

    // A new job, on another node.
    sim.set(|c| c.node = "node018".to_owned());
    let started = crate::unix::block_on(launcher.start(&m.plain())).unwrap();
    assert_eq!(started.endpoint.host, "node018");
    wait_for(
        &rt,
        &connector,
        "connected to the new node",
        Duration::from_secs(30),
        connected(Transport::Forwarded),
    );
    let data = pattern(20_000, 13);
    assert!(echo(&rt, &connector, &data) == data);
    assert!(
        tunnel_calls(&m)
            .iter()
            .any(|c| c.kind == "link" && c.host == "node018")
    );

    // Stopped with the launcher, which forgets it (the connector may see it end first).
    crate::unix::block_on(launcher.cancel(&m.plain())).unwrap();
    wait_for(&rt, &connector, "no job", Duration::from_secs(30), |s| {
        not_running(s) && s.to_string().contains("no helper job")
    });
    rt.block_on(connector.close());
}

/// Answers prompts from a queue (then with the password), counting them.
struct Answers {
    queue: Mutex<VecDeque<Reply>>,
    asked: Mutex<Vec<PromptKind>>,
}

impl Answers {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            queue: Mutex::new(VecDeque::new()),
            asked: Mutex::new(Vec::new()),
        })
    }

    fn asked(&self) -> usize {
        self.asked.lock().unwrap().len()
    }
}

impl PromptHandler for Answers {
    fn prompt(&self, request: PromptRequest, _cancel: PromptCancel) -> PromptFuture<'_> {
        self.asked.lock().unwrap().push(request.kind);
        let reply = self
            .queue
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Reply::Text(Secret::new("s3cr3t")));
        Box::pin(async move { reply })
    }
}

/// A password asked again while reconnecting goes through the askpass bridge, and is never
/// kept; a cancelled one stops the attempts until the person retries.
fn tunnel_askpass_during_a_reconnect() {
    let m = Machine::new();
    deploy_and_start(&m);
    std::fs::write(m.dir.path().join(PASSWORD), "s3cr3t").unwrap();
    let answers = Answers::new();
    let fake = m.fake(Remote::default());
    let ssh: Ssh = fake.ssh.clone().with_prompts(askpass(), answers.clone());
    let target = Target::with_layout(ssh, "cluster", m.layout(), Platform::LinuxX86_64)
        .unwrap()
        .with_tool_path(m.bin.to_str().unwrap())
        .unwrap();
    let rt = runtime();
    let launcher = Arc::new(DirectLauncher::new(launch_options()));
    let connector = start(&rt, Daemon::new(target, launcher), options());
    wait_for(
        &rt,
        &connector,
        "connected",
        Duration::from_secs(30),
        connected(Transport::Forwarded),
    );
    assert_eq!(answers.asked(), 1);

    net_set(&m, Net::Down);
    wait_for(
        &rt,
        &connector,
        "unverifiable",
        Duration::from_secs(15),
        unverifiable,
    );
    net_set(&m, Net::Up);
    wait_for(
        &rt,
        &connector,
        "connected again",
        Duration::from_secs(30),
        connected(Transport::Forwarded),
    );
    assert_eq!(answers.asked(), 2, "asked again while reconnecting");
    let asked = std::fs::read_to_string(m.dir.path().join(ASKED)).unwrap();
    assert_eq!(asked.lines().collect::<Vec<_>>(), ["text", "text"]);

    // Cancelled while reconnecting: no more attempts, no more prompts, until a retry.
    answers.queue.lock().unwrap().push_back(Reply::Cancel);
    net_set(&m, Net::Down);
    wait_for(
        &rt,
        &connector,
        "unverifiable",
        Duration::from_secs(15),
        unverifiable,
    );
    net_set(&m, Net::Up);
    wait_for(
        &rt,
        &connector,
        "unreachable: sign-in",
        Duration::from_secs(30),
        |s| {
            matches!(
                s,
                LinkState::Unreachable {
                    why: Unreachable::SignIn,
                    ..
                }
            )
        },
    );
    let asked = answers.asked();
    std::thread::sleep(Duration::from_secs(4));
    assert_eq!(answers.asked(), asked, "asked again after a cancel");
    // An empty password was never sent.
    let sent = std::fs::read_to_string(m.dir.path().join(ASKED)).unwrap();
    assert!(!sent.contains("empty"), "{sent}");
    connector.retry();
    wait_for(
        &rt,
        &connector,
        "connected after the retry",
        Duration::from_secs(30),
        connected(Transport::Forwarded),
    );
    // The password is nowhere: not in the state, the connector, or any log of the fake's.
    let mut seen = format!("{connector:?} {}", connector.state());
    for entry in std::fs::read_dir(m.dir.path()).unwrap() {
        let path = entry.unwrap().path();
        if path.is_file() && path.file_name().unwrap() != PASSWORD {
            seen.push_str(&String::from_utf8_lossy(&std::fs::read(&path).unwrap()));
        }
    }
    assert!(!seen.contains("s3cr3t"));
    rt.block_on(connector.close());
    stop_helper(&m);
}

/// The endpoint check and the bridge (`'exec' <helper> connect …`) with each POSIX shell as the
/// machine's `sh`, through both transports.
fn tunnel_under_every_posix_sh() {
    let mut checked = Vec::new();
    for shell in crate::unix::posix_shells() {
        let m = Machine::with_shell(&shell);
        deploy_and_start(&m);
        let rt = runtime();
        let launcher = Arc::new(DirectLauncher::new(launch_options()));
        for transport in [Transport::Stdio, Transport::Forwarded] {
            let chosen = ConnectorOptions {
                transport: Some(transport),
                ..options()
            };
            let connector = start(&rt, Daemon::new(target(&m), launcher.clone()), chosen);
            wait_for(
                &rt,
                &connector,
                &format!("connected with {}", shell.display()),
                Duration::from_secs(30),
                connected(transport),
            );
            let data = pattern(30_000, 17);
            assert!(echo(&rt, &connector, &data) == data, "{}", shell.display());
            rt.block_on(connector.close());
        }
        stop_helper(&m);
        checked.push(shell.display().to_string());
    }
    println!("tunnels checked with sh = {checked:?}");
}

// ─── The bridge alone ──────────────────────────────────────────────────────────────────────

/// The fake daemon, serving `<dir>/run/pitcrewd.sock`.
struct Served {
    daemon: std::process::Child,
    socket: PathBuf,
}

impl Served {
    fn new(dir: &Path) -> Self {
        let run = dir.join("run");
        private_dir(&run);
        let socket = run.join("pitcrewd.sock");
        let daemon = Command::new(daemon())
            .env("PITCREW_FAKE_DAEMON", "serve")
            .env(RUN_ENV, run_mark())
            .arg("serve")
            .arg("--listen")
            .arg(format!("unix:{}", socket.display()))
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        crate::unix::eventually("the daemon's socket", || socket.exists());
        Self { daemon, socket }
    }
}

impl Drop for Served {
    fn drop(&mut self) {
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
    }
}

/// `pitcrewd connect --socket <socket>`, with pipes for stdin, stdout and stderr.
fn bridge(socket: &Path) -> std::process::Child {
    Command::new(daemon())
        .env("PITCREW_FAKE_DAEMON", "connect")
        .env(RUN_ENV, run_mark())
        .arg("connect")
        .arg("--socket")
        .arg(socket)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

/// Reads the bridge's ready mark, which comes first.
fn read_ready(out: &mut impl Read) {
    let mut mark = vec![0u8; pitcrew_remote::bridge::READY.len()];
    out.read_exact(&mut mark).unwrap();
    assert_eq!(mark, pitcrew_remote::bridge::READY);
}

/// Every byte value both ways, and a large transfer, exactly.
fn bridge_is_byte_exact_both_ways() {
    let dir = tempfile::tempdir().unwrap();
    let served = Served::new(dir.path());
    for data in [
        (0..=255u8)
            .cycle()
            .skip(1)
            .take(256 * 40)
            .collect::<Vec<u8>>(),
        pattern(24 * 1024 * 1024, 1),
    ] {
        let mut child = bridge(&served.socket);
        let mut stdin = child.stdin.take().unwrap();
        let mut stdout = child.stdout.take().unwrap();
        let sent = data.clone();
        let writer = std::thread::spawn(move || {
            stdin.write_all(&sent).unwrap();
        });
        read_ready(&mut stdout);
        let mut back = Vec::new();
        stdout.read_to_end(&mut back).unwrap();
        writer.join().unwrap();
        assert_eq!(back.len(), data.len());
        assert!(back == data, "the echo differs");
        assert!(child.wait().unwrap().success());
    }
}

/// Either side may stop sending first; the other direction goes on.
fn bridge_half_closes_both_ways() {
    let dir = tempfile::tempdir().unwrap();
    let served = Served::new(dir.path());
    // The client is done first: the daemon reads end of file, and still answers.
    let mut child = bridge(&served.socket);
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(b"the whole request\n").unwrap();
    drop(stdin);
    let mut stdout = child.stdout.take().unwrap();
    read_ready(&mut stdout);
    let mut back = Vec::new();
    stdout.read_to_end(&mut back).unwrap();
    assert_eq!(back, b"the whole request\n");
    assert!(child.wait().unwrap().success());

    // The daemon is done first: the client reads end of file while the bridge still runs, and
    // what it sends then still arrives.
    let mut child = bridge(&served.socket);
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = child.stdout.take().unwrap();
    stdin.write_all(CLOSE_WRITE).unwrap();
    read_ready(&mut stdout);
    let mut back = Vec::new();
    stdout.read_to_end(&mut back).unwrap();
    assert_eq!(back, b"closing\n");
    assert!(
        child.try_wait().unwrap().is_none(),
        "the bridge ended early"
    );
    stdin.write_all(b"after the daemon's end").unwrap();
    drop(stdin);
    assert!(child.wait().unwrap().success());
    let got = PathBuf::from(format!("{}.got", served.socket.display()));
    crate::unix::eventually("the daemon to get the rest", || {
        std::fs::read(&got).is_ok_and(|g| g == b"after the daemon's end")
    });
}

/// A socket that is not the user's alone is refused before a byte passes, with no path in the
/// message; so is a missing one.
fn bridge_refuses_sockets_that_are_not_ours() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = tempfile::tempdir().unwrap();
    let served = Served::new(dir.path());
    let run = served.socket.parent().unwrap().to_path_buf();
    let refused = |socket: &Path, code: u8| {
        let child = bridge(socket);
        let out = child.wait_with_output().unwrap();
        assert_eq!(
            out.status.code(),
            Some(i32::from(code)),
            "{}",
            socket.display()
        );
        assert!(out.stdout.is_empty(), "something passed");
        let said = String::from_utf8_lossy(&out.stderr).into_owned();
        assert!(said.starts_with("pitcrewd connect: "), "{said}");
        assert!(!said.contains(dir.path().to_str().unwrap()), "{said}");
        said
    };
    use pitcrew_remote::bridge::{EXIT_NO_DAEMON, EXIT_UNSAFE, EXIT_USAGE};
    // Its directory open to the group.
    std::fs::set_permissions(&run, std::fs::Permissions::from_mode(0o750)).unwrap();
    let said = refused(&served.socket, EXIT_UNSAFE);
    assert!(said.contains("open to others"), "{said}");
    std::fs::set_permissions(&run, std::fs::Permissions::from_mode(0o700)).unwrap();
    // A link to it, a file that is not a socket, a link to its directory.
    let link = run.join("link.sock");
    std::os::unix::fs::symlink(&served.socket, &link).unwrap();
    refused(&link, EXIT_UNSAFE);
    let file = run.join("file.sock");
    std::fs::write(&file, "").unwrap();
    refused(&file, EXIT_UNSAFE);
    let dir_link = dir.path().join("run-link");
    std::os::unix::fs::symlink(&run, &dir_link).unwrap();
    refused(&dir_link.join("pitcrewd.sock"), EXIT_UNSAFE);
    // Nothing there, or a relative path.
    refused(&run.join("missing.sock"), EXIT_NO_DAEMON);
    refused(Path::new("run/pitcrewd.sock"), EXIT_USAGE);
    // And the real one passes.
    let mut child = bridge(&served.socket);
    drop(child.stdin.take());
    let mut stdout = child.stdout.take().unwrap();
    read_ready(&mut stdout);
    assert!(child.wait().unwrap().success());
    assert!(alive(served.daemon.id()));
}

pub(crate) const CASES: &[(&str, fn())] = &[
    (
        "tunnel_forwarded_socket_with_many_connections",
        tunnel_forwarded_socket_with_many_connections,
    ),
    (
        "tunnel_forwarding_refused_then_the_stdio_bridge",
        tunnel_forwarding_refused_then_the_stdio_bridge,
    ),
    (
        "tunnel_a_refused_forward_is_not_tried_again",
        tunnel_a_refused_forward_is_not_tried_again,
    ),
    (
        "tunnel_bridge_through_srun_to_a_node_local_socket",
        tunnel_bridge_through_srun_to_a_node_local_socket,
    ),
    ("tunnel_proxyjump_to_a_node", tunnel_proxyjump_to_a_node),
    (
        "tunnel_a_node_that_fails_the_check_is_refused",
        tunnel_a_node_that_fails_the_check_is_refused,
    ),
    (
        "tunnel_a_dropped_stream_is_unverifiable_within_ten_seconds_then_recovers",
        tunnel_a_dropped_stream_is_unverifiable_within_ten_seconds_then_recovers,
    ),
    ("tunnel_a_wall_clock_jump", tunnel_a_wall_clock_jump),
    (
        "tunnel_a_job_that_ended_then_moved",
        tunnel_a_job_that_ended_then_moved,
    ),
    (
        "tunnel_askpass_during_a_reconnect",
        tunnel_askpass_during_a_reconnect,
    ),
    ("tunnel_under_every_posix_sh", tunnel_under_every_posix_sh),
    (
        "bridge_is_byte_exact_both_ways",
        bridge_is_byte_exact_both_ways,
    ),
    ("bridge_half_closes_both_ways", bridge_half_closes_both_ways),
    (
        "bridge_refuses_sockets_that_are_not_ours",
        bridge_refuses_sockets_that_are_not_ours,
    ),
];
