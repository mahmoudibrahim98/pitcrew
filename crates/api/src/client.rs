//! Checks a client (the desktop, the CLI) makes before sending a token to a local daemon, so a
//! socket or pipe planted by another user never receives it.
//!
//! - Unix: [`check_unix_socket`] before connecting (the directory is ours and 0700, the socket
//!   is ours), and [`check_unix_peer`] after (the server runs as us).
//! - Windows: [`check_pipe_server`] after connecting: the process serving the pipe runs as the
//!   current user. A pipe name can be created by anyone while the daemon is down, so this check,
//!   not the name, is what makes the pipe trustworthy.

use std::io;

/// Checks the daemon's socket directory and socket, and returns the socket's path.
///
/// # Errors
/// `PermissionDenied` if the directory is not ours and private, or the socket is not ours; the
/// socket is missing or not a socket.
#[cfg(unix)]
pub fn check_unix_socket(dir: &std::path::Path) -> io::Result<std::path::PathBuf> {
    use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _};
    pitcrew_auth::check_private_dir(dir)?;
    let path = dir.join(crate::SOCKET_NAME);
    let meta = std::fs::symlink_metadata(&path)?;
    if !meta.file_type().is_socket() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not a socket", path.display()),
        ));
    }
    if meta.uid() != pitcrew_auth::euid() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{} is owned by another user", path.display()),
        ));
    }
    Ok(path)
}

/// Checks that the process at the other end of a connected socket runs as us.
///
/// # Errors
/// `PermissionDenied` if it runs as another user; the credentials cannot be read.
#[cfg(unix)]
pub fn check_unix_peer(stream: &tokio::net::UnixStream) -> io::Result<()> {
    let uid = stream.peer_cred()?.uid();
    if uid == pitcrew_auth::euid() {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("the daemon runs as another user (uid {uid})"),
        ))
    }
}

/// Checks that the server end of a connected pipe runs as the current user.
///
/// # Errors
/// `PermissionDenied` if it runs as another user; the check cannot be made.
#[cfg(windows)]
pub fn check_pipe_server(pipe: &impl std::os::windows::io::AsHandle) -> io::Result<()> {
    use crate::listener::pipe_security::{current_user_sid, pipe_server_sid};
    let server = pipe_server_sid(pipe)?;
    if server == current_user_sid()? {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("the pipe's server runs as another user ({server})"),
        ))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::UnixSocket;

    #[tokio::test]
    async fn our_own_daemon_passes() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("run");
        let socket = UnixSocket::bind(&dir).unwrap();
        let path = check_unix_socket(&dir).unwrap();
        assert_eq!(path, socket.path());
        let stream = tokio::net::UnixStream::connect(&path).await.unwrap();
        check_unix_peer(&stream).unwrap();
    }

    #[test]
    fn an_open_directory_fails() {
        use std::os::unix::fs::PermissionsExt as _;
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("run");
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        let err = check_unix_socket(&dir).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
    }
}
