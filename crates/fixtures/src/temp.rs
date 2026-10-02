//! Temporary folders short enough for the unix sockets tests make in them.

/// A temporary folder for a test that makes unix sockets in it (an ssh control socket, an
/// askpass socket, a daemon's). A socket's path holds at most 104 bytes on macOS (108 on Linux),
/// and macOS's per-user temporary folder (`/var/folders/…/T/`) takes about half of that, so there
/// the folder goes in `/tmp`; elsewhere it is an ordinary temporary folder.
///
/// # Errors
///
/// If the folder cannot be created.
pub fn short_tempdir() -> std::io::Result<tempfile::TempDir> {
    if cfg!(target_os = "macos") {
        tempfile::Builder::new().tempdir_in("/tmp")
    } else {
        tempfile::tempdir()
    }
}
