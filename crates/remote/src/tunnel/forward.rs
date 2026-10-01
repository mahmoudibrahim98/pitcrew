//! The forwarded socket (Unix): the link's master listens on a socket in the connector's private
//! directory and forwards each connection to the daemon's socket (`ssh -O forward -L
//! <local>:<remote>`), so a connection costs a channel and nothing more.
//!
//! The master binds it with `StreamLocalBindMask=0177` inside the 0700 directory. A site that
//! forbids forwarding unix sockets (`AllowStreamLocalForwarding no`) lets the master listen but
//! refuses every channel: ssh closes the connection at once, and logs "open failed:
//! administratively prohibited" (at `INFO`). [`check`] sends one HTTP request through it
//! (`GET /v1/host/info`, the API's one route without a token) and tells a refused forward
//! apart from a working one.

use super::link::{self, Link};
use crate::{Ssh, SshError};
use std::io;
use std::path::Path;
use std::time::Duration;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

/// What [`check`] sends: the API's route without a token, answered by any version.
pub(crate) const PROBE: &[u8] =
    b"GET /v1/host/info HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n";

/// Why a forward does not work.
#[derive(Debug)]
pub(crate) enum Broken {
    /// The site forbids forwarding unix sockets: ssh's log says so.
    Forbidden,
    /// The connection closed with no answer, for another reason (no daemon behind it, say).
    Closed,
    /// No answer in time.
    Silent,
    /// Something else failed.
    Failed(String),
}

impl std::fmt::Display for Broken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Forbidden => f.write_str("the site forbids forwarding unix sockets"),
            Self::Closed => f.write_str("the forward closed the connection unanswered"),
            Self::Silent => f.write_str("no answer through the forward"),
            Self::Failed(why) => f.write_str(why),
        }
    }
}

/// Adds the forward `local` → `remote` to `link`'s master.
///
/// # Errors
/// ssh refused (a path it cannot take, a bind that failed).
pub(crate) async fn add(
    ssh: &Ssh,
    dir: &Path,
    link: &Link,
    local: &Path,
    remote: &str,
) -> Result<(), SshError> {
    let spec = spec(local, remote)?;
    let Some(master) = link.control() else {
        return Err(SshError::InvalidArgument(
            "this link has no control socket".to_owned(),
        ));
    };
    let _ = std::fs::remove_file(local);
    link::control(ssh, dir, master, link.host(), "forward", &spec).await
}

/// `-L <local>:<remote>`, for paths ssh splits correctly and expands nothing in.
fn spec(local: &Path, remote: &str) -> Result<[String; 2], SshError> {
    let local = local.to_str().unwrap_or("");
    let fits = |p: &str| {
        p.starts_with('/')
            && !p
                .chars()
                .any(|c| c.is_whitespace() || c.is_control() || matches!(c, ':' | '%' | '$' | '~'))
    };
    if !fits(local) || !fits(remote) {
        return Err(SshError::InvalidArgument(
            "a socket path ssh cannot forward".to_owned(),
        ));
    }
    Ok(["-L".to_owned(), format!("{local}:{remote}")])
}

/// Sends [`PROBE`] through the forward at `local` and waits up to `wait` for an HTTP answer.
/// The link's log, from `log_from` on, tells a forbidden forward from other failures.
pub(crate) async fn check(
    local: &Path,
    link: &Link,
    log_from: u64,
    wait: Duration,
) -> Result<(), Broken> {
    let answered = tokio::time::timeout(wait, ask(local)).await;
    let broken = match answered {
        Ok(Ok(true)) => return Ok(()),
        Ok(Ok(false)) => Broken::Closed,
        Ok(Err(e)) => Broken::Failed(e.to_string()),
        Err(_) => Broken::Silent,
    };
    // ssh logs the refusal just as it closes the connection: give it a moment.
    for _ in 0..10 {
        if forbidden(&link.log_since(log_from)) {
            return Err(Broken::Forbidden);
        }
        if !matches!(broken, Broken::Closed) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    Err(broken)
}

/// One request: `true` if an HTTP answer starts, `false` if the connection closed first.
async fn ask(local: &Path) -> io::Result<bool> {
    let mut stream = tokio::net::UnixStream::connect(local).await?;
    stream.write_all(PROBE).await?;
    let mut head = [0u8; 5];
    let mut got = 0;
    while got < head.len() {
        let Some(rest) = head.get_mut(got..) else {
            break;
        };
        match stream.read(rest).await? {
            0 => return Ok(false),
            n => got += n,
        }
    }
    Ok(&head == b"HTTP/")
}

/// Whether ssh logged a refused forwarding channel, read as ssh's own format: the reason text
/// before the server's message is ssh's.
pub(crate) fn forbidden(log: &str) -> bool {
    log.lines().any(|line| {
        line.strip_prefix("channel ")
            .and_then(|rest| rest.split_once(": open failed: "))
            .is_some_and(|(id, why)| {
                !id.is_empty()
                    && id.bytes().all(|b| b.is_ascii_digit())
                    && why.starts_with("administratively prohibited")
            })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forbidden_channels_are_read_from_ssh_alone() {
        assert!(forbidden(
            "Authenticated to x\nchannel 3: open failed: administratively prohibited: open \
             failed\n"
        ));
        assert!(!forbidden(
            "channel 3: open failed: connect failed: No such file or directory\n"
        ));
        // Server text inside another line does not count.
        assert!(!forbidden(
            "Received disconnect: channel 3: open failed: administratively prohibited\n"
        ));
        assert!(!forbidden(
            "channel x: open failed: administratively prohibited\n"
        ));
    }

    #[test]
    fn forward_specs() {
        let spec = spec(
            Path::new("/run/user/1000/pitcrew-ssh/t1/fwd"),
            "/home/sam/.pitcrew/run/pitcrewd.sock",
        )
        .unwrap();
        assert_eq!(
            spec,
            [
                "-L",
                "/run/user/1000/pitcrew-ssh/t1/fwd:/home/sam/.pitcrew/run/pitcrewd.sock"
            ]
        );
        for (local, remote) in [
            ("/tmp/a:b/fwd", "/x/pitcrewd.sock"),
            ("/tmp/fwd", "/x:y/pitcrewd.sock"),
            ("/tmp/%d/fwd", "/x/pitcrewd.sock"),
            ("relative", "/x/pitcrewd.sock"),
        ] {
            assert!(
                super::spec(Path::new(local), remote).is_err(),
                "{local} {remote}"
            );
        }
    }
}
