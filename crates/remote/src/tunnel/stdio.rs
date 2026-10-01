//! One connection through the stdio bridge: `ssh <host> exec <pitcrewd> connect --socket <path>`
//! (or, for a job whose node takes no ssh, `… exec srun --jobid <id> --overlap … <pitcrewd>
//! connect …` on the login node), its stdin and stdout being the connection.
//!
//! On Unix the call is a client of the link's ControlMaster: a channel, never a login. On
//! Windows each one logs in (OpenSSH there has no ControlMaster), with prompts going to the
//! askpass bridge as for any call.
//!
//! Whatever the login shell prints before the bridge starts is skipped: the connection begins
//! after the bridge's [`READY`] mark. If the mark does not come, the bridge's exit code and its
//! last line on stderr say why.

use super::TunnelError;
use crate::askpass::server::AskpassServer;
use crate::bridge::{EXIT_NO_DAEMON, EXIT_UNSAFE, EXIT_USAGE, READY};
use crate::ssh::{Running, SshLog, classify_failure, expire, last_line};
use crate::{Ssh, SshError};
use std::io;
use std::path::Path;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, ReadBuf};
use tokio::process::{ChildStdin, ChildStdout};

/// The most a login shell may print before the bridge's mark.
const MAX_PREAMBLE: usize = 64 * 1024;

/// One connection's ssh, and its stdin and stdout as a byte stream. Dropping it stops ssh.
#[derive(Debug)]
pub(crate) struct StdioStream {
    /// Kills ssh when dropped.
    _proc: Running,
    stdin: Option<ChildStdin>,
    stdout: ChildStdout,
    /// What came after the mark in the read that found it.
    pending: Vec<u8>,
    pos: usize,
    _askpass: Option<AskpassServer>,
    _log: SshLog,
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
    /// The command on the host, quoted by [`crate::quote::remote_command`].
    pub(crate) argv: Vec<String>,
    /// Options before `--` (`-J <login>` on Windows).
    pub(crate) extra: Vec<String>,
    /// How long the mark may take, not counting time spent on prompts.
    pub(crate) wait: Duration,
}

/// Starts the command and waits for the bridge's mark.
///
/// # Errors
/// - [`TunnelError::Refused`]: the bridge refused the socket (it is not this user's alone), or
///   the helper has no `connect` command;
/// - [`TunnelError::NoDaemon`]: nothing listens on the socket, or the helper is missing;
/// - [`TunnelError::Ssh`]: ssh failed (with no master, a call through it fails at once);
/// - [`TunnelError::Bridge`]: anything else, with what it said.
pub(crate) async fn open(opening: Opening<'_>) -> Result<StdioStream, TunnelError> {
    crate::quote::validate_host(opening.host)?;
    let remote = crate::quote::remote_command(&opening.argv)?;
    let log = SshLog::new(opening.dir)?;
    let args = opening.ssh.args(
        opening.dir,
        log.path(),
        opening.host,
        remote,
        &opening.extra,
    )?;
    let (mut proc, askpass) = opening.ssh.spawn(opening.dir, opening.host, args, true)?;
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
    let found = {
        let stopped = async {
            match &askpass {
                Some(server) => server.wait_stopped().await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            found = find_mark(&mut stdout) => found,
            why = stopped => Err(Some(why.error().into())),
            () = expire(opening.wait, open) => Err(Some(TunnelError::Bridge(format!(
                "the bridge did not start within {:?}",
                opening.wait
            )))),
        }
    };
    match found {
        Ok(pending) => Ok(StdioStream {
            _proc: proc,
            stdin: Some(stdin),
            stdout,
            pending,
            pos: 0,
            _askpass: askpass,
            _log: log,
        }),
        Err(Some(error)) => {
            proc.kill().await;
            Err(error)
        }
        Err(None) => {
            // stdout ended without the mark: ssh or the bridge exited. Their word says why.
            let status = tokio::time::timeout(Duration::from_secs(5), proc.child.wait()).await;
            proc.kill().await;
            let code = status.ok().and_then(Result::ok).and_then(|s| s.code());
            // Give the stderr reader a moment to catch the last line.
            tokio::time::sleep(Duration::from_millis(50)).await;
            Err(failure(&log.read(), &super::link::text(&kept), code))
        }
    }
}

/// Reads until [`READY`], returning what came after it. `Err(None)` at end of file.
async fn find_mark(stdout: &mut ChildStdout) -> Result<Vec<u8>, Option<TunnelError>> {
    let mut seen: Vec<u8> = Vec::new();
    let mut buf = vec![0u8; 16 * 1024];
    loop {
        let n = match stdout.read(&mut buf).await {
            Ok(0) => return Err(None),
            Ok(n) => n,
            Err(e) => return Err(Some(TunnelError::Io(e))),
        };
        seen.extend_from_slice(buf.get(..n).unwrap_or_default());
        if let Some(at) = seen.windows(READY.len()).position(|w| w == READY) {
            return Ok(seen.split_off(at + READY.len()));
        }
        if seen.len() > MAX_PREAMBLE + READY.len() {
            return Err(Some(TunnelError::Bridge(format!(
                "more than {MAX_PREAMBLE} bytes came before the bridge started: is something \
                 in the login shell's start-up files printing?"
            ))));
        }
    }
}

/// The error for a bridge that ended before its mark, from ssh's log, the exit code (the
/// bridge's, or the shell's) and the last line on stderr.
fn failure(log: &str, stderr: &str, code: Option<i32>) -> TunnelError {
    let said = last_line(stderr);
    let said = if said.is_empty() {
        format!("it exited with {code:?}")
    } else {
        said
    };
    if code == Some(255)
        && let Some(error) = classify_failure(log, format!("{log}{stderr}"))
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
        // The shell found no such program: the helper's version is gone.
        Some(126 | 127) => TunnelError::NoDaemon(format!("the helper is not there: {said}")),
        Some(255) => TunnelError::Ssh(SshError::Ssh {
            code: 255,
            stderr: format!("{log}{stderr}"),
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

impl AsyncRead for StdioStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if let Some(rest) = this.pending.get(this.pos..).filter(|r| !r.is_empty()) {
            let n = rest.len().min(buf.remaining());
            buf.put_slice(rest.get(..n).unwrap_or_default());
            this.pos += n;
            if this.pos >= this.pending.len() {
                this.pending = Vec::new();
                this.pos = 0;
            }
            return Poll::Ready(Ok(()));
        }
        Pin::new(&mut this.stdout).poll_read(cx, buf)
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
        let err = failure("", "sh: 1: /x/pitcrewd: not found\n", Some(127));
        assert!(matches!(err, TunnelError::NoDaemon(_)), "{err:?}");
        let err = failure(
            "kex_exchange_identification: Connection closed by remote host\n",
            "",
            Some(255),
        );
        assert!(
            matches!(err, TunnelError::Ssh(SshError::Unreachable { .. })),
            "{err:?}"
        );
        let err = failure("", "\x1b[31msomething\n", Some(1));
        assert!(
            matches!(&err, TunnelError::Bridge(why) if why == "?[31msomething"),
            "{err:?}"
        );
    }
}
