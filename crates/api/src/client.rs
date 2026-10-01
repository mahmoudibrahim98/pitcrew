//! Checks a client (the desktop, the CLI) makes before sending a token to a local daemon, so a
//! socket or pipe planted by another user never receives it.
//!
//! - Unix: [`check_unix_socket`] before connecting (the directory is ours and 0700, the socket
//!   is ours), and [`check_unix_peer`] after (the server runs as us).
//! - Windows: [`check_pipe_server`] after connecting: the pipe is owned by the current user (or
//!   by our token's default owner, which is the user unless we run elevated). A pipe name can be
//!   created by anyone while the daemon is down, so this check, not the name, is what makes the
//!   pipe trustworthy.

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

/// Checks that a connected pipe is owned by the current user, or by the owner our own token
/// gives the objects it creates, so it was created by our daemon (or by us).
///
/// The daemon names the current user as its pipe's owner. Another user cannot create a pipe
/// owned by us (that takes the restore privilege), and the owner, unlike the server's process
/// id (which an earlier version checked), cannot be recycled by a process that exits.
///
/// The token's default owner (`TokenOwner`) is also accepted, so a pipe created without naming
/// an owner (as a test server does) passes too. Unelevated, that owner is the user itself, so
/// nothing changes. Elevated, it is typically the Administrators group, so a pipe any elevated
/// administrator created passes. That is the residual, and it is unchanged: an administrator, or
/// a process holding the restore privilege, could already plant a pipe that names us as its
/// owner, and can already read our files.
///
/// The handle needs `READ_CONTROL`, which a client opened for reading (tokio's default) has.
///
/// # Errors
/// `PermissionDenied` if anyone else owns it; the check cannot be made.
#[cfg(windows)]
pub fn check_pipe_server(pipe: &impl std::os::windows::io::AsHandle) -> io::Result<()> {
    use crate::listener::pipe_security::{current_user_sid, default_owner_sid, owner_sid};
    let owner = owner_sid(pipe)?;
    if owner == current_user_sid()? || owner == default_owner_sid()? {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("the pipe is owned by another user ({owner})"),
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
