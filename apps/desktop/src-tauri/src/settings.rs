//! The app's settings: where `pitcrewd` is, which state directory it uses, and for remote
//! machines which `ssh`, `pitcrew-askpass` and helper binaries to use.
//!
//! Read from `settings.json` in the app's config directory, if there is one:
//!
//! ```json
//! { "pitcrewd": "/opt/pitcrew/bin/pitcrewd", "stateDir": "/home/sam/.local/share/pitcrew",
//!   "ssh": "/usr/bin/ssh", "askpass": "/opt/pitcrew/bin/pitcrew-askpass",
//!   "helpers": "/opt/pitcrew/helpers" }
//! ```
//!
//! `PITCREW_PITCREWD`, `PITCREW_STATE_DIR`, `PITCREW_SSH`, `PITCREW_ASKPASS` and
//! `PITCREW_HELPERS` override them (development and tests). Without a state directory the
//! daemon's own default is used, so the app and a `pitcrewd` started by hand find the same
//! socket. Without the others the app uses `ssh` from `PATH`, and the `pitcrew-askpass` and
//! `helpers/` it was installed with.

use serde::Deserialize;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The file's name in the app's config directory.
pub const FILE_NAME: &str = "settings.json";

/// The settings.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Settings {
    /// The `pitcrewd` to run, instead of looking next to the app and on `PATH`.
    #[serde(default)]
    pub pitcrewd: Option<PathBuf>,
    /// The daemon's state directory, passed as `--state-dir`.
    #[serde(default)]
    pub state_dir: Option<PathBuf>,
    /// The OpenSSH client for remote machines, instead of `ssh` on `PATH`.
    #[serde(default)]
    pub ssh: Option<PathBuf>,
    /// The `pitcrew-askpass` program, instead of the one next to the app.
    #[serde(default)]
    pub askpass: Option<PathBuf>,
    /// The folder of helper binaries (`pitcrewd-<target>` and `manifest.json`) deployed to
    /// remote machines, instead of the one installed with the app (development).
    #[serde(default)]
    pub helpers: Option<PathBuf>,
}

impl Settings {
    /// Reads `settings.json` in `config_dir`. A missing file is the defaults; a file that cannot be
    /// read is logged and ignored.
    #[must_use]
    pub fn load(config_dir: &Path) -> Self {
        let path = config_dir.join(FILE_NAME);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Self::default(),
            Err(e) => {
                tracing::warn!(file = %path.display(), error = %e, "cannot read the settings; using the defaults");
                return Self::default();
            }
        };
        match serde_json::from_str::<Self>(&text) {
            Ok(settings) => settings.checked(),
            Err(e) => {
                tracing::warn!(file = %path.display(), error = %e, "the settings are not valid; using the defaults");
                Self::default()
            }
        }
    }

    /// Applies `PITCREW_PITCREWD`, `PITCREW_STATE_DIR`, `PITCREW_SSH`, `PITCREW_ASKPASS` and
    /// `PITCREW_HELPERS` from `env`, when set and not empty.
    #[must_use]
    pub fn with_env(mut self, env: impl Fn(&str) -> Option<OsString>) -> Self {
        let var = |name: &str| env(name).filter(|v| !v.is_empty()).map(PathBuf::from);
        for (name, setting) in [
            ("PITCREW_PITCREWD", &mut self.pitcrewd),
            ("PITCREW_STATE_DIR", &mut self.state_dir),
            ("PITCREW_SSH", &mut self.ssh),
            ("PITCREW_ASKPASS", &mut self.askpass),
            ("PITCREW_HELPERS", &mut self.helpers),
        ] {
            if let Some(path) = var(name) {
                *setting = Some(path);
            }
        }
        self.checked()
    }

    /// Drops relative paths: they would depend on the directory the app was started from.
    fn checked(mut self) -> Self {
        for (name, path) in [
            ("pitcrewd", &mut self.pitcrewd),
            ("stateDir", &mut self.state_dir),
            ("ssh", &mut self.ssh),
            ("askpass", &mut self.askpass),
            ("helpers", &mut self.helpers),
        ] {
            if path.as_ref().is_some_and(|p| !p.is_absolute()) {
                tracing::warn!(
                    setting = name,
                    "ignoring a relative path; use an absolute one"
                );
                *path = None;
            }
        }
        self
    }

    /// The state directory to use: the configured one, or the daemon's default.
    #[must_use]
    pub fn state_dir_or_default(&self) -> Option<PathBuf> {
        self.state_dir.clone().or_else(default_state_dir)
    }
}

/// The daemon's default state directory (`crates/daemon/README.md`): the platform's local data
/// folder, never a roaming one. `%LOCALAPPDATA%\PitCrew\data`, `~/.local/share/pitcrew` (or
/// `$XDG_DATA_HOME/pitcrew`), `~/Library/Application Support/PitCrew`.
#[must_use]
pub fn default_state_dir() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "PitCrew")
        .map(|dirs| dirs.data_local_dir().to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn abs(p: &str) -> PathBuf {
        if cfg!(windows) {
            PathBuf::from(format!(
                "C:\\{}",
                p.trim_start_matches('/').replace('/', "\\")
            ))
        } else {
            PathBuf::from(p)
        }
    }

    #[test]
    fn the_file_and_the_environment() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(Settings::load(tmp.path()), Settings::default());

        let file = serde_json::json!({
            "pitcrewd": abs("/opt/pitcrew/pitcrewd"),
            "stateDir": abs("/srv/state"),
            "ssh": abs("/usr/bin/ssh"),
            "askpass": abs("/opt/pitcrew/pitcrew-askpass"),
            "helpers": abs("/opt/pitcrew/helpers"),
        });
        std::fs::write(tmp.path().join(FILE_NAME), file.to_string()).unwrap();
        let settings = Settings::load(tmp.path());
        assert_eq!(settings.pitcrewd, Some(abs("/opt/pitcrew/pitcrewd")));
        assert_eq!(settings.state_dir, Some(abs("/srv/state")));
        assert_eq!(settings.ssh, Some(abs("/usr/bin/ssh")));
        assert_eq!(settings.askpass, Some(abs("/opt/pitcrew/pitcrew-askpass")));
        assert_eq!(settings.helpers, Some(abs("/opt/pitcrew/helpers")));

        let env_dir = abs("/home/sam/state");
        let env_dir_os = env_dir.clone().into_os_string();
        let helpers = abs("/home/sam/helpers");
        let helpers_os = helpers.clone().into_os_string();
        let settings = settings.with_env(|name| match name {
            "PITCREW_STATE_DIR" => Some(env_dir_os.clone()),
            "PITCREW_HELPERS" => Some(helpers_os.clone()),
            _ => None,
        });
        assert_eq!(settings.state_dir, Some(env_dir));
        assert_eq!(settings.helpers, Some(helpers));
        assert_eq!(settings.pitcrewd, Some(abs("/opt/pitcrew/pitcrewd")));
    }

    #[test]
    fn bad_files_and_relative_paths_are_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join(FILE_NAME), r#"{ "unknown": 1 }"#).unwrap();
        assert_eq!(Settings::load(tmp.path()), Settings::default());
        std::fs::write(
            tmp.path().join(FILE_NAME),
            r#"{ "stateDir": "relative/dir", "askpass": "pitcrew-askpass" }"#,
        )
        .unwrap();
        let settings = Settings::load(tmp.path());
        assert_eq!(settings.state_dir, None);
        assert_eq!(settings.askpass, None);
        let settings = Settings::default().with_env(|_| Some(OsString::from("bin/pitcrewd")));
        assert_eq!(settings, Settings::default());
    }

    #[test]
    fn the_default_state_dir_matches_the_daemons() {
        if let Some(dir) = default_state_dir() {
            assert!(dir.is_absolute());
            let text = dir.to_string_lossy().to_lowercase();
            assert!(text.contains("pitcrew"), "{text}");
        }
    }
}
