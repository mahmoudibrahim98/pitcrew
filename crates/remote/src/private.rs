//! The private runtime directory for ControlMaster sockets, askpass sockets and ssh's own log.
//!
//! It must be owned by us, mode 0700 and not a symlink. An existing directory that fails the
//! check is refused, never repaired: in a shared `/tmp`, someone else may have created it first.
//! That would block us for good, so the defaults are a list, and the first usable one wins.

use std::io;
use std::path::{Path, PathBuf};

/// Where the runtime directory may go, best first:
/// - Unix: `$XDG_RUNTIME_DIR/pitcrew-ssh` when set; `/tmp/pitcrew-ssh-<uid>`; `~/.pitcrew/s`.
///   `/tmp` rather than the platform temp dir because socket paths are limited to about 100
///   bytes, and macOS's per-user temp dir alone takes half of that. The home fallback is for a
///   `/tmp` name another user squatted.
/// - Windows: `%TEMP%\pitcrew-ssh`, which is per user.
#[cfg(unix)]
#[must_use]
pub fn default_runtime_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty()) {
        dirs.push(PathBuf::from(dir).join("pitcrew-ssh"));
    }
    dirs.push(PathBuf::from(format!("/tmp/pitcrew-ssh-{}", euid())));
    if let Some(home) = crate::config::home_dir() {
        dirs.push(home.join(".pitcrew").join("s"));
    }
    dirs
}

/// See the Unix version.
#[cfg(not(unix))]
#[must_use]
pub fn default_runtime_dirs() -> Vec<PathBuf> {
    vec![std::env::temp_dir().join("pitcrew-ssh")]
}

/// The first of `candidates` that is, or can be made, a private directory with a name ssh reads
/// literally.
///
/// # Errors
/// Every candidate failed; the error is the first one's.
pub fn pick_runtime_dir(candidates: &[PathBuf]) -> io::Result<PathBuf> {
    let mut first_error = None;
    for dir in candidates {
        match check_dir_name(dir).and_then(|()| ensure_private_dir(dir)) {
            Ok(()) => return Ok(dir.clone()),
            Err(e) => {
                first_error.get_or_insert(e);
            }
        }
    }
    Err(first_error
        .unwrap_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no runtime directory")))
}

/// The runtime directory goes into `ControlPath` and `-E`, so its name must be absolute, UTF-8,
/// and free of what ssh would expand there (`%`, `$`, a leading `~`) or what would split it.
///
/// # Errors
/// `InvalidInput` naming the problem.
pub fn check_dir_name(dir: &Path) -> io::Result<()> {
    let invalid = |why: &str| {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("runtime directory {}: {why}", dir.display()),
        ))
    };
    let Some(text) = dir.to_str() else {
        return invalid("not UTF-8");
    };
    if !dir.is_absolute() {
        return invalid("not an absolute path");
    }
    if text
        .chars()
        .any(|c| c.is_control() || matches!(c, '%' | '$' | '"' | '\''))
    {
        return invalid("contains a quote, '%', '$' or a control character");
    }
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_ssh_would_expand_are_refused() {
        for bad in [
            "relative/dir",
            "/tmp/50%",
            "/tmp/$HOME",
            "/tmp/a'b",
            "/tmp/a\nb",
        ] {
            assert!(check_dir_name(Path::new(bad)).is_err(), "{bad:?}");
        }
        if cfg!(unix) {
            check_dir_name(Path::new("/run/user/1000/pitcrew-ssh")).unwrap();
        }
    }

    #[cfg(unix)]
    #[test]
    fn creates_0700_and_refuses_open_dirs() {
        use std::os::unix::fs::PermissionsExt as _;
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

    /// A squatted first choice (not private, a file, a symlink) falls through to the next.
    #[cfg(unix)]
    #[test]
    fn a_squatted_dir_falls_back() {
        use std::os::unix::fs::PermissionsExt as _;
        let tmp = tempfile::tempdir().unwrap();
        let squatted = tmp.path().join("squatted");
        std::fs::create_dir(&squatted).unwrap();
        std::fs::set_permissions(&squatted, std::fs::Permissions::from_mode(0o777)).unwrap();
        let file = tmp.path().join("file");
        std::fs::write(&file, "").unwrap();
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&squatted, &link).unwrap();
        let fallback = tmp.path().join("home/.pitcrew/s");

        let picked = pick_runtime_dir(&[squatted.clone(), file, link, fallback.clone()]).unwrap();
        assert_eq!(picked, fallback);
        let mode = std::fs::metadata(&fallback).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
        // The squatted directory was left alone.
        let mode = std::fs::metadata(&squatted).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o777);

        let err = pick_runtime_dir(&[squatted]).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
    }

    #[cfg(unix)]
    #[test]
    fn the_defaults_end_in_the_home_fallback() {
        let dirs = default_runtime_dirs();
        assert!(dirs.iter().any(|d| d.starts_with("/tmp")));
        if crate::config::home_dir().is_some() {
            assert!(dirs.last().unwrap().ends_with(".pitcrew/s"));
        }
    }
}
