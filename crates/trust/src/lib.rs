//! # pitcrew-trust
//!
//! What PitCrew checks before it runs a program of its own, and on Windows the one copy of the
//! code that secures and checks its named pipes.
//!
//! - [`check_trusted`]: a program (or a file PitCrew reads) is one only the person or the system
//!   could have put there. The desktop checks `pitcrewd` and its helpers with it; the daemon checks
//!   `pitcrew-ptyd` when it chooses its terminals' runtime, and again just before each launch.
//! - [`windows`] (Windows only): the current user's SID and integrity level, an object's owner,
//!   DACL and mandatory label, SIDs from text, and security descriptors for named pipes. The
//!   API's pipe, the askpass pipe and pitcrew-ptyd's pipe each keep their own policy (the
//!   descriptor they ask for, what they check) and call these.
//! - [`sddl`]: integrity levels and their SDDL labels, as text.
//!
//! Small on purpose: no async, and nothing beyond the platform's own calls.
//!
//! **Owned by stream 0.**

#![deny(unsafe_code)]

pub mod sddl;
#[cfg(windows)]
pub mod windows;

use std::path::Path;
#[cfg(unix)]
use std::path::PathBuf;

/// Whether `path` (a program PitCrew runs, or a file it reads) is one only the person or the
/// system could have put there:
///
/// - Unix: the file it resolves to, that file's directory, and the directory of the path as
///   given each belong to root or to us (the effective user), and none can be written by group
///   or others. Whoever can write a directory on the way can swap the program, or the link to it.
/// - Windows: a file with a `Zone.Identifier` stream (downloaded from the web and not unblocked)
///   is refused. Its owner is not checked.
/// - Elsewhere: nothing is checked.
///
/// It holds when it is asked: check just before the program is started.
///
/// # Errors
/// Why it does not hold, for people.
pub fn check_trusted(path: &Path) -> Result<(), String> {
    check(path)
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
    if std::fs::metadata(std::path::PathBuf::from(stream)).is_ok() {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A file standing for a program, in `dir` (made if missing); on Unix both are 0755.
    fn program(dir: &Path) -> std::path::PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join("program");
        std::fs::write(&path, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            set_mode(dir, 0o755);
            set_mode(&path, 0o755);
        }
        path
    }

    #[cfg(unix)]
    fn set_mode(path: &Path, mode: u32) {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    #[test]
    fn a_program_of_ours_in_a_folder_of_ours_passes() {
        let tmp = tempfile::tempdir().unwrap();
        let path = program(&tmp.path().join("app"));
        assert_eq!(check_trusted(&path), Ok(()));
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

    /// Group- or world-writable, the program or its folder; a link into a folder others can
    /// write; a folder with no name. Each refused, with the reason naming what is wrong.
    #[cfg(unix)]
    #[test]
    fn a_program_others_can_write_or_swap_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("app");
        let path = program(&dir);
        let refused = |path: &Path, says: &str| {
            let why = check_trusted(path).expect_err("refused");
            assert!(why.contains(says), "{why}");
        };

        set_mode(&path, 0o775);
        refused(&path, "can be written by other users (mode 775)");
        set_mode(&path, 0o757);
        refused(&path, "can be written by other users (mode 757)");
        set_mode(&path, 0o755);

        // Its folder is open: someone could swap it.
        set_mode(&dir, 0o777);
        refused(
            &path,
            &format!("{} can be written by other users", dir.display()),
        );
        set_mode(&dir, 0o775);
        refused(&path, "mode 775");
        set_mode(&dir, 0o755);
        assert_eq!(check_trusted(&path), Ok(()));

        // A link in a trusted folder to a program in an open one.
        let open = tmp.path().join("open");
        let target = program(&open);
        set_mode(&open, 0o777);
        let linked = tmp.path().join("linked");
        std::fs::create_dir(&linked).unwrap();
        set_mode(&linked, 0o755);
        std::os::unix::fs::symlink(&target, linked.join("program")).unwrap();
        refused(
            &linked.join("program"),
            "open can be written by other users",
        );
        set_mode(&open, 0o755);
        assert_eq!(check_trusted(&linked.join("program")), Ok(()));

        // A link in an open folder to a program in a trusted one: the link can be swapped.
        set_mode(&linked, 0o777);
        refused(
            &linked.join("program"),
            "linked can be written by other users",
        );
        set_mode(&linked, 0o755);

        // No such file, and a bare name with no folder.
        refused(&tmp.path().join("missing"), "missing");
        refused(Path::new("program"), "has no directory");
    }

    /// A program downloaded from the web, and not unblocked, carries a `Zone.Identifier` stream.
    #[cfg(windows)]
    #[test]
    fn a_downloaded_program_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let path = program(&tmp.path().join("app"));
        assert_eq!(check_trusted(&path), Ok(()));
        let mut stream = path.as_os_str().to_owned();
        stream.push(":Zone.Identifier");
        std::fs::write(
            std::path::PathBuf::from(stream),
            "[ZoneTransfer]\r\nZoneId=3\r\n",
        )
        .unwrap();
        let why = check_trusted(&path).expect_err("refused");
        assert!(why.contains("Zone.Identifier"), "{why}");
    }
}
