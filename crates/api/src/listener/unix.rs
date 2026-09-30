//! A unix socket in a private directory, accepting only the daemon's own user.

use pitcrew_auth::create_private_dir;
use std::io;
use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use tokio::net::{UnixListener, UnixStream, unix};

/// The socket's file name inside its directory.
pub const SOCKET_NAME: &str = "pitcrewd.sock";

/// A bound unix socket that drops connections from other users. Removes its socket file when
/// dropped.
#[derive(Debug)]
pub struct UnixSocket {
    listener: UnixListener,
    path: PathBuf,
    uid: u32,
}

impl UnixSocket {
    /// Makes `dir` private (0700), removes a stale socket, and binds `dir/pitcrewd.sock`.
    ///
    /// # Errors
    /// The directory cannot be made private (e.g. another user owns it); another daemon is
    /// listening on the socket; something other than a socket is in the way; binding fails.
    pub fn bind(dir: &Path) -> io::Result<Self> {
        create_private_dir(dir)?;
        let path = dir.join(SOCKET_NAME);
        remove_stale(&path)?;
        let listener = UnixListener::bind(&path)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        // We just created the socket, so its owner is our effective uid.
        let uid = std::fs::metadata(&path)?.uid();
        if std::fs::metadata(dir)?.uid() != uid {
            let _ = std::fs::remove_file(&path);
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("{} is owned by another user", dir.display()),
            ));
        }
        Ok(Self {
            listener,
            path,
            uid,
        })
    }

    /// The socket's path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Removes a socket left behind by a daemon that is gone. Refuses to touch a live socket or
/// anything that is not a socket.
fn remove_stale(path: &Path) -> io::Result<()> {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    if !meta.file_type().is_socket() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("{} exists and is not a socket", path.display()),
        ));
    }
    match std::os::unix::net::UnixStream::connect(path) {
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::AddrInUse,
            format!("another daemon is listening on {}", path.display()),
        )),
        Err(e) if e.kind() == io::ErrorKind::ConnectionRefused => {
            tracing::info!(path = %path.display(), "removing a stale socket");
            std::fs::remove_file(path)
        }
        Err(e) => Err(e),
    }
}

impl Drop for UnixSocket {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

impl axum::serve::Listener for UnixSocket {
    type Io = UnixStream;
    type Addr = unix::SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            match self.listener.accept().await {
                Ok((stream, addr)) => match stream.peer_cred() {
                    Ok(cred) if cred.uid() == self.uid => return (stream, addr),
                    Ok(cred) => {
                        tracing::warn!(
                            peer_uid = cred.uid(),
                            "dropped a connection from another user"
                        );
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "dropped a connection without peer credentials");
                    }
                },
                Err(e) => super::accept_error(e).await,
            }
        }
    }

    fn local_addr(&self) -> io::Result<Self::Addr> {
        self.listener.local_addr()
    }
}

/// Used by tests to pretend the daemon runs as another user.
#[cfg(test)]
impl UnixSocket {
    fn with_uid(mut self, uid: u32) -> Self {
        self.uid = uid;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::routing::get;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    #[tokio::test]
    async fn a_live_socket_is_not_replaced_and_a_stale_one_is() {
        let dir = tempfile::tempdir().unwrap();
        let run = dir.path().join("run");
        let first = UnixSocket::bind(&run).unwrap();
        let err = UnixSocket::bind(&run).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AddrInUse);

        // A socket file whose listener is gone.
        let path = first.path().to_path_buf();
        let listener = std::os::unix::net::UnixListener::bind(dir.path().join("x.sock")).unwrap();
        drop(first);
        std::fs::rename(dir.path().join("x.sock"), &path).unwrap();
        drop(listener);
        let second = UnixSocket::bind(&run).unwrap();
        assert!(second.path().exists());
    }

    #[tokio::test]
    async fn something_else_in_the_way_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(SOCKET_NAME), "keep me").unwrap();
        let err = UnixSocket::bind(dir.path()).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(
            std::fs::read_to_string(dir.path().join(SOCKET_NAME)).unwrap(),
            "keep me"
        );
    }

    #[tokio::test]
    async fn connections_from_another_uid_are_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let socket = UnixSocket::bind(dir.path()).unwrap();
        let path = socket.path().to_path_buf();
        let other_uid = socket.uid.wrapping_add(1);
        let app = Router::new().route("/", get(|| async { "hello" }));
        let server = tokio::spawn(axum::serve(socket.with_uid(other_uid), app).into_future());

        let mut stream = UnixStream::connect(&path).await.unwrap();
        // The write may or may not fail, depending on how fast the server drops us.
        let _ = stream
            .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .await;
        let mut reply = Vec::new();
        let _ = stream.read_to_end(&mut reply).await;
        assert!(
            reply.is_empty(),
            "got {:?}",
            String::from_utf8_lossy(&reply)
        );
        server.abort();
    }
}
