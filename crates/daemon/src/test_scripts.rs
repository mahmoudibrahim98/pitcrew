//! Stand-in programs for the unit tests (Unix), written so that no process can still hold one open
//! for writing when a test runs it.
//!
//! The unit tests run as threads of one process, and many of them start programs. A child forked
//! while a test still has its stand-in open for writing inherits that handle until it execs, and
//! running the stand-in meanwhile fails with `ETXTBSY` ("executable file busy"). So the file is
//! written by a `sh` of its own, from standard input: this process never opens it for writing,
//! and once that `sh` has exited nothing does.

use std::io::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::process::{Command, Stdio};

/// Writes `text` to a new or replaced file at `path`, then gives it `mode`.
///
/// # Panics
///
/// When `sh` cannot write the file: the test cannot go on without it.
pub(crate) fn write_script(path: &Path, text: &str, mode: u32) {
    let mut sh = Command::new("/bin/sh")
        .args(["-c", "exec cat > \"$1\"", "sh"])
        .arg(path)
        .stdin(Stdio::piped())
        .spawn()
        .expect("sh writes the stand-in");
    sh.stdin
        .take()
        .expect("its standard input")
        .write_all(text.as_bytes())
        .expect("the stand-in's text");
    let status = sh.wait().expect("sh ends");
    assert!(status.success(), "sh could not write {}", path.display());
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
        .expect("the stand-in's mode");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stand_in_is_written_whole_and_runs() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("stand-in");
        write_script(&path, "#!/bin/sh\necho \"synthetic $1\"\n", 0o700);
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let out = Command::new(&path).arg("run").output().unwrap();
        assert_eq!(String::from_utf8(out.stdout).unwrap(), "synthetic run\n");
        // Replaced whole.
        write_script(&path, "#!/bin/sh\nexit 3\n", 0o755);
        assert_eq!(Command::new(&path).status().unwrap().code(), Some(3));
    }
}
