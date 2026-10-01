//! Registering `pitcrew://` with the desktop.
//!
//! - **Installed apps** register it at install time, from `plugins.deep-link.desktop.schemes` in
//!   `tauri.conf.json`, which Tauri's bundler reads: the macOS bundle's `CFBundleURLTypes`, the
//!   Linux packages' `.desktop` file (`MimeType=x-scheme-handler/pitcrew`), and the Windows
//!   installers' `HKCU\Software\Classes\pitcrew` key.
//! - **On Linux, an AppImage or a debug build** is not installed, so it registers itself when it
//!   starts, as `xdg-mime default` would: a hidden `.desktop` file in
//!   `$XDG_DATA_HOME/applications` that runs this program with the link, and that file as the
//!   default for `x-scheme-handler/pitcrew` in `$XDG_CONFIG_HOME/mimeapps.list` (only that key is
//!   touched). The default matters where `update-desktop-database` is not installed: GIO and
//!   `xdg-open` then find a handler only through `mimeapps.list`.
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
        let Some(dirs) = directories::BaseDirs::new() else {
            tracing::info!("no home directory: pitcrew:// links are not registered");
            return;
        };
        let registered = program.map_err(|e| e.to_string()).and_then(|p| {
            let wrote = linux::write_handler(&dirs.data_dir().join("applications"), &p)
                .map_err(|e| e.to_string())?;
            let defaulted = linux::set_default(&dirs.config_dir().join("mimeapps.list"))
                .map_err(|e| e.to_string())?;
            Ok(wrote || defaulted)
        });
        match registered {
            Ok(true) => tracing::info!("registered pitcrew:// links with the desktop"),
            Ok(false) => {}
            Err(e) => tracing::info!(error = %e, "cannot register pitcrew:// links"),
        }
    }
}

#[cfg(target_os = "linux")]
pub mod linux {
    //! The `.desktop` handler, and the default in `mimeapps.list`.

    use super::HANDLER;
    use std::io;
    use std::path::Path;

    /// The key this app sets in `mimeapps.list`.
    const MIME: &str = "x-scheme-handler/pitcrew";
    /// The section defaults are in.
    const DEFAULTS: &str = "[Default Applications]";
    /// The longest `mimeapps.list` read.
    const MAX_LIST: u64 = 1024 * 1024;

    /// `list` (a `mimeapps.list`) with this app's handler as the default for `pitcrew://`, or
    /// `None` when it already is. Every other line is kept as it is.
    #[must_use]
    pub fn with_default(list: &str) -> Option<String> {
        let entry = format!("{MIME}={HANDLER}");
        let mut lines: Vec<String> = list.lines().map(str::to_owned).collect();
        let mut section = None;
        let mut existing = None;
        let mut current = String::new();
        for (i, line) in lines.iter().enumerate() {
            let trimmed = line.trim();
            if trimmed.starts_with('[') {
                current = trimmed.to_owned();
                if trimmed == DEFAULTS && section.is_none() {
                    section = Some(i);
                }
            } else if current == DEFAULTS
                && trimmed
                    .split_once('=')
                    .is_some_and(|(key, _)| key.trim() == MIME)
            {
                existing = Some(i);
            }
        }
        match (section, existing) {
            (_, Some(i)) => {
                let value = lines[i].split_once('=').map_or("", |(_, v)| v.trim());
                if value.trim_end_matches(';') == HANDLER {
                    return None;
                }
                lines[i] = entry;
            }
            (Some(i), None) => lines.insert(i + 1, entry),
            (None, None) => {
                if lines.last().is_some_and(|l| !l.trim().is_empty()) {
                    lines.push(String::new());
                }
                lines.push(DEFAULTS.to_owned());
                lines.push(entry);
            }
        }
        let mut out = lines.join("\n");
        out.push('\n');
        Some(out)
    }

    /// Makes this app's handler the default for `pitcrew://` in the `mimeapps.list` at `list`.
    /// Returns whether it wrote.
    ///
    /// # Errors
    /// The file cannot be read (other than missing) or written.
    pub fn set_default(list: &Path) -> io::Result<bool> {
        use std::io::Read as _;
        let mut text = String::new();
        match std::fs::File::open(list) {
            Ok(f) => {
                f.take(MAX_LIST).read_to_string(&mut text)?;
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        match with_default(&text) {
            Some(updated) => {
                crate::registry::write_private(list, updated.as_bytes())?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

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

        #[test]
        fn the_default_in_mimeapps_list() {
            let ours = format!("{MIME}={HANDLER}");
            // No file, or no section: the section is added.
            assert_eq!(with_default("").unwrap(), format!("{DEFAULTS}\n{ours}\n"));
            let other = "[Added Associations]\ntext/plain=editor.desktop;\n";
            assert_eq!(
                with_default(other).unwrap(),
                format!("{other}\n{DEFAULTS}\n{ours}\n")
            );
            // A section: inserted first in it; everything else kept.
            let list = "[Default Applications]\ntext/html=browser.desktop\nx-scheme-handler/http=browser.desktop\n\n[Added Associations]\nx-scheme-handler/pitcrew=other.desktop;\n";
            let updated = with_default(list).unwrap();
            assert_eq!(
                updated,
                format!(
                    "[Default Applications]\n{ours}\ntext/html=browser.desktop\nx-scheme-handler/http=browser.desktop\n\n[Added Associations]\nx-scheme-handler/pitcrew=other.desktop;\n"
                )
            );
            assert_eq!(with_default(&updated), None, "already the default");
            // Another handler as the default: replaced, in place.
            let list = "[Default Applications]\nx-scheme-handler/pitcrew = old.desktop\ntext/html=browser.desktop\n";
            assert_eq!(
                with_default(list).unwrap(),
                format!("[Default Applications]\n{ours}\ntext/html=browser.desktop\n")
            );
            assert_eq!(
                with_default(&format!("{DEFAULTS}\n{ours};\n")),
                None,
                "a trailing ; is the same default"
            );

            let tmp = tempfile::tempdir().unwrap();
            let file = tmp.path().join("config").join("mimeapps.list");
            assert!(set_default(&file).unwrap());
            assert!(!set_default(&file).unwrap());
            assert_eq!(
                std::fs::read_to_string(&file).unwrap(),
                format!("{DEFAULTS}\n{ours}\n")
            );
        }
    }
}
