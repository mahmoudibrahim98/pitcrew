//! A portable copy: PitCrew run from an unzipped folder instead of installed (the portable
//! Windows zip, `packaging/portable`).
//!
//! - **Detected** by [`MARKER`] (`portable.txt`) next to the app's executable, which the zip
//!   ships, **on Windows only**, and never next to the installer's `uninstall.exe`: an installed
//!   copy stays installed whatever file appears beside it ([`detect`]).
//! - **Its channel** is the marker's `channel=` line: `channel=release` for a zip built from a
//!   release tag, which offers newer releases; anything else (`channel=main`) for a development
//!   build from `main` or a pull request, which offers no updates ([`Channel`]).
//! - **The installed layout.** On Windows the installer puts `pitcrewd.exe`, `pitcrew-ptyd.exe`,
//!   `pitcrew-askpass.exe` and `pitcrew.exe` next to `pitcrew-desktop.exe`, and `helpers/` beside
//!   them, and the zip holds the same, so the usual lookups find them: `pitcrewd` and
//!   `pitcrew-askpass` next to the app ([`crate::daemon::locate`],
//!   [`crate::remote::helpers::locate_askpass`]), `pitcrew-ptyd` and `pitcrew` next to
//!   `pitcrewd` (the daemon's own lookups), the remote helpers in `helpers/` with the checksums
//!   compiled into the app ([`crate::remote::helpers`]). They pass the same trust check as
//!   installed ones: on Windows a program still marked as downloaded (`Zone.Identifier`) is
//!   refused, so the zip must be unblocked before it is unzipped.
//! - **State where the installed app keeps it.** Nothing is written next to the executable: the
//!   daemon's state directory, the app's settings and its workspace list are in the person's own
//!   folders, whichever copy runs, so moving between the zip and the installer keeps them.
//! - **Updates are never installed** ([`crate::updater`]): a release-channel copy shows the new
//!   release and opens its page, where the new zip is.
//! - **Notifications** on Windows are sent as PowerShell's AppUserModelID, as a debug build's
//!   are: Windows shows toasts only for an id that a Start menu shortcut registers, which only
//!   the installer makes ([`crate::notify`]).
//! - **[`CHECK_LAYOUT`]** (`pitcrew-desktop --check-layout`) prints what the app finds in its
//!   folder and exits, without a window: [`check_layout_main`].

use crate::daemon::locate::{self, LocateError, PITCREWD};
use crate::remote::helpers::{self, ASKPASS, Helpers};
use crate::settings::Settings;
use pitcrew_remote::Platform;
use std::fmt::Write as _;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// The file next to the executable that marks a portable copy.
pub const MARKER: &str = "portable.txt";

/// The NSIS installer's uninstaller, next to an installed copy's executable.
pub const UNINSTALLER: &str = "uninstall.exe";

/// The marker's line for a zip built from a release tag.
pub const RELEASE_CHANNEL: &str = "channel=release";

/// The argument that runs [`check_layout_main`] instead of the app.
pub const CHECK_LAYOUT: &str = "--check-layout";

/// The app's identifier (`tauri.conf.json`), which names its config directory.
pub const IDENTIFIER: &str = "org.pitcrew.desktop";

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

/// The largest marker read.
const MAX_MARKER: u64 = 4096;

/// Which builds a portable copy is offered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channel {
    /// Built from a release tag: newer releases are offered (shown, never installed).
    Release,
    /// Built from `main` or a pull request: nothing is offered.
    Development,
}

/// What the app's folder says it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Detected {
    /// No marker: installed, or run from a build folder.
    Installed,
    /// A portable copy, on this channel.
    Portable(Channel),
    /// A marker that does not count, and why.
    Ignored(&'static str),
}

/// The folder of the app's executable.
#[must_use]
pub fn app_dir() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
}

/// What `dir`, the app's folder, holds: see the module's docs.
#[must_use]
pub fn detect(dir: &Path) -> Detected {
    detect_on(dir, cfg!(windows))
}

fn detect_on(dir: &Path, windows: bool) -> Detected {
    let marker = dir.join(MARKER);
    if !marker.is_file() {
        return Detected::Installed;
    }
    if !windows {
        return Detected::Ignored("portable.txt counts on Windows only");
    }
    if dir.join(UNINSTALLER).exists() {
        return Detected::Ignored("portable.txt is next to an installed PitCrew (uninstall.exe)");
    }
    Detected::Portable(read_channel(&marker))
}

/// The marker's channel: [`Channel::Release`] only for a [`RELEASE_CHANNEL`] line.
fn read_channel(marker: &Path) -> Channel {
    let mut text = String::new();
    let read =
        std::fs::File::open(marker).and_then(|f| f.take(MAX_MARKER).read_to_string(&mut text));
    if read.is_ok() && text.lines().any(|line| line.trim() == RELEASE_CHANNEL) {
        Channel::Release
    } else {
        Channel::Development
    }
}

/// This app's channel, if it is a portable copy.
#[must_use]
pub fn channel() -> Option<Channel> {
    match app_dir().map(|dir| detect(&dir)) {
        Some(Detected::Portable(channel)) => Some(channel),
        _ => None,
    }
}

/// Whether this app is a portable copy.
#[must_use]
pub fn here() -> bool {
    channel().is_some()
}

/// Logs what the app's folder says it is, once at start.
pub fn log_detected() {
    match app_dir().map(|dir| detect(&dir)) {
        Some(Detected::Portable(channel)) => {
            tracing::info!(?channel, "a portable copy: updates are never installed");
        }
        Some(Detected::Ignored(why)) => tracing::warn!("{why}: ignored, so this copy is installed"),
        _ => {}
    }
}

/// The app's config directory, as Tauri names it (`settings.json` is there).
#[must_use]
pub fn config_dir() -> Option<PathBuf> {
    directories::BaseDirs::new().map(|dirs| dirs.config_dir().join(IDENTIFIER))
}

/// One program `settings.json` (or its variable) names instead of the one next to the app: the
/// setting, the path, and the path to run or why it cannot be used.
pub type Configured = (&'static str, PathBuf, Result<PathBuf, String>);

/// What the app finds in its folder: the programs it and its daemon run from there, the
/// settings that replace them, the remote helpers, and whether it is a portable copy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    /// The folder.
    pub dir: PathBuf,
    /// What the folder says it is.
    pub detected: Detected,
    /// Each program next to the app, by file name: its path, or why it cannot be used.
    pub programs: Vec<(&'static str, Result<PathBuf, String>)>,
    /// What the settings replace. The app runs these, not the ones next to it.
    pub configured: Vec<Configured>,
    /// The helpers' folder `settings.json` names instead of `helpers/`, if any.
    pub helpers_dir: Option<PathBuf>,
    /// Each remote platform's helper: the file and its version, or why there is none.
    pub helpers: Vec<(Platform, Result<String, String>)>,
    /// The daemon's state directory.
    pub state: Option<PathBuf>,
}

impl Layout {
    /// Every program the app would run is there and passes the trust check, and there is a state
    /// directory.
    #[must_use]
    pub fn ok(&self) -> bool {
        let configured = |setting: &str| self.configured.iter().any(|(s, _, _)| *s == setting);
        self.programs
            .iter()
            // A program the settings replace is not run from here.
            .filter(|(name, _)| {
                !((*name == PITCREWD && configured("pitcrewd"))
                    || (*name == ASKPASS && configured("askpass")))
            })
            .all(|(_, found)| found.is_ok())
            && self.configured.iter().all(|(_, _, found)| found.is_ok())
            && self.state.is_some()
    }

    /// The layout, for people: one line each.
    #[must_use]
    pub fn report(&self) -> String {
        let mut out = format!("PitCrew in {}\n", self.dir.display());
        let _ = match self.detected {
            Detected::Portable(Channel::Release) => {
                writeln!(out, "portable: yes, release channel (portable.txt is here)")
            }
            Detected::Portable(Channel::Development) => writeln!(
                out,
                "portable: yes, development build: no updates offered (portable.txt is here)"
            ),
            Detected::Installed => writeln!(out, "portable: no (no portable.txt here)"),
            Detected::Ignored(why) => writeln!(out, "portable: no: {why}"),
        };
        for (name, found) in &self.programs {
            let _ = match found {
                Ok(_) => writeln!(out, "ok: {name}"),
                Err(why) => writeln!(out, "not usable: {name}: {why}"),
            };
        }
        for (setting, path, found) in &self.configured {
            let _ = match found {
                Ok(_) => writeln!(out, "settings: {setting} is {}: ok", path.display()),
                Err(why) => writeln!(
                    out,
                    "settings: {setting} is {}: not usable: {why}",
                    path.display()
                ),
            };
        }
        if let Some(dir) = &self.helpers_dir {
            let _ = writeln!(out, "settings: helpers is {}", dir.display());
        }
        for (platform, found) in &self.helpers {
            let _ = match found {
                Ok(what) => writeln!(out, "helper {}: ok ({what})", platform.artefact()),
                Err(why) => writeln!(out, "helper {}: none: {why}", platform.artefact()),
            };
        }
        let _ = match &self.state {
            Some(state) => writeln!(out, "state: {}", state.display()),
            None => writeln!(out, "state: cannot find this user's local data folder"),
        };
        out
    }
}

/// Looks for `pitcrewd`, `pitcrew-ptyd`, `pitcrew-askpass` and `pitcrew` in `dir` (never on
/// `PATH`), and the remote helpers in `dir/helpers` with the checksums compiled into the app,
/// each with the check it passes before it runs or is deployed; reports what `settings` replaces
/// and the state directory it gives.
#[must_use]
pub fn check_layout(dir: &Path, settings: &Settings) -> Layout {
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
    let mut configured: Vec<Configured> = Vec::new();
    if let Some(path) = &settings.pitcrewd {
        let found = locate::locate(Some(path), None, None).map_err(|e| e.to_string());
        configured.push(("pitcrewd", path.clone(), found));
    }
    if let Some(path) = &settings.askpass {
        let found = helpers::locate_askpass(Some(path), None);
        configured.push(("askpass", path.clone(), found));
    }
    // As the app's remote options choose them.
    let found = match &settings.helpers {
        Some(helpers_dir) => Helpers::new(vec![helpers_dir.clone()]),
        None => Helpers::new(vec![dir.join("helpers")]).with_native(Some(dir)),
    };
    let helpers = Platform::ALL
        .into_iter()
        .map(|platform| {
            let helper = found.find(platform).map(|helper| {
                let path = helper.path.strip_prefix(dir).unwrap_or(&helper.path);
                format!("{}, version {}", path.display(), helper.version)
            });
            (platform, helper)
        })
        .collect();
    Layout {
        dir: dir.to_path_buf(),
        detected: detect(dir),
        programs,
        configured,
        helpers_dir: settings.helpers.clone(),
        helpers,
        state: settings.state_dir_or_default(),
    }
}

/// `pitcrew-desktop --check-layout`: prints [`check_layout`] for the app's folder with the
/// app's own settings, and on Windows the WebView2 version, then exits 0 when the app could run
/// from here, 1 otherwise. Opens no window, starts nothing and writes nothing.
#[must_use]
pub fn check_layout_main() -> ExitCode {
    let Some(dir) = app_dir() else {
        let _ = writeln!(std::io::stderr(), "cannot find the app's own folder");
        return ExitCode::FAILURE;
    };
    let settings = config_dir()
        .map(|config| Settings::load(&config))
        .unwrap_or_default()
        .with_env(|name| std::env::var_os(name));
    let layout = check_layout(&dir, &settings);
    let mut out = layout.report();
    let (webview_ok, webview) = webview2();
    out.push_str(&webview);
    let ok = layout.ok() && webview_ok;
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

/// Whether WebView2 is there, and a line saying so (Windows).
#[cfg(windows)]
fn webview2() -> (bool, String) {
    match tauri::webview_version() {
        Ok(version) => (true, format!("WebView2: {version}\n")),
        Err(e) => (
            false,
            format!("WebView2: missing ({e}). {WEBVIEW2_MISSING}\n"),
        ),
    }
}

/// Elsewhere the app's webview is the system's: nothing to report.
#[cfg(not(windows))]
fn webview2() -> (bool, String) {
    (true, String::new())
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

    fn settings_with_state(dir: &Path) -> Settings {
        Settings {
            state_dir: Some(dir.join("state")),
            ..Settings::default()
        }
    }

    #[test]
    fn the_unzipped_folder_has_everything_the_app_runs() {
        let tmp = folder();
        std::fs::write(tmp.path().join(MARKER), "portable\nchannel=release\n").unwrap();
        let layout = check_layout(tmp.path(), &settings_with_state(tmp.path()));
        assert!(layout.ok(), "{}", layout.report());
        let report = layout.report();
        if cfg!(windows) {
            assert_eq!(layout.detected, Detected::Portable(Channel::Release));
            assert!(
                report.contains("portable: yes, release channel"),
                "{report}"
            );
        } else {
            assert_eq!(
                layout.detected,
                Detected::Ignored("portable.txt counts on Windows only")
            );
        }
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
        let state = format!("state: {}\n", tmp.path().join("state").display());
        assert!(report.contains(&state), "{report}");
        // No helpers folder: each platform says why, and the layout is still usable.
        assert_eq!(layout.helpers.len(), Platform::ALL.len());
        assert!(
            report.contains("helper pitcrewd-x86_64-unknown-linux-musl: none: "),
            "{report}"
        );
    }

    #[test]
    fn the_marker_counts_on_windows_only_and_never_beside_the_installer() {
        let tmp = folder();
        assert_eq!(detect_on(tmp.path(), true), Detected::Installed);
        std::fs::write(tmp.path().join(MARKER), "portable\r\nchannel=release\r\n").unwrap();
        assert_eq!(
            detect_on(tmp.path(), true),
            Detected::Portable(Channel::Release)
        );
        assert!(matches!(detect_on(tmp.path(), false), Detected::Ignored(_)));
        // Any other channel, or none, is a development build.
        std::fs::write(tmp.path().join(MARKER), "portable\nchannel=main\n").unwrap();
        assert_eq!(
            detect_on(tmp.path(), true),
            Detected::Portable(Channel::Development)
        );
        std::fs::write(tmp.path().join(MARKER), "channel=release-candidate\n").unwrap();
        assert_eq!(
            detect_on(tmp.path(), true),
            Detected::Portable(Channel::Development)
        );
        // Next to the installer's uninstaller, it is an installed copy whatever the marker says.
        std::fs::write(tmp.path().join(UNINSTALLER), "MZ").unwrap();
        assert_eq!(
            detect_on(tmp.path(), true),
            Detected::Ignored("portable.txt is next to an installed PitCrew (uninstall.exe)")
        );
        // A folder named portable.txt is not the marker.
        let other = folder();
        std::fs::create_dir(other.path().join(MARKER)).unwrap();
        assert_eq!(detect_on(other.path(), true), Detected::Installed);
    }

    #[test]
    fn a_missing_program_is_named() {
        let tmp = folder();
        std::fs::remove_file(tmp.path().join(CLI)).unwrap();
        let layout = check_layout(tmp.path(), &settings_with_state(tmp.path()));
        assert!(!layout.ok());
        let report = layout.report();
        assert!(
            report.contains(&format!("not usable: {CLI}: it is not in ")),
            "{report}"
        );
        assert!(report.contains(&format!("ok: {PITCREWD}\n")), "{report}");
    }

    /// What `settings.json` names instead is what the app runs: reported, and checked.
    #[test]
    fn settings_overrides_are_reported_and_checked() {
        let tmp = folder();
        std::fs::remove_file(tmp.path().join(PITCREWD)).unwrap();
        let elsewhere = folder();
        let settings = Settings {
            pitcrewd: Some(elsewhere.path().join(PITCREWD)),
            askpass: Some(tmp.path().join("missing").join(ASKPASS)),
            helpers: Some(tmp.path().join("my-helpers")),
            ..settings_with_state(tmp.path())
        };
        let layout = check_layout(tmp.path(), &settings);
        let report = layout.report();
        let pitcrewd = format!(
            "settings: pitcrewd is {}: ok",
            elsewhere.path().join(PITCREWD).display()
        );
        assert!(report.contains(&pitcrewd), "{report}");
        assert!(
            report.contains("settings: askpass is ") && report.contains("is not a program"),
            "{report}"
        );
        let helpers = format!(
            "settings: helpers is {}",
            tmp.path().join("my-helpers").display()
        );
        assert!(report.contains(&helpers), "{report}");
        // The configured askpass is missing: not usable, though one is next to the app.
        assert!(!layout.ok());
        // pitcrewd is missing here, but the configured one is what runs.
        let fixed = Settings {
            askpass: None,
            ..settings
        };
        assert!(check_layout(tmp.path(), &fixed).ok());
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
        let layout = check_layout(tmp.path(), &settings_with_state(tmp.path()));
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
        let layout = check_layout(tmp.path(), &settings_with_state(tmp.path()));
        assert!(!layout.ok());
        assert!(
            layout.report().contains("can be written by other users"),
            "{}",
            layout.report()
        );
    }

    #[test]
    fn the_identifier_is_the_apps_and_the_message_links_the_bootstrapper() {
        let config: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        assert_eq!(config["identifier"], IDENTIFIER);
        assert!(WEBVIEW2_MISSING.contains(WEBVIEW2_BOOTSTRAPPER));
    }
}
