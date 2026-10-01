//! The state directory: where the daemon keeps everything, and the private token files in it.
//!
//! | Path | What |
//! |---|---|
//! | `hub.db` | The hub's store (SQLite, WAL). |
//! | `tokens.json`, `tokens.lock` | The token registry (hashes only), owned by `pitcrew-auth`. While a daemon runs it holds `tokens.lock`, so a second daemon on the same directory is refused. |
//! | `device.token` | The desktop's device token, for the UI in development. Private (0600). |
//! | `demo-agent.token` | With `--demo`: a token for the demo's first agent. Private (0600). |
//! | `run/pitcrewd.sock` | The private socket (Unix). |

use anyhow::Context as _;
use pitcrew_auth::SecretToken;
use std::fs;
use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};

/// The longest token file read, in bytes. Tokens are about 50.
const MAX_TOKEN_FILE: u64 = 4096;

/// A daemon's state directory and the paths in it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StateDir {
    root: PathBuf,
}

impl StateDir {
    /// `dir` made absolute, or the default.
    ///
    /// # Errors
    /// The current directory or the user's data folder cannot be found.
    pub fn resolve(dir: Option<PathBuf>) -> anyhow::Result<Self> {
        let root = match dir {
            Some(dir) => std::path::absolute(&dir)
                .with_context(|| format!("cannot resolve the state directory {}", dir.display()))?,
            None => default_dir()?,
        };
        Ok(Self { root })
    }

    /// The directory itself.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The hub's store.
    #[must_use]
    pub fn store(&self) -> PathBuf {
        self.root.join("hub.db")
    }

    /// The desktop's device token.
    #[must_use]
    pub fn device_token(&self) -> PathBuf {
        self.root.join("device.token")
    }

    /// The demo agent's token (`--demo` only).
    #[must_use]
    pub fn demo_agent_token(&self) -> PathBuf {
        self.root.join("demo-agent.token")
    }

    /// The private socket's directory (Unix).
    #[must_use]
    pub fn run_dir(&self) -> PathBuf {
        self.root.join("run")
    }
}

/// The platform's local data folder for PitCrew: never a roaming or synced one.
fn default_dir() -> anyhow::Result<PathBuf> {
    directories::ProjectDirs::from("", "", "PitCrew")
        .map(|dirs| dirs.data_local_dir().to_path_buf())
        .context("cannot find this user's local data folder; pass --state-dir")
}

/// Writes `token` to `path` so that only this user can read it: a new private file, renamed over
/// any old one. The directory must be private already (the token registry makes it so).
///
/// # Errors
/// Creating, writing or renaming fails.
pub fn write_token(path: &Path, token: &SecretToken) -> io::Result<()> {
    let tmp = path.with_extension("token.tmp");
    // Only this daemon writes here (it holds the state directory's lock), so a leftover is stale.
    let _ = fs::remove_file(&tmp);
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let written = options.open(&tmp).and_then(|mut file| {
        file.write_all(token.expose().as_bytes())?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        drop(file);
        fs::rename(&tmp, path)
    });
    if written.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    written
}

/// Reads a token file written by [`write_token`]: `Ok(None)` if there is none. On Unix it must be
/// a regular file of ours that nobody else can read or write.
///
/// The text is a secret: never log it.
///
/// # Errors
/// It is not a private regular file, or cannot be read.
pub fn read_token(path: &Path) -> io::Result<Option<String>> {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    if !meta.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} is not a regular file", path.display()),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if meta.uid() != pitcrew_auth::euid() || meta.mode() & 0o077 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "{} must be ours and private (mode 600), but has mode {:o}",
                    path.display(),
                    meta.mode() & 0o777
                ),
            ));
        }
    }
    let mut text = String::new();
    fs::File::open(path)?
        .take(MAX_TOKEN_FILE)
        .read_to_string(&mut text)?;
    Ok(Some(text.trim().to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pitcrew_auth::{FileTokenStore, TokenStore as _};
    use pitcrew_protocol::MemberId;
    use pitcrew_protocol::api::{Caller, TokenScope};

    fn minted() -> SecretToken {
        let tokens = FileTokenStore::in_memory();
        let caller = Caller {
            member: MemberId::new(),
            scope: TokenScope::Device,
            on_behalf_of: None,
        };
        tokens.mint(caller).unwrap().1
    }

    #[test]
    fn a_token_round_trips_and_replaces_the_old_one() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("device.token");
        assert_eq!(read_token(&path).unwrap(), None);

        let first = minted();
        write_token(&path, &first).unwrap();
        assert_eq!(read_token(&path).unwrap().as_deref(), Some(first.expose()));

        let second = minted();
        write_token(&path, &second).unwrap();
        assert_eq!(read_token(&path).unwrap().as_deref(), Some(second.expose()));
        let names: Vec<_> = fs::read_dir(tmp.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, vec!["device.token"]);
    }

    #[cfg(unix)]
    #[test]
    fn token_files_are_private() {
        use std::os::unix::fs::PermissionsExt as _;
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("device.token");
        write_token(&path, &minted()).unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);

        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        let err = read_token(&path).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);

        let link = tmp.path().join("link.token");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert_eq!(
            read_token(&link).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn paths_live_in_the_state_dir() {
        let state = StateDir::resolve(Some(PathBuf::from("some/state"))).unwrap();
        assert!(state.root().is_absolute());
        assert!(state.root().ends_with("some/state"));
        for path in [
            state.store(),
            state.device_token(),
            state.demo_agent_token(),
            state.run_dir(),
        ] {
            assert_eq!(path.parent(), Some(state.root()));
        }
    }
}
