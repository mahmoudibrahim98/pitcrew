//! Finding `pitcrewd`: the configured path, then next to the app's executable, then (debug builds
//! only) on `PATH`. A configured path that is not a program is an error, not a reason to look
//! elsewhere.
//!
//! **A planted binary is refused.** The app hands the daemon it starts the person's token
//! directory, so the program must be one only the person (or the system) could have put there:
//!
//! - Unix: the file it resolves to, its directory, and the directory of the path as given are each
//!   owned by root or by us, and none can be written by group or others.
//! - Windows: a file with a `Zone.Identifier` stream (downloaded from the web and not unblocked) is
//!   refused. Checking its owner needs the Win32 security API, which needs `unsafe`; that check is
//!   left for later.
//! - Release builds never search `PATH`: a writable directory early in someone's `PATH` must not
//!   supply the daemon.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// The daemon's file name on this platform.
pub const PITCREWD: &str = if cfg!(windows) {
    "pitcrewd.exe"
} else {
    "pitcrewd"
};

/// Why `pitcrewd` was not found.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum LocateError {
    /// The configured path is not a program.
    #[error("the configured pitcrewd, {0}, is not a program")]
    NotAProgram(PathBuf),
    /// The program is there, but someone other than the person or the system could have put it
    /// there.
    #[error("not running {path}: {reason}")]
    Untrusted {
        /// The program.
        path: PathBuf,
        /// Why it is refused.
        reason: String,
    },
    /// Nowhere it is looked for.
    #[error("{}", not_found_message())]
    NotFound,
}

fn not_found_message() -> &'static str {
    if cfg!(debug_assertions) {
        "pitcrewd is not next to the app or on PATH; install it, or set its path in settings"
    } else {
        "pitcrewd is not next to the app; install it there, or set its path in settings"
    }
}

/// Finds `pitcrewd`.
///
/// - `configured`: from the app's settings, if any.
/// - `beside`: the directory of the app's executable.
/// - `path_var`: the `PATH` variable, searched in debug builds only. Relative entries (such as
///   `.`) are skipped, so the current directory never supplies the daemon.
///
/// # Errors
/// See [`LocateError`].
pub fn locate(
    configured: Option<&Path>,
    beside: Option<&Path>,
    path_var: Option<&OsStr>,
) -> Result<PathBuf, LocateError> {
    if let Some(path) = configured {
        return if is_program(path) {
            trusted(path)
        } else {
            Err(LocateError::NotAProgram(path.to_path_buf()))
        };
    }
    if let Some(dir) = beside {
        let path = dir.join(PITCREWD);
        if is_program(&path) {
            return trusted(&path);
        }
    }
    if cfg!(debug_assertions)
        && let Some(var) = path_var
    {
        for dir in std::env::split_paths(var).filter(|d| d.is_absolute()) {
            let path = dir.join(PITCREWD);
            if is_program(&path) {
                return trusted(&path);
            }
        }
    }
    Err(LocateError::NotFound)
}

/// `path`, if it passes the checks in the module's docs.
fn trusted(path: &Path) -> Result<PathBuf, LocateError> {
    check(path)
        .map(|()| path.to_path_buf())
        .map_err(|reason| LocateError::Untrusted {
            path: path.to_path_buf(),
            reason,
        })
}

#[cfg(unix)]
fn check(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt as _;
    let euid = rustix::process::geteuid().as_raw();
    let inspect = |what: &Path| -> Result<(), String> {
        let meta = std::fs::metadata(what).map_err(|e| format!("{}: {e}", what.display()))?;
        owner_and_mode(meta.uid(), meta.mode(), euid)
            .map_err(|why| format!("{} {why}", what.display()))
    };
    let directory = |of: &Path| -> Result<PathBuf, String> {
        of.parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(Path::to_path_buf)
            .ok_or_else(|| format!("{} has no directory", of.display()))
    };
    // Whoever can write the directory of the path as given can swap the program (or the link).
    inspect(&directory(path)?)?;
    // The file it resolves to, and that file's directory.
    let real = std::fs::canonicalize(path).map_err(|e| format!("{}: {e}", path.display()))?;
    inspect(&real)?;
    inspect(&directory(&real)?)
}

/// Owned by root or by `euid`, and not writable by group or others.
#[cfg(unix)]
fn owner_and_mode(uid: u32, mode: u32, euid: u32) -> Result<(), String> {
    if uid != 0 && uid != euid {
        return Err(format!("is owned by another user (uid {uid})"));
    }
    if mode & 0o022 != 0 {
        return Err(format!(
            "can be written by other users (mode {:o})",
            mode & 0o7777
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn check(path: &Path) -> Result<(), String> {
    let mut stream = path.as_os_str().to_owned();
    stream.push(":Zone.Identifier");
    if std::fs::metadata(PathBuf::from(stream)).is_ok() {
        return Err(
            "it was downloaded from the internet (it has a Zone.Identifier); install it from \
             the app's installer, or unblock it in its properties"
                .into(),
        );
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn check(_path: &Path) -> Result<(), String> {
    Ok(())
}

/// A regular file (following links), executable on Unix.
fn is_program(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    fn program(dir: &Path) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join(PITCREWD);
        std::fs::write(&path, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755)).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        path
    }

    #[test]
    fn configured_then_beside_then_path() {
        let tmp = tempfile::tempdir().unwrap();
        let configured = program(&tmp.path().join("configured"));
        let beside_dir = tmp.path().join("app");
        let beside = program(&beside_dir);
        let on_path_dir = tmp.path().join("bin");
        let on_path = program(&on_path_dir);
        let path_var: OsString =
            std::env::join_paths([tmp.path().join("empty"), on_path_dir.clone()]).unwrap();

        assert_eq!(
            locate(Some(&configured), Some(&beside_dir), Some(&path_var)),
            Ok(configured)
        );
        assert_eq!(locate(None, Some(&beside_dir), Some(&path_var)), Ok(beside));
        // Tests are debug builds, which search PATH; release builds do not.
        assert_eq!(
            locate(None, Some(&tmp.path().join("nowhere")), Some(&path_var)),
            Ok(on_path)
        );
        assert_eq!(locate(None, None, None), Err(LocateError::NotFound));
    }

    #[test]
    fn a_configured_path_that_is_not_a_program_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let beside_dir = tmp.path().join("app");
        program(&beside_dir);
        let missing = tmp.path().join("missing").join(PITCREWD);
        assert_eq!(
            locate(Some(&missing), Some(&beside_dir), None),
            Err(LocateError::NotAProgram(missing.clone()))
        );
        assert_eq!(
            locate(Some(tmp.path()), None, None),
            Err(LocateError::NotAProgram(tmp.path().to_path_buf()))
        );
    }

    #[test]
    fn relative_path_entries_are_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        program(tmp.path());
        let var = OsString::from(".");
        let cwd = std::env::current_dir().unwrap();
        // Whatever the current directory holds, "." is never searched.
        assert_eq!(locate(None, None, Some(&var)), Err(LocateError::NotFound));
        assert_eq!(std::env::current_dir().unwrap(), cwd);
    }

    #[cfg(unix)]
    #[test]
    fn a_file_without_the_execute_bit_is_not_a_program() {
        use std::os::unix::fs::PermissionsExt as _;
        let tmp = tempfile::tempdir().unwrap();
        let path = program(tmp.path());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(
            locate(None, Some(tmp.path()), None),
            Err(LocateError::NotFound)
        );
    }

    #[cfg(unix)]
    #[test]
    fn only_root_or_us_and_never_writable_by_others() {
        let us = 1000;
        for (uid, mode) in [
            (0, 0o100_755),
            (us, 0o100_755),
            (us, 0o040_700),
            (0, 0o040_555),
        ] {
            assert_eq!(owner_and_mode(uid, mode, us), Ok(()), "{uid} {mode:o}");
        }
        for (uid, mode) in [
            (1001, 0o100_755),
            (65534, 0o040_755),
            (us, 0o100_775),
            (us, 0o100_757),
            (0, 0o041_777),
            (us, 0o040_770),
        ] {
            assert!(owner_and_mode(uid, mode, us).is_err(), "{uid} {mode:o}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_program_others_can_write_or_swap_is_refused() {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = |p: &Path, m: u32| {
            std::fs::set_permissions(p, std::fs::Permissions::from_mode(m)).unwrap();
        };
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("app");
        let path = program(&dir);
        assert_eq!(locate(Some(&path), None, None), Ok(path.clone()));

        // The file is group-writable.
        mode(&path, 0o775);
        let refused = locate(Some(&path), None, None);
        assert!(
            matches!(refused, Err(LocateError::Untrusted { .. })),
            "{refused:?}"
        );
        mode(&path, 0o755);

        // Its directory is world-writable: someone could swap it.
        mode(&dir, 0o777);
        let refused = locate(None, Some(&dir), None);
        assert!(
            matches!(refused, Err(LocateError::Untrusted { .. })),
            "{refused:?}"
        );
        mode(&dir, 0o755);

        // A link in a trusted directory to a program in an open one.
        let open = tmp.path().join("open");
        let target = program(&open);
        mode(&open, 0o777);
        let linked = tmp.path().join("linked");
        std::fs::create_dir(&linked).unwrap();
        mode(&linked, 0o755);
        std::os::unix::fs::symlink(&target, linked.join(PITCREWD)).unwrap();
        let refused = locate(None, Some(&linked), None);
        assert!(
            matches!(refused, Err(LocateError::Untrusted { .. })),
            "{refused:?}"
        );
        mode(&open, 0o755);
        assert_eq!(locate(None, Some(&linked), None), Ok(linked.join(PITCREWD)));
    }
}
