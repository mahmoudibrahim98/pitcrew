//! Finding `pitcrewd`: the configured path, next to the app's executable, or on `PATH`, in that
//! order. A configured path that is not a program is an error, not a reason to look elsewhere.

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
    /// Neither next to the app nor on `PATH`.
    #[error("pitcrewd is not next to the app or on PATH; install it, or set its path in settings")]
    NotFound,
}

/// Finds `pitcrewd`.
///
/// - `configured`: from the app's settings, if any.
/// - `beside`: the directory of the app's executable.
/// - `path_var`: the `PATH` variable. Relative entries (such as `.`) are skipped, so the current
///   directory never supplies the daemon.
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
            Ok(path.to_path_buf())
        } else {
            Err(LocateError::NotAProgram(path.to_path_buf()))
        };
    }
    if let Some(dir) = beside {
        let path = dir.join(PITCREWD);
        if is_program(&path) {
            return Ok(path);
        }
    }
    if let Some(var) = path_var {
        for dir in std::env::split_paths(var).filter(|d| d.is_absolute()) {
            let path = dir.join(PITCREWD);
            if is_program(&path) {
                return Ok(path);
            }
        }
    }
    Err(LocateError::NotFound)
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
}
