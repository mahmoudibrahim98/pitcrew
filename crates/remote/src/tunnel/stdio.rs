//! One connection through the stdio bridge: `ssh <host> exec <pitcrewd> connect --socket <path>
//! --nonce <hex>` (or, for a job whose node takes no ssh, `… exec srun --jobid <id> --overlap …
//! <pitcrewd> connect … --framed`, on the login node), its stdin and stdout being the
//! connection.
//!
//! On Unix the call is a client of the link's ControlMaster: a channel (a session, which sshd's
//! `MaxSessions` counts), never a login. On Windows each one logs in (OpenSSH there has no
//! ControlMaster), with prompts going to the askpass bridge as for any call.
//!
//! Whatever the login shell prints before the bridge starts is skipped: the connection begins
//! after the bridge's ready mark, which carries this call's random nonce. If the mark does not
//! come, the bridge's exit code and its last line on stderr say why. Through `srun` the bridge
//! frames its output ([`crate::bridge`]), and this end takes the frames apart.
//!
//! The ssh behind a connection belongs to a task that stops it (and what it started) when the
//! stream is dropped, or when the connector closes.

use super::TunnelError;
use crate::askpass::server::AskpassServer;
use crate::bridge::{EXIT_NO_DAEMON, EXIT_UNSAFE, EXIT_USAGE, FRAME_HEADER, ready_mark};
use crate::ssh::{SshLog, classify_failure, expire, last_line};
use crate::{Ssh, SshError};
use std::io;
use std::path::Path;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, ReadBuf};
use tokio::process::{ChildStdin, ChildStdout};
use tokio::sync::{oneshot, watch};

/// The most a login shell may print before the bridge's mark.
const MAX_PREAMBLE: usize = 64 * 1024;

/// The largest frame accepted (the bridge sends at most 64 KiB).
const MAX_FRAME: usize = 1024 * 1024;

/// One connection's stdin and stdout as a byte stream. Dropping it stops its ssh.
#[derive(Debug)]
pub(crate) struct StdioStream {
    stdin: Option<ChildStdin>,
    stdout: ChildStdout,
    read: Buffered,
    /// Dropped with the stream: the task that owns ssh then stops it.
    _owner: oneshot::Sender<()>,
}

/// What was read from stdout and not handed out yet, and the frames' state.
#[derive(Debug, Default)]
struct Buffered {
    /// Bytes read from stdout, not yet handed out (after the mark; still framed if framed).
    raw: Vec<u8>,
    pos: usize,
    /// Framed (through srun): where in the frames the reading is.
    frames: Option<Frames>,
}

/// Where the decoding of the bridge's frames stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Frames {
    /// Before a header.
    Header,
    /// Inside a frame, with this many bytes of it to come.
    Body(usize),
    /// After a frame's bytes, before its `\n`; `true` for the end mark.
    Trailer(bool),
    /// After the end mark.
    End,
}

/// What to run.
#[derive(Debug)]
pub(crate) struct Opening<'a> {
    /// How to call ssh: through the link's master (Unix), or with prompts (Windows).
    pub(crate) ssh: &'a Ssh,
    /// Where ssh's log and the prompt bridge go.
    pub(crate) dir: &'a Path,
    /// The host, as given to ssh.
    pub(crate) host: &'a str,
    /// The command on the host, quoted by [`crate::quote::remote_command`]; `--nonce` (and
    /// `--framed`) are added.
    pub(crate) argv: Vec<String>,
    /// Ask the bridge for frames (through srun).
    pub(crate) framed: bool,
    /// Options before `--` (`-J <login>` on Windows).
    pub(crate) extra: Vec<String>,
    /// How long the mark may take, not counting time spent on prompts.
    pub(crate) wait: Duration,
    /// Turns `true` when the connector closes: the connection's ssh is stopped then.
    pub(crate) closing: watch::Receiver<bool>,
}

/// Starts the command and waits for the bridge's mark.
///
/// # Errors
/// - [`TunnelError::Refused`]: the bridge refused the socket (it is not this user's alone), or
///   the helper has no `connect` command;
/// - [`TunnelError::NoDaemon`]: nothing listens on the socket, or the helper is missing;
/// - [`TunnelError::Ssh`]: ssh failed ([`SshError::SessionRefused`] when the server allows no
///   more sessions; with no master, a call through it fails at once);
/// - [`TunnelError::Bridge`]: anything else, with what it said.
pub(crate) async fn open(opening: Opening<'_>) -> Result<StdioStream, TunnelError> {
    opening.ssh.validate_destination(opening.host)?;
    let nonce = crate::askpass::to_hex(&crate::askpass::random::<8>().map_err(SshError::Setup)?);
    let mut argv = opening.argv;
    argv.extend(["--nonce".to_owned(), nonce.clone()]);
    if opening.framed {
        argv.push("--framed".to_owned());
    }
    let remote = crate::quote::remote_command(&argv)?;
    let log = SshLog::new(opening.dir)?;
    let args = opening.ssh.args(
        opening.dir,
        log.path(),
        opening.host,
        remote,
        &opening.extra,
    )?;
    let (mut proc, askpass) = opening
        .ssh
        .spawn(opening.dir, opening.host, args, true, &[])?;
    let (Some(stdin), Some(mut stdout), Some(stderr)) = (
        proc.child.stdin.take(),
        proc.child.stdout.take(),
        proc.child.stderr.take(),
    ) else {
        return Err(TunnelError::Io(io::Error::other(
            "ssh's stdio is not piped",
        )));
    };
    let kept = keep_tail(stderr);
    let open = askpass.as_ref().map(AskpassServer::open_prompts);
    let mark = ready_mark(Some(&nonce));
    let found = {
        let stopped = async {
            match &askpass {
                Some(server) => server.wait_stopped().await,
                None => std::future::pending().await,
            }
        };
        let mut closing = opening.closing.clone();
        tokio::select! {
            found = find_mark(&mut stdout, &mark) => found,
            why = stopped => Err(Some(why.error().into())),
            () = expire(opening.wait, open) => Err(Some(TunnelError::Bridge(format!(
                "the bridge did not start within {:?}",
                opening.wait
            )))),
            _ = closing.wait_for(|c| *c) => Err(Some(TunnelError::NotConnected(
                super::LinkState::Closed,
            ))),
        }
    };
    match found {
        Ok(raw) => {
            let (owner, dropped) = oneshot::channel::<()>();
            let mut closing = opening.closing;
            // Owns ssh (and the prompt bridge and its log) until the stream or the connector
            // goes; it stops ssh then, unless it ended by itself.
            tokio::spawn(async move {
                let _askpass = askpass;
                let _log = log;
                tokio::select! {
                    _ = proc.child.wait() => return,
                    _ = dropped => {}
                    _ = closing.wait_for(|c| *c) => {}
                }
                proc.kill().await;
            });
            Ok(StdioStream {
                stdin: Some(stdin),
                stdout,
                read: Buffered {
                    raw,
                    pos: 0,
                    frames: opening.framed.then_some(Frames::Header),
                },
                _owner: owner,
            })
        }
        Err(Some(error)) => {
            proc.kill().await;
            Err(error)
        }
        Err(None) => {
            // stdout ended without the mark: ssh or the bridge exited. A cancelled prompt
            // (askpass then stops ssh) says why first; else their word does.
            if let Some(why) = askpass.as_ref().and_then(AskpassServer::stopped) {
                proc.kill().await;
                return Err(why.error().into());
            }
            let status = tokio::time::timeout(Duration::from_secs(5), proc.child.wait()).await;
            proc.kill().await;
            let code = status.ok().and_then(Result::ok).and_then(|s| s.code());
            // Give the stderr reader a moment to catch the last line.
            tokio::time::sleep(Duration::from_millis(50)).await;
            Err(failure(&log.read(), &super::link::text(&kept), code))
        }
    }
}

/// Reads until `mark`, returning what came after it. `Err(None)` at end of file.
async fn find_mark(stdout: &mut ChildStdout, mark: &[u8]) -> Result<Vec<u8>, Option<TunnelError>> {
    let mut seen: Vec<u8> = Vec::new();
    let mut buf = vec![0u8; 16 * 1024];
    loop {
        let n = match stdout.read(&mut buf).await {
            Ok(0) => return Err(None),
            Ok(n) => n,
            Err(e) => return Err(Some(TunnelError::Io(e))),
        };
        seen.extend_from_slice(buf.get(..n).unwrap_or_default());
        if let Some(at) = seen.windows(mark.len()).position(|w| w == mark) {
            return Ok(seen.split_off(at + mark.len()));
        }
        if seen.len() > MAX_PREAMBLE + mark.len() {
            return Err(Some(TunnelError::Bridge(format!(
                "more than {MAX_PREAMBLE} bytes came before the bridge started: is something \
                 in the login shell's start-up files printing?"
            ))));
        }
    }
}

/// The error for a bridge that ended before its mark, from ssh's log, the exit code (the
/// bridge's, or the shell's) and the last line on stderr, with no paths in it.
fn failure(log: &str, stderr: &str, code: Option<i32>) -> TunnelError {
    let said = super::scrub(&last_line(stderr));
    let said = if said.is_empty() {
        format!("it exited with {code:?}")
    } else {
        said
    };
    if code == Some(255)
        && let Some(error) = classify_failure(log, super::scrub(&format!("{log}{stderr}")))
    {
        return TunnelError::Ssh(error);
    }
    match code.and_then(|c| u8::try_from(c).ok()) {
        Some(EXIT_UNSAFE) => TunnelError::Refused(said),
        Some(EXIT_USAGE) => TunnelError::Refused(format!(
            "the helper's `pitcrewd connect` did not take the call (is it older than this \
             app?): {said}"
        )),
        Some(EXIT_NO_DAEMON) => TunnelError::NoDaemon(said),
        // The shell found no such program: the helper's version is gone. (Its line names the
        // path, so it is left out.)
        Some(126 | 127) => TunnelError::NoDaemon("the helper is not there".to_owned()),
        Some(255) => TunnelError::Ssh(SshError::Ssh {
            code: 255,
            stderr: super::scrub(&format!("{log}{stderr}")),
        }),
        _ => TunnelError::Bridge(said),
    }
}

/// Reads `stderr` to its end in the background, keeping the last 4 KiB.
fn keep_tail(mut stderr: tokio::process::ChildStderr) -> Arc<Mutex<Vec<u8>>> {
    let kept = Arc::new(Mutex::new(Vec::new()));
    let keep = kept.clone();
    tokio::spawn(async move {
        let mut buf = [0u8; 4096];
        loop {
            let n = match stderr.read(&mut buf).await {
                Ok(n) if n > 0 => n,
                _ => break,
            };
            if let (Ok(mut kept), Some(chunk)) = (keep.lock(), buf.get(..n)) {
                kept.extend_from_slice(chunk);
                let excess = kept.len().saturating_sub(4096);
                kept.drain(..excess);
            }
        }
    });
    kept
}

/// What decoding produced.
enum Decoded {
    /// Bytes went into the buffer.
    Bytes,
    /// The end mark: end of file.
    End,
    /// More bytes are needed.
    More,
}

impl Buffered {
    /// The bytes not yet decoded.
    fn rest(&self) -> &[u8] {
        self.raw.get(self.pos..).unwrap_or_default()
    }

    fn consume(&mut self, n: usize) {
        self.pos += n;
        if self.pos >= self.raw.len() {
            self.raw.clear();
            self.pos = 0;
        }
    }

    /// Adds bytes read, dropping what was decoded first: the buffer holds no more than what
    /// was read last and a partial frame.
    fn push(&mut self, bytes: &[u8]) {
        self.raw.drain(..self.pos.min(self.raw.len()));
        self.pos = 0;
        self.raw.extend_from_slice(bytes);
    }

    /// Plain: hands out what came after the mark; `false` once there is none left.
    fn plain(&mut self, buf: &mut ReadBuf<'_>) -> bool {
        if self.rest().is_empty() {
            return false;
        }
        let n = self.rest().len().min(buf.remaining());
        buf.put_slice(self.rest().get(..n).unwrap_or_default());
        self.consume(n);
        true
    }

    /// Takes frames apart into `buf`, as far as the bytes read allow. `buf` must have room.
    fn decode(&mut self, buf: &mut ReadBuf<'_>) -> io::Result<Decoded> {
        let bad = |why: &str| io::Error::new(io::ErrorKind::InvalidData, why.to_owned());
        let mut state = self.frames.unwrap_or(Frames::End);
        let decoded = loop {
            match state {
                Frames::End => break Decoded::End,
                Frames::Header => {
                    let Some(header) = self.rest().get(..FRAME_HEADER) else {
                        break Decoded::More;
                    };
                    let len = std::str::from_utf8(header)
                        .ok()
                        .and_then(|h| h.strip_suffix(':'))
                        .filter(|h| h.bytes().all(|b| b.is_ascii_hexdigit()))
                        .and_then(|h| usize::from_str_radix(h, 16).ok())
                        .filter(|len| *len <= MAX_FRAME)
                        .ok_or_else(|| bad("a frame from the bridge has a bad header"))?;
                    self.consume(FRAME_HEADER);
                    state = if len == 0 {
                        Frames::Trailer(true)
                    } else {
                        Frames::Body(len)
                    };
                }
                Frames::Body(left) => {
                    let n = left.min(self.rest().len()).min(buf.remaining());
                    if n == 0 {
                        break Decoded::More;
                    }
                    buf.put_slice(self.rest().get(..n).unwrap_or_default());
                    self.consume(n);
                    state = if left == n {
                        Frames::Trailer(false)
                    } else {
                        Frames::Body(left - n)
                    };
                    break Decoded::Bytes;
                }
                Frames::Trailer(end) => {
                    let Some(&byte) = self.rest().first() else {
                        break Decoded::More;
                    };
                    if byte != b'\n' {
                        return Err(bad("a frame from the bridge does not end its line"));
                    }
                    self.consume(1);
                    state = if end { Frames::End } else { Frames::Header };
                }
            }
        };
        self.frames = Some(state);
        Ok(decoded)
    }
}

impl AsyncRead for StdioStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if this.read.frames.is_none() {
            // Plain: what came after the mark first, then stdout as it is.
            if this.read.plain(buf) {
                return Poll::Ready(Ok(()));
            }
            return Pin::new(&mut this.stdout).poll_read(cx, buf);
        }
        if buf.remaining() == 0 {
            // Nothing could be handed out: no reading ahead.
            return Poll::Ready(Ok(()));
        }
        loop {
            match this.read.decode(buf)? {
                Decoded::Bytes | Decoded::End => return Poll::Ready(Ok(())),
                Decoded::More => {}
            }
            let mut chunk = [0u8; 16 * 1024];
            let mut read = ReadBuf::new(&mut chunk);
            match Pin::new(&mut this.stdout).poll_read(cx, &mut read) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Ready(Ok(())) => {}
            }
            if read.filled().is_empty() {
                // The output ended without the end mark: at a frame's boundary it is still an
                // end, else the stream was cut.
                return Poll::Ready(match this.read.frames {
                    Some(Frames::Header | Frames::End) => Ok(()),
                    _ => Err(io::ErrorKind::UnexpectedEof.into()),
                });
            }
            this.read.push(read.filled());
        }
    }
}

impl AsyncWrite for StdioStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match &mut self.get_mut().stdin {
            Some(stdin) => Pin::new(stdin).poll_write(cx, buf),
            None => Poll::Ready(Err(io::ErrorKind::BrokenPipe.into())),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match &mut self.get_mut().stdin {
            Some(stdin) => Pin::new(stdin).poll_flush(cx),
            None => Poll::Ready(Ok(())),
        }
    }

    /// Ends what is sent: ssh's stdin closes, and the daemon reads end of file, while its
    /// answer still comes.
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if let Some(stdin) = &mut this.stdin {
            let flushed = Pin::new(stdin).poll_shutdown(cx);
            if flushed.is_pending() {
                return flushed;
            }
            this.stdin = None;
        }
        Poll::Ready(Ok(()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Frames come apart whatever the pieces they arrive in and the room the reader has, and
    /// what is kept stays small.
    #[test]
    fn frames_are_taken_apart_in_any_pieces() {
        let framed = b"00000003:abc\n00000001:\n\n00000000:\n";
        for size in [1, 2, 5, 9, 100] {
            for room in [1, 2, 64] {
                let mut read = Buffered {
                    frames: Some(Frames::Header),
                    ..Buffered::default()
                };
                let mut pieces = framed.chunks(size);
                let mut out = Vec::new();
                loop {
                    let mut space = vec![0u8; room];
                    let mut buf = ReadBuf::new(&mut space);
                    match read.decode(&mut buf).unwrap() {
                        Decoded::Bytes => out.extend_from_slice(buf.filled()),
                        Decoded::End => break,
                        Decoded::More => read.push(pieces.next().expect("the end mark")),
                    }
                    assert!(read.raw.len() <= FRAME_HEADER + size, "{size} {room}");
                }
                assert_eq!(out, b"abc\n", "{size} {room}");
            }
        }
        for bad in [
            &b"0000000g:x\n"[..],
            b"00000001;x\n",
            b"00200000:",
            b"00000001:xy",
        ] {
            let mut read = Buffered {
                frames: Some(Frames::Header),
                ..Buffered::default()
            };
            read.push(bad);
            let mut space = [0u8; 8];
            let mut buf = ReadBuf::new(&mut space);
            let mut result = read.decode(&mut buf);
            if matches!(result, Ok(Decoded::Bytes)) {
                let mut more = [0u8; 8];
                result = read.decode(&mut ReadBuf::new(&mut more));
            }
            assert!(result.is_err(), "{bad:?}");
        }
    }

    #[test]
    fn failures_say_why() {
        let err = failure(
            "",
            "pitcrewd connect: the daemon's socket is not safe to use: x\n",
            Some(3),
        );
        assert!(
            matches!(&err, TunnelError::Refused(why) if why.ends_with(": x")),
            "{err:?}"
        );
        let err = failure(
            "",
            "pitcrewd connect: no daemon listens on the socket: y\n",
            Some(4),
        );
        assert!(matches!(err, TunnelError::NoDaemon(_)), "{err:?}");
        let err = failure("", "error: unrecognized subcommand 'connect'\n", Some(2));
        assert!(
            matches!(&err, TunnelError::Refused(why) if why.contains("older")),
            "{err:?}"
        );
        // The shell's line names the path: it is left out.
        let err = failure(
            "",
            "sh: 1: /home/sam/.pitcrew/bin/1.0/pitcrewd: not found\n",
            Some(127),
        );
        assert!(
            matches!(&err, TunnelError::NoDaemon(why) if !why.contains('/')),
            "{err:?}"
        );
        let err = failure(
            "kex_exchange_identification: Connection closed by remote host\n",
            "",
            Some(255),
        );
        assert!(
            matches!(err, TunnelError::Ssh(SshError::Unreachable { .. })),
            "{err:?}"
        );
        let err = failure(
            "mux_client_request_session: session request failed: Session open refused by peer\n\
             kex_exchange_identification: Connection closed by remote host\n",
            "",
            Some(255),
        );
        assert!(
            matches!(err, TunnelError::Ssh(SshError::SessionRefused { .. })),
            "{err:?}"
        );
        let err = failure("", "\x1b[31msomething at /home/sam/x\n", Some(1));
        assert!(
            matches!(&err, TunnelError::Bridge(why) if why == "?[31msomething at …"),
            "{err:?}"
        );
    }
}
