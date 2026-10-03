//! Finding and starting pitcrew-ptyd.
//!
//! - **Where it is.** Next to the running executable (as the desktop finds `pitcrewd` and its
//!   helpers), or wherever [`super::PtyOptions::ptyd`] says. `PATH` is never searched.
//! - **Not a planted binary.** ptyd starts every agent, so it passes the check the desktop makes
//!   of `pitcrewd` ([`check_trusted`]) when the runtime is detected and again just before each
//!   launch, which may come hours later: a ptyd that fails it is not started, and the terminal
//!   request fails with the reason.
//! - **Detached.** On Unix the process started here only starts the real ptyd and exits at once
//!   (so no zombie is left behind); the real one leaves this process's session (`setsid`), so
//!   neither a terminal's signals nor this process ending reach it. On Windows it is started
//!   detached, in a new process group, and outside any job this process is in when the job
//!   allows it (`CREATE_BREAKAWAY_FROM_JOB`).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

use super::PtyOptions;

/// The file name of pitcrew-ptyd on this platform.
pub const PTYD: &str = if cfg!(windows) {
    "pitcrew-ptyd.exe"
} else {
    "pitcrew-ptyd"
};

/// pitcrew-ptyd next to this process's executable, the one place PitCrew looks for it by
/// default.
pub fn beside_current_exe() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    Some(exe.parent()?.join(PTYD))
}

/// True if `path` is a program: a file (with an execute bit, on Unix).
pub(crate) fn is_program(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|meta| {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            meta.is_file() && meta.permissions().mode() & 0o111 != 0
        }
        #[cfg(not(unix))]
        {
            meta.is_file()
        }
    })
}

/// Whether `ptyd` passes the check every program PitCrew launches passes
/// (`pitcrew_trust::check_trusted`): on Unix it, the file it resolves to and both their
/// directories belong to root or us and only we can write them; on Windows it was not downloaded
/// from the internet (no `Zone.Identifier`).
///
/// # Errors
///
/// Why not, for people.
pub fn check_trusted(ptyd: &Path) -> Result<(), String> {
    pitcrew_trust::check_trusted(ptyd)
        .map_err(|why| format!("not running pitcrew-ptyd at {}: {why}", ptyd.display()))
}

/// Starts pitcrew-ptyd for `options.endpoint`, once it has passed [`check_trusted`] (just
/// before it is started, as it may have changed since it was detected). On Unix, waits (until
/// `deadline`) for the starter process, which reports problems it finds (an unsafe directory,
/// say) before the real ptyd starts.
pub(crate) fn launch(options: &PtyOptions, deadline: Instant) -> Result<(), String> {
    if !options.ptyd.is_absolute() || !is_program(&options.ptyd) {
        return Err(format!(
            "pitcrew-ptyd is not at {} (it is looked for next to PitCrew's own executable)",
            options.ptyd.display()
        ));
    }
    check_trusted(&options.ptyd)?;
    let mut command = Command::new(&options.ptyd);
    command
        .arg("serve")
        .arg("--endpoint")
        .arg(&options.endpoint)
        .arg("--history")
        .arg(options.history.to_string());
    if let Some(idle) = options.idle_exit {
        command
            .arg("--idle-exit-ms")
            .arg(idle.as_millis().to_string());
    }
    command
        .envs(options.env.iter().map(|(k, v)| (k, v)))
        .stdin(Stdio::null())
        .stdout(Stdio::null());
    // A directory that stays: a process's working directory cannot be removed on Windows.
    if let Some(dir) = options.ptyd.parent() {
        command.current_dir(dir);
    }
    spawn(command, &options.ptyd, deadline)
}

#[cfg(unix)]
fn spawn(mut command: Command, ptyd: &Path, deadline: Instant) -> Result<(), String> {
    use std::io::Read;
    use std::time::Duration;

    let mut child = command
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run {}: {e}", ptyd.display()))?;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if status.success() {
                    return Ok(());
                }
                let mut text = String::new();
                if let Some(stderr) = child.stderr.take() {
                    let _ = stderr.take(4096).read_to_string(&mut text);
                }
                return Err(format!(
                    "pitcrew-ptyd did not start ({status}): {}",
                    text.trim()
                ));
            }
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(5));
            }
            outcome => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(match outcome {
                    Err(e) => format!("cannot wait for pitcrew-ptyd: {e}"),
                    _ => "pitcrew-ptyd did not start in time".into(),
                });
            }
        }
    }
}

#[cfg(windows)]
fn spawn(mut command: Command, ptyd: &Path, _deadline: Instant) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    use windows_sys::Win32::System::Threading::{
        CREATE_BREAKAWAY_FROM_JOB, CREATE_NEW_PROCESS_GROUP, DETACHED_PROCESS,
    };

    command.arg("--foreground").stderr(Stdio::null());
    let detached = DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP;
    // Leaving our job, if we are in one, keeps ptyd alive when that job is closed; a job that
    // does not allow it refuses the whole start, so then it starts inside (and ends with it).
    let spawned = command
        .creation_flags(detached | CREATE_BREAKAWAY_FROM_JOB)
        .spawn()
        .or_else(|e| {
            tracing::warn!(
                error = %e,
                "pitcrew-ptyd could not leave this process's job (CREATE_BREAKAWAY_FROM_JOB); \
                 starting it inside, so it ends when that job is closed"
            );
            command.creation_flags(detached).spawn()
        });
    // The handle is closed; the process runs on.
    spawned
        .map(drop)
        .map_err(|e| format!("cannot run {}: {e}", ptyd.display()))
}

#[cfg(not(any(unix, windows)))]
fn spawn(_command: Command, _ptyd: &Path, _deadline: Instant) -> Result<(), String> {
    Err("pitcrew-ptyd runs on Unix and Windows only".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn ptyd_is_found_beside_the_executable_and_must_be_a_program() {
        let beside = beside_current_exe().expect("current exe");
        assert_eq!(beside.file_name().and_then(|n| n.to_str()), Some(PTYD));
        let exe = std::env::current_exe().expect("exe");
        assert_eq!(beside.parent(), exe.parent());
        assert!(is_program(&exe));
        assert!(!is_program(exe.parent().expect("dir")));
        assert!(!is_program(&exe.with_file_name("no-such-program-pitcrew")));

        let mut options = PtyOptions::new("/nonexistent/pitcrew/ptyd");
        options.ptyd = PathBuf::from("pitcrew-ptyd");
        let why = launch(&options, Instant::now() + Duration::from_secs(1)).expect_err("relative");
        assert!(why.contains("not at"), "{why}");
    }

    /// Refused by `detect` and by `launch`, with the reason, and never run.
    #[cfg(any(unix, windows))]
    fn refused(options: &PtyOptions, says: &str) {
        let why = launch(options, Instant::now() + Duration::from_secs(5)).expect_err("refused");
        assert!(why.starts_with("not running pitcrew-ptyd at "), "{why}");
        assert!(why.contains(says), "{why}");
        match crate::pty::detect(options) {
            Err(pitcrew_interfaces::runtime::RuntimeError::Unavailable(why)) => {
                assert!(why.contains(says), "{why}");
            }
            other => panic!("{other:?}"),
        }
    }

    /// A stand-in ptyd in `dir` (both 0755) that leaves `ran` behind if it runs, and fails.
    #[cfg(unix)]
    fn stand_in(dir: &Path, ran: &Path) -> PathBuf {
        std::fs::create_dir_all(dir).expect("dir");
        let ptyd = dir.join(PTYD);
        std::fs::write(
            &ptyd,
            format!("#!/bin/sh\ntouch '{}'\nexit 1\n", ran.display()),
        )
        .expect("stand-in");
        set_mode(dir, 0o755);
        set_mode(&ptyd, 0o755);
        ptyd
    }

    #[cfg(unix)]
    fn set_mode(path: &Path, mode: u32) {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("chmod");
    }

    /// A ptyd that others can write, in a folder others can write, or reached through a link
    /// into one, is refused before it is started; the same ptyd, once only we can write it, is
    /// started.
    #[cfg(unix)]
    #[test]
    fn a_ptyd_others_can_write_or_swap_is_refused_before_it_runs() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let ran = tmp.path().join("ran");
        let dir = tmp.path().join("bin");
        let ptyd = stand_in(&dir, &ran);
        let mut options = PtyOptions::new(tmp.path().join("p").join("ptyd"));
        options.ptyd = ptyd.clone();
        assert_eq!(check_trusted(&ptyd), Ok(()));
        assert!(crate::pty::detect(&options).is_ok());

        set_mode(&ptyd, 0o775);
        refused(&options, "can be written by other users (mode 775)");
        set_mode(&ptyd, 0o757);
        refused(&options, "can be written by other users (mode 757)");
        set_mode(&ptyd, 0o755);
        set_mode(&dir, 0o777);
        refused(&options, &format!("{} can be written", dir.display()));
        set_mode(&dir, 0o755);

        // A link in a folder of ours to a ptyd in a folder anyone can write.
        let open = tmp.path().join("open");
        let target = stand_in(&open, &ran);
        set_mode(&open, 0o777);
        let linked = tmp.path().join("linked");
        std::fs::create_dir(&linked).expect("dir");
        set_mode(&linked, 0o755);
        std::os::unix::fs::symlink(&target, linked.join(PTYD)).expect("link");
        let mut through_link = options.clone();
        through_link.ptyd = linked.join(PTYD);
        refused(&through_link, &format!("{} can be written", open.display()));
        assert!(!ran.exists(), "a refused ptyd never ran");

        // The control: trusted again, it is started (and the stand-in fails).
        let why = launch(&options, Instant::now() + Duration::from_secs(5)).expect_err("fails");
        assert!(why.contains("did not start"), "{why}");
        assert!(ran.exists(), "the stand-in ran");
    }

    /// A ptyd downloaded from the web (with a `Zone.Identifier` stream) is refused before it is
    /// started.
    #[cfg(windows)]
    #[test]
    fn a_downloaded_ptyd_is_refused_before_it_runs() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let ptyd = tmp.path().join(PTYD);
        std::fs::write(&ptyd, "not a program").expect("stand-in");
        let mut options = PtyOptions::new(format!(
            r"\\.\pipe\pitcrew-ptyd-unit-zone-{}",
            std::process::id()
        ));
        options.ptyd = ptyd.clone();
        assert_eq!(check_trusted(&ptyd), Ok(()));
        assert!(crate::pty::detect(&options).is_ok());
        let mut stream = ptyd.as_os_str().to_owned();
        stream.push(":Zone.Identifier");
        std::fs::write(PathBuf::from(stream), "[ZoneTransfer]\r\nZoneId=3\r\n")
            .expect("mark it downloaded");
        refused(&options, "Zone.Identifier");
    }
}
