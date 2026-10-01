//! Registering `pitcrew://` with the desktop.
//!
//! - **Installed apps** register it at install time, from `plugins.deep-link.desktop.schemes` in
//!   `tauri.conf.json`, which Tauri's bundler reads: the macOS bundle's `CFBundleURLTypes`, the
//!   Linux packages' `.desktop` file (`MimeType=x-scheme-handler/pitcrew`), and the Windows
//!   installers' `HKCU\Software\Classes\pitcrew` key.
//! - **On Linux, an AppImage or a debug build** is not installed, so it registers itself when it
//!   starts: a hidden `.desktop` file in `$XDG_DATA_HOME/applications` that runs this program with
//!   the link. The person's default applications (`mimeapps.list`) are not touched; the handler is
//!   found because it is the only one for the scheme.
//!
//! Either way the link arrives as a command-line argument (on macOS as an Apple Event), and only
//! [`crate::navigate::parse_link`] decides what it opens.

/// The handler's file name in `applications/`.
#[cfg(target_os = "linux")]
pub const HANDLER: &str = "org.pitcrew.desktop-url-handler.desktop";

/// Registers this program as the `pitcrew://` handler, where the platform needs it at run time
/// (Linux, for an AppImage or a debug build). Never fails the app: problems are logged.
pub fn register() {
    #[cfg(target_os = "linux")]
    {
        let appimage = std::env::var_os("APPIMAGE");
        if !cfg!(debug_assertions) && appimage.is_none() {
            return;
        }
        let program = match appimage {
            Some(path) => Ok(std::path::PathBuf::from(path)),
            None => std::env::current_exe(),
        };
        let Some(data) = directories::BaseDirs::new().map(|d| d.data_dir().to_path_buf()) else {
            tracing::info!("no data directory: pitcrew:// links are not registered");
            return;
        };
        match program.map_err(|e| e.to_string()).and_then(|p| {
            linux::write_handler(&data.join("applications"), &p).map_err(|e| e.to_string())
        }) {
            Ok(true) => tracing::info!("registered pitcrew:// links with the desktop"),
            Ok(false) => {}
            Err(e) => tracing::info!(error = %e, "cannot register pitcrew:// links"),
        }
    }
}

#[cfg(target_os = "linux")]
pub mod linux {
    //! The `.desktop` handler.

    use super::HANDLER;
    use std::io;
    use std::path::Path;

    /// The handler's text for `program`, or `None` when its path cannot be written in a desktop
    /// entry's `Exec` without escaping (quotes, `$`, `` ` ``, `\`, `%`, controls, non-UTF-8).
    #[must_use]
    pub fn handler_text(program: &Path) -> Option<String> {
        let path = program.to_str()?;
        if !program.is_absolute()
            || path
                .chars()
                .any(|c| c.is_control() || matches!(c, '"' | '`' | '$' | '\\' | '%'))
        {
            return None;
        }
        Some(format!(
            "[Desktop Entry]\nType=Application\nName=PitCrew\nExec=\"{path}\" %u\nTerminal=false\nNoDisplay=true\nMimeType=x-scheme-handler/pitcrew;\n"
        ))
    }

    /// Writes the handler for `program` into `applications` if it is missing or different, then
    /// refreshes that directory's MIME cache when `update-desktop-database` is installed. Returns
    /// whether it wrote.
    ///
    /// # Errors
    /// The path cannot go in a desktop entry, or the file cannot be written.
    pub fn write_handler(applications: &Path, program: &Path) -> io::Result<bool> {
        let text = handler_text(program).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "the program's path cannot go in a desktop entry",
            )
        })?;
        let file = applications.join(HANDLER);
        if std::fs::read_to_string(&file).is_ok_and(|old| old == text) {
            return Ok(false);
        }
        crate::registry::write_private(&file, text.as_bytes())?;
        if let Err(e) = std::process::Command::new("update-desktop-database")
            .arg(applications)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
        {
            tracing::debug!(error = %e, "update-desktop-database did not run");
        }
        Ok(true)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn the_handler() {
            let text = handler_text(Path::new("/opt/PitCrew/pitcrew-desktop")).unwrap();
            assert!(text.contains("Exec=\"/opt/PitCrew/pitcrew-desktop\" %u\n"));
            assert!(text.contains("MimeType=x-scheme-handler/pitcrew;\n"));
            assert!(text.contains("NoDisplay=true\n"));
            for bad in [
                "relative/pitcrew-desktop",
                "/opt/a\"b/pitcrew-desktop",
                "/opt/$HOME/pitcrew-desktop",
                "/opt/100%/pitcrew-desktop",
                "/opt/a`id`/pitcrew-desktop",
                "/opt/a\\b/pitcrew-desktop",
                "/opt/a\nExec=evil/pitcrew-desktop",
            ] {
                assert_eq!(handler_text(Path::new(bad)), None, "{bad:?}");
            }

            let tmp = tempfile::tempdir().unwrap();
            let apps = tmp.path().join("applications");
            let program = Path::new("/opt/PitCrew/pitcrew-desktop");
            assert!(write_handler(&apps, program).unwrap());
            assert!(!write_handler(&apps, program).unwrap(), "unchanged");
            assert_eq!(
                std::fs::read_to_string(apps.join(HANDLER)).unwrap(),
                handler_text(program).unwrap()
            );
            assert!(write_handler(&apps, Path::new("/other/pitcrew-desktop")).unwrap());
        }
    }
}
