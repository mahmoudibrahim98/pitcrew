//! Where a local daemon listens, and a connection to it that is checked before any token is sent
//! (`pitcrew_api::client`):
//!
//! - Unix: the socket's directory is ours and 0700 and the socket is ours, before connecting; the
//!   process at the other end runs as us, after.
//! - Windows: the pipe is opened at identification level (the server learns who we are but cannot
//!   act as us), then its owner must be us. A pipe name can be taken by anyone while the daemon is
//!   down, so this check, not the name, is what makes the pipe trustworthy.

use std::io;
use std::path::Path;
#[cfg(unix)]
use std::path::PathBuf;
use tokio::io::{AsyncRead, AsyncWrite};

/// A byte stream to a daemon.
pub trait Io: AsyncRead + AsyncWrite + Unpin + Send + 'static {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send + 'static> Io for T {}

/// A boxed [`Io`].
pub type BoxIo = Box<dyn Io>;

/// A daemon's private transport.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Endpoint {
    /// The directory of a unix socket named `pitcrewd.sock`.
    #[cfg(unix)]
    Unix {
        /// The socket's directory, `<state dir>/run` for `pitcrewd serve --listen private`.
        dir: PathBuf,
    },
    /// A local named pipe, `\\.\pipe\…`.
    #[cfg(windows)]
    Pipe {
        /// The full name.
        name: String,
    },
}

/// Why a connection failed.
#[derive(Debug, thiserror::Error)]
pub enum ConnectError {
    /// Nothing listens there: the daemon is not running.
    #[error("pitcrewd is not running at {at} ({source})")]
    NotRunning {
        /// Where.
        at: String,
        /// The OS error.
        source: io::Error,
    },
    /// Something listens there, but it is not provably ours; no token is sent to it.
    #[error("not sending a token to {at}: {reason}")]
    Untrusted {
        /// Where.
        at: String,
        /// Why.
        reason: String,
    },
    /// Anything else.
    #[error("cannot connect to {at}: {source}")]
    Failed {
        /// Where.
        at: String,
        /// The OS error.
        source: io::Error,
    },
}

impl Endpoint {
    /// Where `pitcrewd --state-dir <state_dir> serve --listen private` listens: the socket in
    /// `<state_dir>/run` on Unix; on Windows the current user's pipe, whatever the directory.
    ///
    /// # Errors
    /// On Windows, the current user's SID cannot be read.
    pub fn private_default(state_dir: &Path) -> io::Result<Self> {
        #[cfg(unix)]
        {
            Ok(Self::Unix {
                dir: state_dir.join("run"),
            })
        }
        #[cfg(windows)]
        {
            let _ = state_dir;
            Ok(Self::Pipe {
                name: pitcrew_api::default_pipe_name()?,
            })
        }
    }

    /// Where it is, for people and logs.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            #[cfg(unix)]
            Self::Unix { dir } => dir.join(pitcrew_api::SOCKET_NAME).display().to_string(),
            #[cfg(windows)]
            Self::Pipe { name } => name.clone(),
        }
    }

    /// Connects and checks that the daemon is ours.
    ///
    /// # Errors
    /// See [`ConnectError`].
    pub async fn connect(&self) -> Result<BoxIo, ConnectError> {
        match self {
            #[cfg(unix)]
            Self::Unix { dir } => connect_unix(dir, self.describe()).await,
            #[cfg(windows)]
            Self::Pipe { name } => connect_pipe(name).await,
        }
    }
}

#[cfg(unix)]
async fn connect_unix(dir: &Path, at: String) -> Result<BoxIo, ConnectError> {
    let path = pitcrew_api::client::check_unix_socket(dir).map_err(|e| match e.kind() {
        io::ErrorKind::NotFound => ConnectError::NotRunning {
            at: at.clone(),
            source: e,
        },
        io::ErrorKind::PermissionDenied => ConnectError::Untrusted {
            at: at.clone(),
            reason: e.to_string(),
        },
        _ => ConnectError::Failed {
            at: at.clone(),
            source: e,
        },
    })?;
    let stream = tokio::net::UnixStream::connect(&path)
        .await
        .map_err(|e| match e.kind() {
            io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused => {
                ConnectError::NotRunning {
                    at: at.clone(),
                    source: e,
                }
            }
            _ => ConnectError::Failed {
                at: at.clone(),
                source: e,
            },
        })?;
    pitcrew_api::client::check_unix_peer(&stream).map_err(|e| ConnectError::Untrusted {
        at,
        reason: e.to_string(),
    })?;
    Ok(Box::new(stream))
}

#[cfg(windows)]
async fn connect_pipe(name: &str) -> Result<BoxIo, ConnectError> {
    use std::time::{Duration, Instant};
    use tokio::net::windows::named_pipe::ClientOptions;
    // winerror.h
    const ERROR_FILE_NOT_FOUND: i32 = 2;
    const ERROR_PIPE_BUSY: i32 = 231;
    // winbase.h: SecurityIdentification << 16. tokio adds SECURITY_SQOS_PRESENT.
    const SECURITY_IDENTIFICATION: u32 = 0x0001_0000;

    let at = name.to_owned();
    let deadline = Instant::now() + Duration::from_secs(2);
    let client = loop {
        match ClientOptions::new()
            .security_qos_flags(SECURITY_IDENTIFICATION)
            .open(name)
        {
            Ok(client) => break client,
            // Every instance is taken; the daemon opens a new one right after each connection.
            Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY) && Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            Err(e) if e.raw_os_error() == Some(ERROR_FILE_NOT_FOUND) => {
                return Err(ConnectError::NotRunning { at, source: e });
            }
            Err(e) => return Err(ConnectError::Failed { at, source: e }),
        }
    };
    pitcrew_api::client::check_pipe_server(&client).map_err(|e| ConnectError::Untrusted {
        at: at.clone(),
        reason: e.to_string(),
    })?;
    Ok(Box::new(client))
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    fn private_dir(root: &Path) -> PathBuf {
        let dir = root.join("run");
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        dir
    }

    #[tokio::test]
    async fn our_own_socket_connects() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = private_dir(tmp.path());
        let _listener = tokio::net::UnixListener::bind(dir.join("pitcrewd.sock")).unwrap();
        let endpoint = Endpoint::Unix { dir };
        assert!(endpoint.connect().await.is_ok());
    }

    #[tokio::test]
    async fn no_socket_means_not_running() {
        let tmp = tempfile::tempdir().unwrap();
        let endpoint = Endpoint::private_default(tmp.path()).unwrap();
        assert!(matches!(
            endpoint.connect().await,
            Err(ConnectError::NotRunning { .. })
        ));
        // A stale socket file whose daemon is gone.
        let dir = private_dir(tmp.path());
        drop(std::os::unix::net::UnixListener::bind(dir.join("pitcrewd.sock")).unwrap());
        assert!(matches!(
            endpoint.connect().await,
            Err(ConnectError::NotRunning { .. })
        ));
    }

    #[tokio::test]
    async fn an_open_directory_is_untrusted() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = private_dir(tmp.path());
        let _listener = tokio::net::UnixListener::bind(dir.join("pitcrewd.sock")).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        let err = Endpoint::Unix { dir }.connect().await.err().unwrap();
        assert!(matches!(err, ConnectError::Untrusted { .. }), "{err}");
    }
}
