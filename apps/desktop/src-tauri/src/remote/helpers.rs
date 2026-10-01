//! What the app ships for remote machines: the helper binaries it deploys, and
//! `pitcrew-askpass`, which ssh runs to ask the person.
//!
//! **Helpers** are in a folder installed with the app (`helpers/` beside the executable, or in
//! the app's resources on macOS), or the one `settings.json` names for development:
//!
//! ```text
//! helpers/pitcrewd-x86_64-unknown-linux-musl     one per platform, named as
//! helpers/pitcrewd-aarch64-unknown-linux-musl    `pitcrew_remote::Platform::artefact`
//! helpers/pitcrewd-universal-apple-darwin
//! helpers/manifest.json    { "version": "0.4.0", "sha256": { "pitcrewd-x86_64-unknown-linux-musl": "…", … } }
//! ```
//!
//! A release build may carry the manifest compiled in (`PITCREW_HELPERS_MANIFEST`, its JSON, set
//! when the app is built): the sha256 is then the app's own, as ADR-0009 says, and
//! `manifest.json` is not read. Either way the bytes are checked against it before anything is
//! sent ([`Helper::new`]), and again on the machine ([`pitcrew_remote::deploy`]). On Unix the
//! manifest and the helper must belong to root or the person and be writable by no one else,
//! as for `pitcrewd` ([`crate::daemon::locate`]).

use crate::daemon::locate::{self, LocateError};
use pitcrew_remote::helper::{MAX_HELPER_SIZE, validate_version};
use pitcrew_remote::{Helper, HelperError, Platform};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::io::Read as _;
use std::path::{Path, PathBuf};

/// `pitcrew-askpass`'s file name on this platform.
pub const ASKPASS: &str = if cfg!(windows) {
    "pitcrew-askpass.exe"
} else {
    "pitcrew-askpass"
};

/// The manifest's file name in the helpers' folder.
pub const MANIFEST: &str = "manifest.json";

/// The largest manifest read.
const MAX_MANIFEST: u64 = 64 * 1024;

/// A manifest compiled into the app (release builds), as JSON.
const COMPILED: Option<&str> = option_env!("PITCREW_HELPERS_MANIFEST");

/// `manifest.json`: the helpers' version and each one's sha256.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// The version every helper reports (`pitcrewd --version`'s second word).
    pub version: String,
    /// Each helper's sha256 (lower-case hex), by its file name.
    pub sha256: BTreeMap<String, String>,
}

/// Where the helpers are.
#[derive(Clone, Debug)]
pub struct Helpers {
    dirs: Vec<PathBuf>,
    compiled: Option<&'static str>,
}

/// A helper found for a platform: not read yet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HelperRef {
    /// Its platform.
    pub platform: Platform,
    /// Its version.
    pub version: String,
    /// Its expected sha256.
    pub sha256: String,
    /// The file.
    pub path: PathBuf,
}

impl Helpers {
    /// The helpers in the first of `dirs` that exists, with the manifest compiled into the app
    /// if there is one.
    #[must_use]
    pub fn new(dirs: Vec<PathBuf>) -> Self {
        Self {
            dirs,
            compiled: COMPILED,
        }
    }

    /// The helpers in `dir`, with its `manifest.json` (development, tests).
    #[must_use]
    pub fn in_dir(dir: PathBuf) -> Self {
        Self {
            dirs: vec![dir],
            compiled: None,
        }
    }

    /// The helper for `platform`.
    ///
    /// # Errors
    /// A sentence for people: no helpers' folder, no manifest, no helper for the platform, or
    /// one that fails the checks.
    pub fn find(&self, platform: Platform) -> Result<HelperRef, String> {
        let artefact = platform.artefact();
        let Some(dir) = self.dirs.iter().find(|d| d.is_dir()) else {
            return Err(format!(
                "PitCrew has no helper for {platform}: there is no helpers folder ({}). Release \
                 builds install one with the app; for development, set \"helpers\" in \
                 settings.json to a folder holding {artefact} and {MANIFEST}",
                self.dirs
                    .iter()
                    .map(|d| d.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        };
        let manifest = match self.compiled {
            Some(text) => serde_json::from_str::<Manifest>(text)
                .map_err(|e| format!("the helper manifest built into the app is not valid: {e}"))?,
            None => read_manifest(&dir.join(MANIFEST))?,
        };
        validate_version(&manifest.version).map_err(|e| e.to_string())?;
        let sha256 = manifest.sha256.get(artefact).ok_or_else(|| {
            format!("PitCrew has no helper for {platform}: the manifest lists no {artefact}")
        })?;
        let path = dir.join(artefact);
        if !path.is_file() {
            return Err(format!(
                "PitCrew has no helper for {platform}: {} is missing",
                path.display()
            ));
        }
        locate::check_trusted(&path)
            .map_err(|why| format!("not deploying {}: {why}", path.display()))?;
        Ok(HelperRef {
            platform,
            version: manifest.version.clone(),
            sha256: sha256.clone(),
            path,
        })
    }

    /// The helper's bytes, checked against its sha256.
    ///
    /// # Errors
    /// The file cannot be read, is too large, or does not hash to the manifest's sha256.
    pub fn load(found: &HelperRef) -> Result<Helper, HelperError> {
        let file = std::fs::File::open(&found.path).map_err(|e| {
            HelperError::InvalidArgument(format!("cannot read {}: {e}", found.path.display()))
        })?;
        let mut bytes = Vec::new();
        file.take(u64::try_from(MAX_HELPER_SIZE).unwrap_or(u64::MAX) + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| {
                HelperError::InvalidArgument(format!("cannot read {}: {e}", found.path.display()))
            })?;
        Helper::new(found.platform, &found.version, &found.sha256, bytes)
    }
}

/// Reads `manifest.json`, after the same checks as a helper.
fn read_manifest(path: &Path) -> Result<Manifest, String> {
    if !path.is_file() {
        return Err(format!(
            "the helpers' manifest {} is missing",
            path.display()
        ));
    }
    locate::check_trusted(path).map_err(|why| format!("not reading {}: {why}", path.display()))?;
    let mut text = String::new();
    std::fs::File::open(path)
        .and_then(|f| f.take(MAX_MANIFEST + 1).read_to_string(&mut text))
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    if u64::try_from(text.len()).unwrap_or(u64::MAX) > MAX_MANIFEST {
        return Err(format!("{} is too large", path.display()));
    }
    serde_json::from_str(&text).map_err(|e| format!("{} is not valid: {e}", path.display()))
}

/// Finds `pitcrew-askpass`: the configured path, else next to the app. `PATH` is never searched.
///
/// # Errors
/// A sentence for people: missing, not a program, or untrusted.
pub fn locate_askpass(configured: Option<&Path>, beside: Option<&Path>) -> Result<PathBuf, String> {
    locate::locate_named(ASKPASS, configured, beside, None).map_err(|e| match e {
        LocateError::NotFound => format!(
            "{ASKPASS} is not next to the app{}: PitCrew needs it to ask for SSH passwords, \
             passphrases and codes. Install it there, or set \"askpass\" in settings.json",
            beside
                .map(|d| format!(" ({})", d.display()))
                .unwrap_or_default()
        ),
        LocateError::NotAProgram(path) => {
            format!(
                "the configured askpass, {}, is not a program",
                path.display()
            )
        }
        other => other.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest as _, Sha256};

    fn hex_sha256(bytes: &[u8]) -> String {
        Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    fn private(dir: &Path) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let _ = dir;
    }

    #[test]
    fn helpers_are_found_by_platform_and_checked() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("helpers");
        std::fs::create_dir(&dir).unwrap();
        private(&dir);
        let helpers = Helpers::in_dir(dir.clone());
        let missing = helpers.find(Platform::LinuxX86_64).unwrap_err();
        assert!(missing.contains("manifest"), "{missing}");

        let bytes = b"#!/bin/sh\necho pitcrewd 1.2.3\n";
        let artefact = Platform::LinuxX86_64.artefact();
        std::fs::write(dir.join(artefact), bytes).unwrap();
        let manifest =
            serde_json::json!({ "version": "1.2.3", "sha256": { artefact: hex_sha256(bytes) } });
        std::fs::write(dir.join(MANIFEST), manifest.to_string()).unwrap();
        let found = helpers.find(Platform::LinuxX86_64).unwrap();
        assert_eq!(found.version, "1.2.3");
        assert_eq!(found.path, dir.join(artefact));
        let helper = Helpers::load(&found).unwrap();
        assert_eq!(helper.bytes(), bytes);

        // No helper for another platform: a clear error naming it.
        let e = helpers.find(Platform::LinuxAarch64).unwrap_err();
        assert!(e.contains("Linux on aarch64"), "{e}");
        // A file that does not hash to the manifest's sha256 is refused before anything is sent.
        std::fs::write(dir.join(artefact), b"something else").unwrap();
        let found = helpers.find(Platform::LinuxX86_64).unwrap();
        assert!(matches!(
            Helpers::load(&found),
            Err(HelperError::LocalHashMismatch)
        ));
        // A folder that is not there.
        let e = Helpers::in_dir(tmp.path().join("nowhere"))
            .find(Platform::LinuxX86_64)
            .unwrap_err();
        assert!(e.contains("no helpers folder"), "{e}");
    }

    #[cfg(unix)]
    #[test]
    fn a_helper_others_can_write_is_refused() {
        use std::os::unix::fs::PermissionsExt as _;
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("helpers");
        std::fs::create_dir(&dir).unwrap();
        private(&dir);
        let artefact = Platform::LinuxX86_64.artefact();
        std::fs::write(dir.join(artefact), b"x").unwrap();
        let manifest =
            serde_json::json!({ "version": "1.2.3", "sha256": { artefact: hex_sha256(b"x") } });
        std::fs::write(dir.join(MANIFEST), manifest.to_string()).unwrap();
        std::fs::set_permissions(dir.join(artefact), std::fs::Permissions::from_mode(0o666))
            .unwrap();
        let e = Helpers::in_dir(dir)
            .find(Platform::LinuxX86_64)
            .unwrap_err();
        assert!(e.contains("written by other users"), "{e}");
    }

    #[test]
    fn askpass_is_looked_for_next_to_the_app_only() {
        let tmp = tempfile::tempdir().unwrap();
        let e = locate_askpass(None, Some(tmp.path())).unwrap_err();
        assert!(e.contains("is not next to the app"), "{e}");
        assert!(e.contains("askpass"), "{e}");
        let e = locate_askpass(Some(&tmp.path().join("missing")), None).unwrap_err();
        assert!(e.contains("is not a program"), "{e}");
    }
}
