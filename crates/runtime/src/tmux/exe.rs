//! Finding programs as files, so a name is never taken as a shell builtin or function.

use std::ffi::OsStr;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// The executable file `program` names: itself if it contains a slash (relative to `cwd`),
/// otherwise the first match in `path`'s absolute directories. Relative and empty `PATH`
/// entries are skipped: they would depend on the working directory.
pub(crate) fn find(program: &str, path: Option<&OsStr>, cwd: &Path) -> Option<PathBuf> {
    if program.contains('/') {
        let candidate = cwd.join(program);
        return is_executable(&candidate).then_some(candidate);
    }
    std::env::split_paths(path?)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(program))
        .find(|candidate| is_executable(candidate))
}

fn is_executable(path: &Path) -> bool {
    std::fs::metadata(path)
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_resolve_to_files_never_to_builtins() {
        let path = std::ffi::OsString::from("relative/bin::/usr/bin:/bin");
        let sh = find("sh", Some(&path), Path::new("/")).expect("sh");
        assert!(sh.is_absolute() && sh.ends_with("sh"), "{}", sh.display());
        for builtin in ["eval", "exec", "trap", "no-such-program-pitcrew"] {
            assert_eq!(
                find(builtin, Some(&path), Path::new("/")),
                None,
                "{builtin}"
            );
        }
        assert_eq!(find("sh", None, Path::new("/")), None);
        assert_eq!(
            find("bin/sh", Some(&path), Path::new("/")),
            Some(PathBuf::from("/bin/sh"))
        );
        // A directory, or a file without an execute bit, is not a program.
        assert_eq!(find("/tmp", Some(&path), Path::new("/")), None);
        assert_eq!(find("/etc/hostname", Some(&path), Path::new("/")), None);
    }
}
