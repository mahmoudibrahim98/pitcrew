//! The `pitcrew-askpass` side: connect, prove the key, ask, print the answer.
//!
//! Synchronous and small, since ssh starts it once per prompt.

use super::{
    ADDR_ENV, Ask, Hello, KEY_ENV, MAX_LINE, NONCE_LEN, PromptKind, ServerHello, WireReply,
    classify, client_proof, from_hex, key_from_hex, random, same, server_proof, to_hex,
};
use std::io::{self, Read, Write};

/// Exit status: the answer was printed.
pub const EXIT_ANSWERED: i32 = 0;
/// Exit status: not started by PitCrew (the bridge variables are unset), or the answer could
/// not be written.
pub const EXIT_FAILED: i32 = 2;
/// Exit status: started by PitCrew, but the bridge gave no answer (unreachable, a wrong
/// proof, or the connection closed: the app quit or crashed, or the handshake failed). ssh
/// must not see this, or it sends an empty password: the program calls [`Parent::stop_ssh`]
/// first.
pub const EXIT_NO_ANSWER: i32 = 3;

/// Runs the askpass program: `args` are its arguments (ssh passes the prompt as the first),
/// `var` reads the environment, and the answer goes to `out`. Returns the exit status.
/// Errors go to `err`, never the answer.
pub fn main_with(
    args: &[String],
    var: impl Fn(&str) -> Option<String>,
    out: &mut impl Write,
    err: &mut impl Write,
) -> i32 {
    let prompt = args.first().map_or("", String::as_str);
    let hint = var("SSH_ASKPASS_PROMPT");
    let kind = classify(prompt, hint.as_deref());
    let Some(addr) = var(ADDR_ENV) else {
        let _ = writeln!(
            err,
            "pitcrew-askpass: not started by PitCrew ({ADDR_ENV} unset)"
        );
        return EXIT_FAILED;
    };
    let asked = var(KEY_ENV)
        .as_deref()
        .and_then(key_from_hex)
        .ok_or_else(|| bad(&format!("{KEY_ENV} is missing or malformed")))
        .and_then(|key| connect(&addr).and_then(|mut stream| ask(&mut stream, &key, kind, prompt)));
    let reply = match asked {
        Ok(reply) => reply,
        Err(e) => {
            let _ = writeln!(err, "pitcrew-askpass: no answer: {e}");
            return EXIT_NO_ANSWER;
        }
    };
    let answer: &[u8] = match &reply {
        WireReply::Text { text } => text.as_bytes(),
        // `yes` answers a host-key question; ssh also takes it for a confirmation.
        WireReply::Accept => b"yes",
        WireReply::Decline => b"no",
    };
    let written = out
        .write_all(answer)
        .and_then(|()| out.write_all(b"\n"))
        .and_then(|()| out.flush());
    // Dropping the reply overwrites a typed answer.
    drop(reply);
    match written {
        Ok(()) => EXIT_ANSWERED,
        Err(e) => {
            let _ = writeln!(err, "pitcrew-askpass: {e}");
            EXIT_FAILED
        }
    }
}

/// The ssh that started this askpass, recorded when askpass starts, so it can be stopped when
/// the bridge gives no answer ([`EXIT_NO_ANSWER`]).
#[derive(Debug)]
pub struct Parent {
    #[cfg(unix)]
    pid: Option<rustix::process::Pid>,
}

impl Parent {
    /// Records the parent process. Call it first thing in `main`.
    #[must_use]
    pub fn at_start() -> Self {
        #[cfg(unix)]
        {
            // Parent 1 means ssh is already gone and init adopted us: nothing to stop.
            let pid = rustix::process::getppid().filter(|p| !p.is_init());
            Self { pid }
        }
        #[cfg(not(unix))]
        Self {}
    }

    /// Stops the ssh that asked, so it cannot go on to send an empty password.
    /// - Unix: kills it with SIGKILL, but only while it is still this process's parent: once
    ///   ssh is gone we are reparented, and its pid may belong to someone else. On Linux the
    ///   process is first pinned with a pidfd, then checked, then signalled through the pidfd,
    ///   which closes the last race. Returns whether the signal was sent.
    /// - Windows: never returns. ssh and this program run in a Job Object that PitCrew ends on
    ///   cancel, timeout, drop and exit (the OS ends it when PitCrew dies); until then ssh waits
    ///   here and never sees a failure.
    pub fn stop_ssh(&self) -> bool {
        #[cfg(unix)]
        {
            self.pid.is_some_and(kill_parent)
        }
        #[cfg(windows)]
        loop {
            std::thread::park();
        }
        #[cfg(not(any(unix, windows)))]
        false
    }
}

#[cfg(unix)]
fn kill_parent(pid: rustix::process::Pid) -> bool {
    use rustix::process::{Signal, getppid};
    let still_parent = || getppid() == Some(pid);
    #[cfg(target_os = "linux")]
    if let Ok(pinned) = rustix::process::pidfd_open(pid, rustix::process::PidfdFlags::empty()) {
        return still_parent() && rustix::process::pidfd_send_signal(&pinned, Signal::KILL).is_ok();
    }
    still_parent() && rustix::process::kill_process(pid, Signal::KILL).is_ok()
}

/// The handshake and one question, over any stream.
pub(crate) fn ask(
    stream: &mut (impl Read + Write),
    key: &[u8; super::KEY_LEN],
    kind: PromptKind,
    prompt: &str,
) -> io::Result<WireReply> {
    let nc = random::<NONCE_LEN>()?;
    send(
        stream,
        &Hello {
            v: 1,
            nonce: to_hex(&nc),
        },
    )?;
    let hello: ServerHello = receive(stream)?;
    let ns = from_hex(&hello.nonce)
        .filter(|n| n.len() == NONCE_LEN)
        .ok_or_else(|| bad("bad server nonce"))?;
    let proof = from_hex(&hello.proof).ok_or_else(|| bad("bad server proof"))?;
    if !same(&proof, &server_proof(key, &nc, &ns)) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the listener does not hold this call's key",
        ));
    }
    send(
        stream,
        &Ask {
            proof: to_hex(&client_proof(key, &nc, &ns, kind, prompt)),
            kind,
            prompt: prompt.to_owned(),
        },
    )?;
    receive(stream)
}

fn bad(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what.to_owned())
}

pub(crate) fn send(stream: &mut impl Write, message: &impl serde::Serialize) -> io::Result<()> {
    let mut line = serde_json::to_vec(message)?;
    line.push(b'\n');
    stream.write_all(&line)?;
    stream.flush()
}

/// Reads one line, a byte at a time so nothing past it is consumed. The line may hold a
/// secret: its buffer is allocated once, so it never leaves copies behind when growing, and is
/// overwritten before it is freed.
pub(crate) fn receive<T: serde::de::DeserializeOwned>(stream: &mut impl Read) -> io::Result<T> {
    let mut line = Vec::with_capacity(MAX_LINE);
    let result = read_line(stream, &mut line)
        .and_then(|()| serde_json::from_slice(&line).map_err(|e| bad(&e.to_string())));
    line.fill(0);
    std::hint::black_box(&line);
    result
}

fn read_line(stream: &mut impl Read, line: &mut Vec<u8>) -> io::Result<()> {
    let mut byte = [0u8; 1];
    loop {
        if stream.read(&mut byte)? == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "the listener closed the connection without an answer",
            ));
        }
        if byte[0] == b'\n' {
            return Ok(());
        }
        if line.len() >= MAX_LINE {
            return Err(bad("line too long"));
        }
        line.push(byte[0]);
    }
}

#[cfg(unix)]
fn connect(addr: &str) -> io::Result<std::os::unix::net::UnixStream> {
    std::os::unix::net::UnixStream::connect(addr)
}

/// Opens the pipe as a file, retrying briefly while every instance is busy. The server may
/// only identify us, never impersonate us (`SECURITY_IDENTIFICATION`), in case the name was
/// squatted.
#[cfg(windows)]
fn connect(addr: &str) -> io::Result<std::fs::File> {
    use std::os::windows::fs::OpenOptionsExt as _;
    use windows_sys::Win32::Storage::FileSystem::SECURITY_IDENTIFICATION;
    const ERROR_PIPE_BUSY: i32 = 231;
    let mut tries = 0;
    loop {
        match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .security_qos_flags(SECURITY_IDENTIFICATION)
            .open(addr)
        {
            Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY) && tries < 50 => {
                tries += 1;
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            other => return other,
        }
    }
}

#[cfg(not(any(unix, windows)))]
fn connect(_addr: &str) -> io::Result<std::fs::File> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "no local sockets on this platform",
    ))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn the_parent_is_recorded() {
        let parent = Parent::at_start();
        assert_eq!(parent.pid, rustix::process::getppid());
    }

    /// A process that is not (or no longer) our parent is never signalled: here, ourselves.
    #[test]
    fn only_the_parent_is_ever_signalled() {
        let me = Parent {
            pid: Some(rustix::process::getpid()),
        };
        assert!(!me.stop_ssh());
        assert!(!Parent { pid: None }.stop_ssh());
    }
}
