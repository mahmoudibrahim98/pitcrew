//! Blocking connections to the daemon, checked before any token is sent.
//!
//! - Unix socket: `pitcrew_api::client::check_unix_socket` (the directory is ours and 0700, the
//!   socket is ours) before connecting, then the peer's uid (`SO_PEERCRED`) after. On Unix
//!   systems other than Linux the peer check is skipped: the directory and owner checks already
//!   mean only our user could have bound that socket.
//! - Named pipe: opened at identification-level impersonation (the server can learn who we are
//!   but cannot act as us), then `pitcrew_api::client::check_pipe_server`: the pipe's owner SID
//!   matches the current user (or our token's default owner, if elevated).
//! - Loopback TCP: no identity check is possible, which is why it is for development only.

use crate::config::Endpoint;
use crate::error::{Error, Kind, Result};
use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::time::Duration;

/// How long to wait.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timeouts {
    /// To connect (TCP), or to wait for a free pipe instance.
    pub connect: Duration,
    /// For each read and write (sockets only; pipes rely on the caller's deadline).
    pub io: Duration,
}

impl Timeouts {
    /// For verbs: generous, since a person or agent is waiting for the answer anyway.
    pub const VERB: Self = Self {
        connect: Duration::from_secs(2),
        io: Duration::from_secs(20),
    };
    /// For the hook: short, so a stuck daemon never holds up the agent.
    pub const HOOK: Self = Self {
        connect: Duration::from_millis(100),
        io: Duration::from_millis(250),
    };
}

/// An open, checked connection.
#[derive(Debug)]
pub enum Conn {
    /// A unix socket.
    #[cfg(unix)]
    Unix(std::os::unix::net::UnixStream),
    /// A named pipe, opened as a file.
    #[cfg(windows)]
    Pipe(std::fs::File),
    /// Loopback TCP.
    Tcp(TcpStream),
}

impl Read for Conn {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            #[cfg(unix)]
            Self::Unix(s) => s.read(buf),
            #[cfg(windows)]
            Self::Pipe(f) => f.read(buf),
            Self::Tcp(s) => s.read(buf),
        }
    }
}

impl Write for Conn {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            #[cfg(unix)]
            Self::Unix(s) => s.write(buf),
            #[cfg(windows)]
            Self::Pipe(f) => f.write(buf),
            Self::Tcp(s) => s.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            #[cfg(unix)]
            Self::Unix(s) => s.flush(),
            #[cfg(windows)]
            Self::Pipe(f) => f.flush(),
            Self::Tcp(s) => s.flush(),
        }
    }
}

impl Endpoint {
    /// Connects and checks that the server is who it should be.
    ///
    /// # Errors
    /// `unavailable` when nothing answers; `untrusted` when the server fails the identity check;
    /// `invalid` when the address is unusable.
    pub fn connect(&self, timeouts: Timeouts) -> Result<Conn> {
        match self {
            #[cfg(unix)]
            Self::Unix { dir } => connect_unix(dir, timeouts),
            #[cfg(windows)]
            Self::Pipe { name } => connect_pipe(name, timeouts),
            Self::Tcp { addrs, .. } => connect_tcp(self, addrs, timeouts),
        }
    }
}

fn unreachable_daemon(at: &str, e: &io::Error) -> Error {
    Error::new(
        Kind::Unavailable,
        format!("cannot reach the daemon at {at}: {e}. Is pitcrewd running?"),
    )
}

#[cfg(unix)]
fn connect_unix(dir: &std::path::Path, timeouts: Timeouts) -> Result<Conn> {
    use std::os::unix::net::UnixStream;
    let at = dir.join(pitcrew_api::SOCKET_NAME).display().to_string();
    let path = pitcrew_api::client::check_unix_socket(dir).map_err(|e| match e.kind() {
        io::ErrorKind::NotFound => unreachable_daemon(&at, &e),
        io::ErrorKind::PermissionDenied => Error::new(
            Kind::Untrusted,
            format!("not sending the token to {at}: {e}"),
        ),
        _ => Error::invalid(format!(
            "PITCREW_SOCKET must be the daemon's socket or its directory: {e}"
        )),
    })?;
    let stream = UnixStream::connect(&path).map_err(|e| unreachable_daemon(&at, &e))?;
    stream
        .set_read_timeout(Some(timeouts.io))
        .and_then(|()| stream.set_write_timeout(Some(timeouts.io)))
        .map_err(|e| Error::internal(format!("cannot set timeouts on {at}: {e}")))?;
    check_peer(&stream, &at)?;
    Ok(Conn::Unix(stream))
}

/// The process on the other end runs as us.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn check_peer(stream: &std::os::unix::net::UnixStream, at: &str) -> Result<()> {
    let uid = rustix::net::sockopt::socket_peercred(stream)
        .map_err(|e| {
            Error::new(
                Kind::Untrusted,
                format!(
                    "not sending the token to {at}: cannot read the daemon's credentials: {}",
                    io::Error::from(e)
                ),
            )
        })?
        .uid
        .as_raw();
    if uid == pitcrew_auth::euid() {
        Ok(())
    } else {
        Err(Error::new(
            Kind::Untrusted,
            format!("not sending the token to {at}: the daemon runs as another user (uid {uid})"),
        ))
    }
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
fn check_peer(_stream: &std::os::unix::net::UnixStream, _at: &str) -> Result<()> {
    Ok(())
}

#[cfg(windows)]
fn connect_pipe(name: &str, timeouts: Timeouts) -> Result<Conn> {
    use std::os::windows::fs::OpenOptionsExt as _;
    use std::time::Instant;
    // winbase.h: SECURITY_SQOS_PRESENT, and SecurityIdentification << 16.
    const SECURITY_SQOS_PRESENT: u32 = 0x0010_0000;
    const SECURITY_IDENTIFICATION: u32 = 0x0001_0000;
    const ERROR_PIPE_BUSY: i32 = 231;

    let deadline = Instant::now() + timeouts.connect;
    let pipe = loop {
        let opened = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .security_qos_flags(SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION)
            .open(name);
        match opened {
            Ok(pipe) => break pipe,
            // Every instance is taken; the daemon opens a new one right after each connection.
            Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY) && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(2));
            }
            Err(e) => return Err(unreachable_daemon(name, &e)),
        }
    };
    pitcrew_api::client::check_pipe_server(&pipe).map_err(|e| {
        Error::new(
            Kind::Untrusted,
            format!("not sending the token to {name}: {e}"),
        )
    })?;
    Ok(Conn::Pipe(pipe))
}

fn connect_tcp(endpoint: &Endpoint, addrs: &[std::net::SocketAddr], t: Timeouts) -> Result<Conn> {
    let mut last = io::Error::new(io::ErrorKind::NotFound, "no address to try");
    for addr in addrs {
        match TcpStream::connect_timeout(addr, t.connect) {
            Ok(stream) => {
                stream
                    .set_nodelay(true)
                    .and_then(|()| stream.set_read_timeout(Some(t.io)))
                    .and_then(|()| stream.set_write_timeout(Some(t.io)))
                    .map_err(|e| Error::internal(format!("cannot configure the socket: {e}")))?;
                return Ok(Conn::Tcp(stream));
            }
            Err(e) => last = e,
        }
    }
    Err(unreachable_daemon(&endpoint.describe(), &last))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    #[test]
    fn our_own_socket_passes() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("run");
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        let _listener =
            std::os::unix::net::UnixListener::bind(dir.join(pitcrew_api::SOCKET_NAME)).unwrap();
        let conn = Endpoint::Unix { dir }.connect(Timeouts::HOOK).unwrap();
        assert!(matches!(conn, Conn::Unix(_)));
    }

    #[test]
    fn an_open_directory_is_refused_before_connecting() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("run");
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        let listener =
            std::os::unix::net::UnixListener::bind(dir.join(pitcrew_api::SOCKET_NAME)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let err = Endpoint::Unix { dir }.connect(Timeouts::HOOK).unwrap_err();
        assert_eq!(err.kind, Kind::Untrusted, "{err}");
        // Nothing connected.
        assert!(listener.accept().is_err());
    }

    #[test]
    fn a_missing_socket_is_unavailable() {
        let tmp = tempfile::tempdir().unwrap();
        let err = Endpoint::Unix {
            dir: tmp.path().join("nothing"),
        }
        .connect(Timeouts::HOOK)
        .unwrap_err();
        assert_eq!(err.kind, Kind::Unavailable);
    }
}
