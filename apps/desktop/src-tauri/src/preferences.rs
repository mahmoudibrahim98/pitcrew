//! The person's choices in the app: whether it notifies, and whether closing the window quits.
//!
//! Kept in `preferences.json` in the app's config directory, next to `settings.json` (which the
//! person edits by hand, and which the app never writes). Changed from the tray menu, written
//! atomically with private permissions. A missing or unreadable file is the defaults.
//!
//! ```json
//! { "notifications": true, "quitOnClose": false, "trayHintShown": true }
//! ```

use crate::registry::write_private;
use serde::{Deserialize, Serialize};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

/// The file's name in the app's config directory.
pub const FILE_NAME: &str = "preferences.json";
/// The longest file read.
const MAX_FILE: u64 = 64 * 1024;

/// The preferences.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Preferences {
    /// Show a notification when an agent needs the person and the window is not focused.
    pub notifications: bool,
    /// Closing the window quits the app, instead of keeping it in the tray.
    pub quit_on_close: bool,
    /// The person was told, once, that closing the window keeps the app in the tray.
    pub tray_hint_shown: bool,
    /// Include signed pre-releases in update checks (off by default).
    pub update_prereleases: bool,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            notifications: true,
            quit_on_close: false,
            tray_hint_shown: false,
            update_prereleases: false,
        }
    }
}

/// The preferences, and where they are saved.
#[derive(Debug)]
pub struct PreferenceStore {
    file: Option<PathBuf>,
    current: Mutex<Preferences>,
}

impl PreferenceStore {
    /// Preferences kept only in memory (tests).
    #[must_use]
    pub fn in_memory(preferences: Preferences) -> Self {
        Self {
            file: None,
            current: Mutex::new(preferences),
        }
    }

    /// Reads `preferences.json` in `config_dir`; the defaults if it is missing or unreadable.
    #[must_use]
    pub fn load(config_dir: &Path) -> Self {
        let file = config_dir.join(FILE_NAME);
        let current = match read(&file) {
            Ok(Some(preferences)) => preferences,
            Ok(None) => Preferences::default(),
            Err(e) => {
                tracing::warn!(file = %file.display(), error = %e, "cannot read the preferences; using the defaults");
                Preferences::default()
            }
        };
        Self {
            file: Some(file),
            current: Mutex::new(current),
        }
    }

    /// The current preferences.
    #[must_use]
    pub fn get(&self) -> Preferences {
        *self.lock()
    }

    /// Changes the preferences with `change` and saves them. Returns the new preferences.
    ///
    /// # Errors
    /// The file cannot be written; the change holds in memory anyway.
    pub fn update(&self, change: impl FnOnce(&mut Preferences)) -> io::Result<Preferences> {
        let mut current = self.lock();
        change(&mut current);
        let updated = *current;
        let saved = match &self.file {
            Some(file) => serde_json::to_vec_pretty(&updated)
                .map_err(io::Error::other)
                .and_then(|json| write_private(file, &json))
                .inspect_err(|e| {
                    tracing::warn!(file = %file.display(), error = %e, "cannot save the preferences");
                }),
            None => Ok(()),
        };
        drop(current);
        saved.map(|()| updated)
    }

    fn lock(&self) -> MutexGuard<'_, Preferences> {
        self.current
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

fn read(file: &Path) -> io::Result<Option<Preferences>> {
    use std::io::Read as _;
    let mut text = Vec::new();
    match std::fs::File::open(file) {
        Ok(f) => {
            f.take(MAX_FILE).read_to_end(&mut text)?;
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    }
    serde_json::from_slice(&text)
        .map(Some)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_load_and_save() {
        let tmp = tempfile::tempdir().unwrap();
        let store = PreferenceStore::load(tmp.path());
        assert_eq!(store.get(), Preferences::default());
        assert!(store.get().notifications && !store.get().quit_on_close);

        let updated = store
            .update(|p| {
                p.notifications = false;
                p.tray_hint_shown = true;
                p.update_prereleases = true;
            })
            .unwrap();
        assert!(!updated.notifications);
        let text = std::fs::read_to_string(tmp.path().join(FILE_NAME)).unwrap();
        let saved: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            saved,
            serde_json::json!({ "notifications": false, "quitOnClose": false, "trayHintShown": true, "updatePrereleases": true })
        );
        assert_eq!(PreferenceStore::load(tmp.path()).get(), updated);
    }

    #[test]
    fn partial_and_bad_files() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join(FILE_NAME);
        std::fs::write(&file, r#"{ "quitOnClose": true, "later": 1 }"#).unwrap();
        let p = PreferenceStore::load(tmp.path()).get();
        assert!(p.quit_on_close && p.notifications && !p.tray_hint_shown && !p.update_prereleases);
        std::fs::write(&file, "not json").unwrap();
        assert_eq!(
            PreferenceStore::load(tmp.path()).get(),
            Preferences::default()
        );
    }
}
