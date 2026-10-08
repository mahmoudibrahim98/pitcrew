//! A portable copy: PitCrew run from an unzipped folder instead of installed (the portable
//! Windows zip, `packaging/portable`).
//!
//! - **Detected** by [`MARKER`] (`portable.txt`) next to the app's executable, which the zip
//!   ships. What it says does not matter.
//! - **The installed layout.** On Windows the installer puts `pitcrewd.exe`, `pitcrew-ptyd.exe`,
//!   `pitcrew-askpass.exe` and `pitcrew.exe` next to `pitcrew-desktop.exe`, and the zip holds
//!   the same five side by side, so the usual lookups find them: `pitcrewd` and
//!   `pitcrew-askpass` next to the app ([`crate::daemon::locate`],
//!   [`crate::remote::helpers::locate_askpass`]), `pitcrew-ptyd` and `pitcrew` next to
//!   `pitcrewd` (the daemon's own lookups). They pass the same trust check as installed ones: on
//!   Windows a program still marked as downloaded (`Zone.Identifier`) is refused, so the zip must
//!   be unblocked before it is unzipped.
//! - **State where the installed app keeps it.** Nothing is written next to the executable: the
//!   daemon's state directory, the app's settings and its workspace list are in the person's own
//!   folders, whichever copy runs, so moving between the zip and the installer keeps them.
//! - **Updates are never installed** ([`crate::updater`]): the app shows the new version and
//!   opens the page with the newest portable zip instead.
//! - **Notifications** on Windows are sent as PowerShell's AppUserModelID, as a debug build's
//!   are: Windows shows toasts only for an id that a Start menu shortcut registers, which only
//!   the installer makes ([`crate::notify`]).
//! - **[`CHECK_LAYOUT`]** (`pitcrew-desktop --check-layout`) prints what the app finds in its
//!   folder and exits, without a window: [`check_layout_main`].

use crate::daemon::locate::{self, LocateError, PITCREWD};
use crate::remote::helpers::ASKPASS;
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// The file next to the executable that marks a portable copy.
pub const MARKER: &str = "portable.txt";

/// The argument that runs [`check_layout_main`] instead of the app.
pub const CHECK_LAYOUT: &str = "--check-layout";

/// Microsoft's Evergreen Bootstrapper for the WebView2 runtime.
pub const WEBVIEW2_BOOTSTRAPPER: &str = "https://go.microsoft.com/fwlink/p/?LinkId=2124703";

/// What the app says when WebView2 is missing.
pub const WEBVIEW2_MISSING: &str = "PitCrew's window needs Microsoft Edge WebView2, which is not \
     installed for this user. Windows 10 and 11 include it; to install it again, run the \
     Evergreen Bootstrapper from https://go.microsoft.com/fwlink/p/?LinkId=2124703";

/// `pitcrew-ptyd`'s file name on this platform (pitcrewd runs it from its own folder).
pub const PTYD: &str = if cfg!(windows) {
    "pitcrew-ptyd.exe"
} else {
    "pitcrew-ptyd"
};

/// The agent CLI's file name on this platform (pitcrewd installs hooks that run it from its own
/// folder).
pub const CLI: &str = if cfg!(windows) {
    "pitcrew.exe"
} else {
    "pitcrew"
};

/// The folder of the app's executable.
#[must_use]
pub fn app_dir() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
}

/// Whether `dir`, the app's folder, holds a portable copy.
#[must_use]
pub fn is_portable(dir: &Path) -> bool {
    dir.join(MARKER).is_file()
}

/// Whether this app is a portable copy.
#[must_use]
pub fn here() -> bool {
    app_dir().is_some_and(|dir| is_portable(&dir))
}

/// What the app finds in its folder: the programs it and its daemon run from there, and whether
/// it is a portable copy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    /// The folder.
    pub dir: PathBuf,
    /// [`MARKER`] is there.
    pub portable: bool,
    /// Each program, by file name: its path, or why it cannot be used.
    pub programs: Vec<(&'static str, Result<PathBuf, String>)>,
}

impl Layout {
    /// Every program is there and passes the trust check.
    #[must_use]
    pub fn ok(&self) -> bool {
        self.programs.iter().all(|(_, found)| found.is_ok())
    }

    /// The layout, for people: one line each.
    #[must_use]
    pub fn report(&self) -> String {
        let mut out = format!("PitCrew in {}\n", self.dir.display());
        let _ = writeln!(
            out,
            "portable: {}",
            if self.portable {
                "yes (portable.txt is here)"
            } else {
                "no (no portable.txt here)"
            }
        );
        for (name, found) in &self.programs {
            let _ = match found {
                Ok(_) => writeln!(out, "ok: {name}"),
                Err(why) => writeln!(out, "not usable: {name}: {why}"),
            };
        }
        out
    }
}

/// Looks for `pitcrewd`, `pitcrew-ptyd`, `pitcrew-askpass` and `pitcrew` in `dir` (never on
/// `PATH`, never in the settings), with the trust check each passes before it runs.
#[must_use]
pub fn check_layout(dir: &Path) -> Layout {
    let programs = [PITCREWD, PTYD, ASKPASS, CLI]
        .into_iter()
        .map(|name| {
            let found = locate::locate_named(name, None, Some(dir), None).map_err(|e| match e {
                LocateError::NotFound => format!("it is not in {}", dir.display()),
                other => other.to_string(),
            });
            (name, found)
        })
        .collect();
    Layout {
        dir: dir.to_path_buf(),
        portable: is_portable(dir),
        programs,
    }
}

/// `pitcrew-desktop --check-layout`: prints [`check_layout`] for the app's folder, the default
/// state directory, and on Windows the WebView2 version, then exits 0 when the app could run from
/// here, 1 otherwise. Opens no window and starts nothing.
#[must_use]
pub fn check_layout_main() -> ExitCode {
    let Some(dir) = app_dir() else {
        let _ = writeln!(std::io::stderr(), "cannot find the app's own folder");
        return ExitCode::FAILURE;
    };
    let layout = check_layout(&dir);
    let mut ok = layout.ok();
    let mut out = layout.report();
    match crate::settings::default_state_dir() {
        Some(state) => {
            let _ = writeln!(
                out,
                "state: {} (the installed app's too, unless settings.json says otherwise)",
                state.display()
            );
        }
        None => {
            ok = false;
            let _ = writeln!(out, "state: cannot find this user's local data folder");
        }
    }
    #[cfg(windows)]
    {
        match tauri::webview_version() {
            Ok(version) => {
                let _ = writeln!(out, "WebView2: {version}");
            }
            Err(e) => {
                ok = false;
                let _ = writeln!(out, "WebView2: missing ({e}). {WEBVIEW2_MISSING}");
            }
        }
    }
    let _ = writeln!(
        out,
        "{}",
        if ok {
            "layout: ok"
        } else {
            "layout: PitCrew cannot run from here as it is"
        }
    );
    // A Windows release build has no console: the output goes where the caller redirected it.
    let _ = std::io::stdout().lock().write_all(out.as_bytes());
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The four programs, as stand-ins, in a folder of ours.
    fn folder() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        for name in [PITCREWD, PTYD, ASKPASS, CLI] {
            let path = tmp.path().join(name);
            std::fs::write(&path, "#!/bin/sh\n").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        tmp
    }

    #[test]
    fn the_unzipped_folder_has_everything_the_app_runs() {
        let tmp = folder();
        std::fs::write(tmp.path().join(MARKER), "portable\n").unwrap();
        let layout = check_layout(tmp.path());
        assert!(layout.ok(), "{}", layout.report());
        assert!(layout.portable);
        assert!(is_portable(tmp.path()));
        let report = layout.report();
        assert!(report.contains("portable: yes"), "{report}");
        for name in [PITCREWD, PTYD, ASKPASS, CLI] {
            assert!(report.contains(&format!("ok: {name}\n")), "{report}");
            assert!(
                layout
                    .programs
                    .iter()
                    .any(|(n, found)| *n == name && found.as_ref() == Ok(&tmp.path().join(name))),
                "{layout:?}"
            );
        }
    }

    #[test]
    fn without_the_marker_it_is_the_installed_layout() {
        let tmp = folder();
        let layout = check_layout(tmp.path());
        assert!(layout.ok());
        assert!(!layout.portable);
        assert!(layout.report().contains("portable: no"));
        // A folder named portable.txt is not the marker.
        std::fs::create_dir(tmp.path().join(MARKER)).unwrap();
        assert!(!is_portable(tmp.path()));
    }

    #[test]
    fn a_missing_program_is_named() {
        let tmp = folder();
        std::fs::remove_file(tmp.path().join(CLI)).unwrap();
        let layout = check_layout(tmp.path());
        assert!(!layout.ok());
        let report = layout.report();
        assert!(
            report.contains(&format!("not usable: {CLI}: it is not in ")),
            "{report}"
        );
        assert!(report.contains(&format!("ok: {PITCREWD}\n")), "{report}");
    }

    /// A program still marked as downloaded is refused, as the app refuses it when it starts.
    #[cfg(windows)]
    #[test]
    fn a_program_marked_as_downloaded_is_refused() {
        let tmp = folder();
        let path = tmp.path().join(PITCREWD);
        let mut stream = path.as_os_str().to_owned();
        stream.push(":Zone.Identifier");
        std::fs::write(PathBuf::from(stream), "[ZoneTransfer]\r\nZoneId=3\r\n").unwrap();
        let layout = check_layout(tmp.path());
        assert!(!layout.ok());
        let report = layout.report();
        assert!(report.contains("Zone.Identifier"), "{report}");
    }

    #[cfg(unix)]
    #[test]
    fn a_program_others_can_write_is_refused() {
        use std::os::unix::fs::PermissionsExt as _;
        let tmp = folder();
        let path = tmp.path().join(PTYD);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o775)).unwrap();
        let layout = check_layout(tmp.path());
        assert!(!layout.ok());
        assert!(
            layout.report().contains("can be written by other users"),
            "{}",
            layout.report()
        );
    }

    #[test]
    fn the_webview2_message_links_the_bootstrapper() {
        assert!(WEBVIEW2_MISSING.contains(WEBVIEW2_BOOTSTRAPPER));
    }
}
