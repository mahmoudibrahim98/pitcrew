//! Registering `pitcrew://` with the desktop.
//!
//! - **Installed apps** register it at install time, from `plugins.deep-link.desktop.schemes` in
//!   `tauri.conf.json`, which Tauri's bundler reads: the macOS bundle's `CFBundleURLTypes`, the
//!   Linux packages' `.desktop` file (`MimeType=x-scheme-handler/pitcrew`), and the Windows
//!   installers' `HKCU\Software\Classes\pitcrew` key.
//! - **On Linux, an AppImage** is not installed, so it registers itself when it starts. It is
//!   trusted to be one only when `APPIMAGE` and `APPDIR` are set and this program runs from inside
//!   `APPDIR`: both are inherited by everything an AppImage starts, so `APPIMAGE` alone could name
//!   another program. **A debug build** registers itself only when `PITCREW_DEV_REGISTER_SCHEME=1`.
//! - **Registering** is what `xdg-mime default` does: a hidden `.desktop` file in
//!   `$XDG_DATA_HOME/applications` that runs the program with the link, and that file as the
//!   default for `x-scheme-handler/pitcrew` in `$XDG_CONFIG_HOME/mimeapps.list` (where
//!   `update-desktop-database` is not installed, GIO and `xdg-open` find a handler only there).
//!   The default is set only when there is none, when it is this handler already, or when it
//!   names a handler that is gone (its `.desktop` file or its program): another installed PitCrew,
//!   or another app, is never replaced. Only that one key is touched; a `mimeapps.list` that is a
//!   link (home-manager, stow, chezmoi) or over 1 MiB is left alone, and the file keeps its mode.
//!
//! Either way the link arrives as a command-line argument (on macOS as an Apple Event), and only
//! [`crate::navigate::parse_link`] decides what it opens.

/// The variable that lets a debug build register itself (`1`).
pub const DEV_REGISTER: &str = "PITCREW_DEV_REGISTER_SCHEME";

/// The handler's file name in `applications/`.
#[cfg(target_os = "linux")]
pub const HANDLER: &str = "org.pitcrew.desktop-url-handler.desktop";

/// Registers this program as the `pitcrew://` handler, where the platform needs it at run time
/// (Linux, for an AppImage, or a debug build that opted in). Never fails the app: problems are
/// logged.
pub fn register() {
    #[cfg(target_os = "linux")]
    {
        let Ok(exe) = std::env::current_exe() else {
            return;
        };
        let Some(program) =
            linux::program_to_register(cfg!(debug_assertions), |name| std::env::var_os(name), &exe)
        else {
            return;
        };
        let Some(dirs) = directories::BaseDirs::new() else {
            tracing::info!("no home directory: pitcrew:// links are not registered");
            return;
        };
        let lookup = linux::Lookup::from_env(dirs.data_dir());
        // The default only names a handler that was written.
        let registered = linux::write_handler(&dirs.data_dir().join("applications"), &program)
            .and_then(|wrote| {
                linux::set_default(&dirs.config_dir().join("mimeapps.list"), &lookup)
                    .map(|defaulted| wrote || defaulted)
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
    //! Which program registers, the `.desktop` handler, and the default in `mimeapps.list`.

    use super::{DEV_REGISTER, HANDLER};
    use std::ffi::OsString;
    use std::io::{self, Read as _};
    use std::path::{Path, PathBuf};

    /// The key this app sets in `mimeapps.list`.
    const MIME: &str = "x-scheme-handler/pitcrew";
    /// The section defaults are in.
    const DEFAULTS: &str = "[Default Applications]";
    /// The longest `mimeapps.list` read; a longer one is left alone.
    pub const MAX_LIST: u64 = 1024 * 1024;
    /// The longest `.desktop` file read to find a handler's program.
    const MAX_ENTRY: u64 = 64 * 1024;

    /// The program to register as the handler, if this run may register at all: the AppImage,
    /// when `APPIMAGE` names a file and this program (`exe`) runs from inside `APPDIR`; else, in a
    /// debug build (`debug`) with `PITCREW_DEV_REGISTER_SCHEME=1`, this program.
    #[must_use]
    pub fn program_to_register(
        debug: bool,
        env: impl Fn(&str) -> Option<OsString>,
        exe: &Path,
    ) -> Option<PathBuf> {
        if let (Some(image), Some(appdir)) = (env("APPIMAGE"), env("APPDIR")) {
            let image = PathBuf::from(image);
            let inside = std::fs::canonicalize(appdir)
                .ok()
                .zip(std::fs::canonicalize(exe).ok())
                .is_some_and(|(dir, exe)| exe.starts_with(dir));
            if inside && image.is_absolute() && image.is_file() {
                return Some(image);
            }
        }
        (debug && env(DEV_REGISTER).is_some_and(|v| v == "1")).then(|| exe.to_path_buf())
    }

    /// Where handlers' `.desktop` files and programs are looked for.
    #[derive(Clone, Debug, Default)]
    pub struct Lookup {
        /// Data directories (`$XDG_DATA_HOME`, then `$XDG_DATA_DIRS`), each with `applications/`.
        pub data_dirs: Vec<PathBuf>,
        /// `PATH`, for programs named without a path.
        pub path: Option<OsString>,
    }

    impl Lookup {
        /// From the environment, with `data_home` first.
        #[must_use]
        pub fn from_env(data_home: &Path) -> Self {
            let mut data_dirs = vec![data_home.to_path_buf()];
            let system = std::env::var_os("XDG_DATA_DIRS")
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| OsString::from("/usr/local/share:/usr/share"));
            data_dirs.extend(std::env::split_paths(&system).filter(|p| p.is_absolute()));
            Self {
                data_dirs,
                path: std::env::var_os("PATH"),
            }
        }

        /// Whether the handler `id` (a desktop file id) is gone: no `.desktop` file for it, or
        /// one whose program does not exist. A handler whose program cannot be read is kept.
        #[must_use]
        pub fn is_gone(&self, id: &str) -> bool {
            let Some(entry) = self.desktop_file(id) else {
                return true;
            };
            match exec_program(&entry) {
                Some(program) => !self.program_exists(&program),
                None => false,
            }
        }

        /// The `.desktop` file for `id`: `applications/<id>`, or with a `-` read as a
        /// subdirectory (`kde-foo.desktop` is `applications/kde/foo.desktop`).
        fn desktop_file(&self, id: &str) -> Option<String> {
            if id.contains('/') || id.starts_with('.') || !id.ends_with(".desktop") {
                return None;
            }
            let mut names = vec![PathBuf::from(id)];
            for (i, _) in id.match_indices('-') {
                let mut name = PathBuf::new();
                for part in id[..i].split('-') {
                    name.push(part);
                }
                name.push(&id[i + 1..]);
                names.push(name);
            }
            self.data_dirs.iter().find_map(|dir| {
                names.iter().find_map(|name| {
                    let file = std::fs::File::open(dir.join("applications").join(name)).ok()?;
                    let mut text = String::new();
                    file.take(MAX_ENTRY).read_to_string(&mut text).ok()?;
                    Some(text)
                })
            })
        }

        fn program_exists(&self, program: &str) -> bool {
            let program = Path::new(program);
            if program.is_absolute() {
                return program.is_file();
            }
            if program.components().count() != 1 {
                return true;
            }
            self.path.as_ref().is_some_and(|path| {
                std::env::split_paths(path).any(|dir| dir.join(program).is_file())
            })
        }
    }

    /// The program of a `.desktop` file's `Exec` key: its first argument, unquoted.
    fn exec_program(entry: &str) -> Option<String> {
        let mut group = "";
        for line in entry.lines() {
            let line = line.trim();
            if line.starts_with('[') {
                group = line;
                continue;
            }
            if group != "[Desktop Entry]" {
                continue;
            }
            let Some(value) = line
                .split_once('=')
                .filter(|(key, _)| key.trim() == "Exec")
                .map(|(_, v)| v.trim_start())
            else {
                continue;
            };
            if let Some(quoted) = value.strip_prefix('"') {
                let mut out = String::new();
                let mut chars = quoted.chars();
                while let Some(c) = chars.next() {
                    match c {
                        '"' => return Some(out),
                        '\\' => out.push(chars.next()?),
                        c => out.push(c),
                    }
                }
                return None;
            }
            return value
                .split_whitespace()
                .next()
                .map(str::to_owned)
                .filter(|p| !p.is_empty());
        }
        None
    }

    /// `list` (a `mimeapps.list`) with this app's handler as the default for `pitcrew://`, or
    /// `None` when nothing changes: it is the default already, or the current default is another
    /// handler that `replaceable` says to keep. Every other line is kept as it is.
    #[must_use]
    pub fn with_default(list: &str, replaceable: impl Fn(&str) -> bool) -> Option<String> {
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
                && existing.is_none()
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
                let first = value.split(';').map(str::trim).find(|id| !id.is_empty());
                match first {
                    Some(HANDLER) => return None,
                    Some(other) if !replaceable(other) => return None,
                    _ => {}
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

    /// Makes this app's handler the default for `pitcrew://` in the `mimeapps.list` at `list`,
    /// unless another handler that is not gone is the default. Returns whether it wrote.
    ///
    /// # Errors
    /// The file is a link, over [`MAX_LIST`], not text, or cannot be read or written; it is then
    /// left as it is.
    pub fn set_default(list: &Path, lookup: &Lookup) -> io::Result<bool> {
        let mut mode = 0o600;
        let mut text = String::new();
        match std::fs::symlink_metadata(list) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("{} is a link; it is left alone", list.display()),
                ));
            }
            Ok(meta) => {
                use std::os::unix::fs::PermissionsExt as _;
                mode = meta.permissions().mode();
                std::fs::File::open(list)?
                    .take(MAX_LIST + 1)
                    .read_to_string(&mut text)?;
                if text.len() as u64 > MAX_LIST {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "{} is over {MAX_LIST} bytes; it is left alone",
                            list.display()
                        ),
                    ));
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        match with_default(&text, |id| lookup.is_gone(id)) {
            Some(updated) => {
                crate::registry::write_atomic(list, updated.as_bytes(), mode)?;
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
        use std::os::unix::fs::PermissionsExt as _;

        #[test]
        fn who_registers() {
            let tmp = tempfile::tempdir().unwrap();
            let appdir = tmp.path().join("mount");
            std::fs::create_dir_all(appdir.join("usr/bin")).unwrap();
            let inside = appdir.join("usr/bin/pitcrew-desktop");
            std::fs::write(&inside, "").unwrap();
            let image = tmp.path().join("PitCrew.AppImage");
            std::fs::write(&image, "").unwrap();
            let elsewhere = tmp.path().join("pitcrew-desktop");
            std::fs::write(&elsewhere, "").unwrap();
            let env =
                |pairs: &'static [(&'static str, &'static str)], image: &Path, appdir: &Path| {
                    let image = image.as_os_str().to_owned();
                    let appdir = appdir.as_os_str().to_owned();
                    move |name: &str| {
                        pairs
                            .iter()
                            .find(|(k, _)| *k == name)
                            .map(|(_, v)| match *v {
                                "<image>" => image.clone(),
                                "<appdir>" => appdir.clone(),
                                v => OsString::from(v),
                            })
                    }
                };
            let both: &[(&str, &str)] = &[("APPIMAGE", "<image>"), ("APPDIR", "<appdir>")];

            // An AppImage, running from inside its mount: the AppImage registers.
            assert_eq!(
                program_to_register(false, env(both, &image, &appdir), &inside),
                Some(image.clone())
            );
            // Another program that inherited an AppImage's variables: not registered.
            assert_eq!(
                program_to_register(false, env(both, &image, &appdir), &elsewhere),
                None
            );
            // APPIMAGE without APPDIR: not trusted.
            let only: &[(&str, &str)] = &[("APPIMAGE", "<image>")];
            assert_eq!(
                program_to_register(false, env(only, &image, &appdir), &inside),
                None
            );
            // An APPIMAGE that is not a file.
            assert_eq!(
                program_to_register(false, env(both, &tmp.path().join("gone"), &appdir), &inside),
                None
            );
            // A debug build registers only when asked; a release build never does.
            let none: &[(&str, &str)] = &[];
            let opted: &[(&str, &str)] = &[(DEV_REGISTER, "1")];
            let other: &[(&str, &str)] = &[(DEV_REGISTER, "yes")];
            assert_eq!(
                program_to_register(true, env(none, &image, &appdir), &elsewhere),
                None
            );
            assert_eq!(
                program_to_register(true, env(other, &image, &appdir), &elsewhere),
                None
            );
            assert_eq!(
                program_to_register(true, env(opted, &image, &appdir), &elsewhere),
                Some(elsewhere.clone())
            );
            assert_eq!(
                program_to_register(false, env(opted, &image, &appdir), &elsewhere),
                None
            );
        }

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
        fn the_default_line() {
            let ours = format!("{MIME}={HANDLER}");
            let any = |_: &str| true;
            // No file, or no section: the section is added.
            assert_eq!(
                with_default("", any).unwrap(),
                format!("{DEFAULTS}\n{ours}\n")
            );
            let other = "[Added Associations]\ntext/plain=editor.desktop;\n";
            assert_eq!(
                with_default(other, any).unwrap(),
                format!("{other}\n{DEFAULTS}\n{ours}\n")
            );
            // A section: inserted first in it; everything else kept.
            let list = "[Default Applications]\ntext/html=browser.desktop\n\n[Added Associations]\nx-scheme-handler/pitcrew=other.desktop;\n";
            let updated = with_default(list, any).unwrap();
            assert_eq!(
                updated,
                format!(
                    "[Default Applications]\n{ours}\ntext/html=browser.desktop\n\n[Added Associations]\nx-scheme-handler/pitcrew=other.desktop;\n"
                )
            );
            assert_eq!(with_default(&updated, any), None, "already the default");
            assert_eq!(with_default(&format!("{DEFAULTS}\n{ours};\n"), any), None);
            // Another handler: replaced in place only if it is gone.
            let list = "[Default Applications]\nx-scheme-handler/pitcrew = old.desktop;b.desktop\ntext/html=browser.desktop\n";
            assert_eq!(with_default(list, |id| id != "old.desktop"), None);
            assert_eq!(
                with_default(list, |id| id == "old.desktop").unwrap(),
                format!("[Default Applications]\n{ours}\ntext/html=browser.desktop\n")
            );
        }

        /// A data directory with `applications/<name>` running `program`.
        fn handler(data: &Path, name: &str, program: &str) {
            let file = data.join("applications").join(name);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(
                file,
                format!("[Desktop Entry]\nName=X\nExec={program} %u\nMimeType={MIME};\n"),
            )
            .unwrap();
        }

        fn list_with(dir: &Path, default: &str) -> PathBuf {
            let file = dir.join("mimeapps.list");
            std::fs::write(
                &file,
                format!("[Default Applications]\n{MIME}={default}\ntext/html=browser.desktop\n"),
            )
            .unwrap();
            file
        }

        #[test]
        fn the_default_in_mimeapps_list() {
            let tmp = tempfile::tempdir().unwrap();
            let data = tmp.path().join("data");
            let lookup = Lookup {
                data_dirs: vec![data.clone()],
                path: Some(OsString::from("/bin:/usr/bin")),
            };
            handler(&data, "pitcrew.desktop", "\"/bin/sh\"");
            handler(&data, "kde/pitcrew.desktop", "sh");
            handler(&data, "moved.desktop", "/nowhere/pitcrew-desktop");
            handler(&data, "unknown.desktop", "not-a-program-anywhere");

            // Another installed PitCrew, or another app: kept (by path, on PATH, in a subdir).
            assert!(!lookup.is_gone("pitcrew.desktop"));
            assert!(!lookup.is_gone("kde-pitcrew.desktop"));
            // Gone: no .desktop file, or its program is not there.
            assert!(lookup.is_gone("missing.desktop"));
            assert!(lookup.is_gone("moved.desktop"));
            assert!(lookup.is_gone("unknown.desktop"));
            assert!(lookup.is_gone("../escape.desktop"));

            let foreign = list_with(tmp.path(), "pitcrew.desktop");
            let before = std::fs::read_to_string(&foreign).unwrap();
            assert!(
                !set_default(&foreign, &lookup).unwrap(),
                "a foreign default is kept"
            );
            assert_eq!(std::fs::read_to_string(&foreign).unwrap(), before);

            let dir = tmp.path().join("dangling");
            std::fs::create_dir_all(&dir).unwrap();
            let dangling = list_with(&dir, "moved.desktop");
            std::fs::set_permissions(&dangling, std::fs::Permissions::from_mode(0o644)).unwrap();
            assert!(
                set_default(&dangling, &lookup).unwrap(),
                "a dangling default is replaced"
            );
            let text = std::fs::read_to_string(&dangling).unwrap();
            assert!(text.contains(&format!("{MIME}={HANDLER}\n")), "{text}");
            assert!(text.contains("text/html=browser.desktop"));
            let mode = std::fs::metadata(&dangling).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o644, "the file keeps its mode");

            // No file yet: created, private.
            let fresh = tmp.path().join("config").join("mimeapps.list");
            assert!(set_default(&fresh, &lookup).unwrap());
            assert!(!set_default(&fresh, &lookup).unwrap());
            assert_eq!(
                std::fs::metadata(&fresh).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }

        #[test]
        fn linked_and_oversized_lists_are_left_alone() {
            let tmp = tempfile::tempdir().unwrap();
            let lookup = Lookup::default();
            let real = tmp.path().join("dotfiles-mimeapps.list");
            std::fs::write(&real, "[Default Applications]\ntext/html=browser.desktop\n").unwrap();
            let link = tmp.path().join("mimeapps.list");
            std::os::unix::fs::symlink(&real, &link).unwrap();
            assert!(set_default(&link, &lookup).is_err());
            assert!(
                std::fs::symlink_metadata(&link)
                    .unwrap()
                    .file_type()
                    .is_symlink()
            );
            assert_eq!(
                std::fs::read_to_string(&real).unwrap(),
                "[Default Applications]\ntext/html=browser.desktop\n"
            );

            let big = tmp.path().join("big").join("mimeapps.list");
            std::fs::create_dir_all(big.parent().unwrap()).unwrap();
            let text = format!(
                "[Added Associations]\n{}",
                "x".repeat(MAX_LIST as usize + 10)
            );
            std::fs::write(&big, &text).unwrap();
            let e = set_default(&big, &lookup).unwrap_err();
            assert_eq!(e.kind(), io::ErrorKind::InvalidData);
            assert_eq!(
                std::fs::read_to_string(&big).unwrap().len(),
                text.len(),
                "not truncated"
            );
        }
    }
}
