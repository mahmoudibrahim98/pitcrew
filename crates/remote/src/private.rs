//! The private runtime directory for ControlMaster sockets and askpass sockets (Unix).
//!
//! It must be owned by us, mode 0700 and not a symlink. An existing directory that fails the
//! check is refused, never repaired: in a shared `/tmp`, someone else may have created it first.

use std::io;
use std::path::Path;
#[cfg(unix)]
use std::path::PathBuf;

/// `$XDG_RUNTIME_DIR/pitcrew-ssh` when set, else `/tmp/pitcrew-ssh-<uid>`. One level, so its
/// parent is either our own runtime dir or the sticky `/tmp`, where nobody else can rename it.
/// `/tmp` rather than the platform temp dir because socket paths are limited to about 100
/// bytes, and macOS's per-user temp dir alone takes half of that.
#[cfg(unix)]
#[must_use]
pub fn default_runtime_dir() -> PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty()) {
        Some(dir) => PathBuf::from(dir).join("pitcrew-ssh"),
        None => PathBuf::from(format!("/tmp/pitcrew-ssh-{}", euid())),
    }
}

/// Creates `dir` and its missing parents with mode 0700, then checks that `dir` is private.
///
/// # Errors
/// Creating fails, or the directory is not private.
pub fn ensure_private_dir(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
    }
    #[cfg(not(unix))]
    std::fs::create_dir_all(dir)?;
    check_private_dir(dir)
}

/// Checks that `dir` is a real directory owned by us with no group or other access.
///
/// # Errors
/// `PermissionDenied` or `InvalidInput` naming what is wrong.
pub fn check_private_dir(dir: &Path) -> io::Result<()> {
    let meta = std::fs::symlink_metadata(dir)?;
    if !meta.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not a directory", dir.display()),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if meta.uid() != euid() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("{} is owned by another user", dir.display()),
            ));
        }
        if meta.mode() & 0o077 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "{} has mode {:o}; it must be 0700",
                    dir.display(),
                    meta.mode() & 0o777
                ),
            ));
        }
    }
    Ok(())
}

/// The effective user id.
#[cfg(unix)]
#[must_use]
pub fn euid() -> u32 {
    rustix::process::geteuid().as_raw()
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    #[test]
    fn creates_0700_and_refuses_open_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("a/b");
        ensure_private_dir(&dir).unwrap();
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);

        let open = tmp.path().join("open");
        std::fs::create_dir(&open).unwrap();
        std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(ensure_private_dir(&open).is_err());

        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&dir, &link).unwrap();
        assert!(check_private_dir(&link).is_err());
    }
}
