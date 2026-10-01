//! A unix socket in a private directory, accepting only the daemon's own user.

use pitcrew_auth::{create_private_dir, euid};
use std::io;
use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use tokio::net::{UnixListener, UnixStream, unix};

/// The socket's file name inside its directory.
pub const SOCKET_NAME: &str = "pitcrewd.sock";

/// A bound unix socket that drops connections from other users. Removes its socket file when
/// dropped, if the file is still the one it created.
#[derive(Debug)]
pub struct UnixSocket {
    listener: UnixListener,
    path: PathBuf,
    uid: u32,
    /// `(dev, ino)` of the socket file it created.
    file_id: (u64, u64),
}

impl UnixSocket {
    /// Creates `dir` with mode 0700 (or checks that an existing one is ours and private),
    /// removes a stale socket, and binds `dir/pitcrewd.sock`.
    ///
    /// # Errors
    /// The directory is not private or another user owns it; another daemon is listening on the
    /// socket; something other than our own socket is in the way; binding fails.
    pub fn bind(dir: &Path) -> io::Result<Self> {
        // Checks owner and mode before anything inside the directory is touched.
        create_private_dir(dir)?;
        let path = dir.join(SOCKET_NAME);
        remove_stale(&path)?;
        let listener = UnixListener::bind(&path)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        let meta = std::fs::symlink_metadata(&path)?;
        Ok(Self {
            listener,
            path,
            uid: euid(),
            file_id: (meta.dev(), meta.ino()),
        })
    }

    /// The socket's path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Removes a socket left behind by a daemon that is gone. Refuses to touch a live socket, a
/// socket owned by someone else, or anything that is not a socket.
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
    if meta.uid() != euid() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{} is owned by another user", path.display()),
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
        // Leave a socket that a newer daemon put in our place.
        let ours =
            std::fs::symlink_metadata(&self.path).is_ok_and(|m| (m.dev(), m.ino()) == self.file_id);
        if ours {
            let _ = std::fs::remove_file(&self.path);
        }
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
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("run");
        create_private_dir(&dir).unwrap();
        std::fs::write(dir.join(SOCKET_NAME), "keep me").unwrap();
        let err = UnixSocket::bind(&dir).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(
            std::fs::read_to_string(dir.join(SOCKET_NAME)).unwrap(),
            "keep me"
        );
    }

    #[tokio::test]
    async fn connections_from_another_uid_are_dropped() {
        let tmp = tempfile::tempdir().unwrap();
        let socket = UnixSocket::bind(&tmp.path().join("run")).unwrap();
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

    #[test]
    fn an_open_directory_is_refused_before_anything_is_touched() {
        use std::os::unix::fs::PermissionsExt as _;
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("run");
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o775)).unwrap();
        let err = UnixSocket::bind(&dir).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        assert!(!dir.join(SOCKET_NAME).exists());
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o775, "the directory is not re-permissioned");
    }

    #[tokio::test]
    async fn drop_leaves_a_socket_that_replaced_ours() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("run");
        let socket = UnixSocket::bind(&dir).unwrap();
        let path = socket.path().to_path_buf();
        // Another socket takes the name.
        std::fs::remove_file(&path).unwrap();
        let _other = std::os::unix::net::UnixListener::bind(&path).unwrap();
        drop(socket);
        assert!(path.exists());
    }
}
