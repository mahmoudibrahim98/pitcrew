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
/// Exit status: the user cancelled.
pub const EXIT_CANCELLED: i32 = 1;
/// Exit status: the bridge failed (not set up, unreachable, or the server's proof was wrong).
pub const EXIT_FAILED: i32 = 2;

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
    let (Some(addr), Some(key)) = (
        var(ADDR_ENV),
        var(KEY_ENV).as_deref().and_then(key_from_hex),
    ) else {
        let _ = writeln!(
            err,
            "pitcrew-askpass: not started by PitCrew ({ADDR_ENV} unset)"
        );
        return EXIT_FAILED;
    };
    let reply = connect(&addr).and_then(|mut stream| ask(&mut stream, &key, kind, prompt));
    match reply {
        Ok(WireReply::Text { text }) => {
            let mut text = text.into_bytes();
            let written = out
                .write_all(&text)
                .and_then(|()| out.write_all(b"\n"))
                .and_then(|()| out.flush());
            text.fill(0);
            match written {
                Ok(()) => EXIT_ANSWERED,
                Err(e) => {
                    let _ = writeln!(err, "pitcrew-askpass: {e}");
                    EXIT_FAILED
                }
            }
        }
        // `yes` answers a host-key question; ssh also takes it for a confirmation.
        Ok(WireReply::Accept) => match writeln!(out, "yes").and_then(|()| out.flush()) {
            Ok(()) => EXIT_ANSWERED,
            Err(_) => EXIT_FAILED,
        },
        Ok(WireReply::Cancel) => EXIT_CANCELLED,
        Err(e) => {
            let _ = writeln!(err, "pitcrew-askpass: {e}");
            EXIT_FAILED
        }
    }
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

/// Reads one line, a byte at a time so nothing past it is consumed.
pub(crate) fn receive<T: serde::de::DeserializeOwned>(stream: &mut impl Read) -> io::Result<T> {
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        if stream.read(&mut byte)? == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "the listener closed the connection",
            ));
        }
        if byte[0] == b'\n' {
            break;
        }
        if line.len() >= MAX_LINE {
            return Err(bad("line too long"));
        }
        line.push(byte[0]);
    }
    serde_json::from_slice(&line).map_err(|e| bad(&e.to_string()))
}

#[cfg(unix)]
fn connect(addr: &str) -> io::Result<std::os::unix::net::UnixStream> {
    std::os::unix::net::UnixStream::connect(addr)
}

/// Opens the pipe as a file, retrying briefly while every instance is busy.
#[cfg(windows)]
fn connect(addr: &str) -> io::Result<std::fs::File> {
    const ERROR_PIPE_BUSY: i32 = 231;
    let mut tries = 0;
    loop {
        match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
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
