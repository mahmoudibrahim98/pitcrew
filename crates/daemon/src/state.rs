//! The state directory: where the daemon keeps everything, and the private token files in it.
//!
//! | Path | What |
//! |---|---|
//! | `hub.db` | The hub's store (SQLite, WAL). |
//! | `tokens.json`, `tokens.lock` | The token registry (hashes only), owned by `pitcrew-auth`. While a daemon runs it holds `tokens.lock`, so a second daemon on the same directory is refused. |
//! | `device.token` | The desktop's device token, for the UI in development. Private (0600). |
//! | `demo-agent.token` | With `--demo`: a token for the demo's first agent. Private (0600). |
//! | `workspace.json` | The workspace's id and name, which the event log does not hold. Private (0600). |
//! | `office.json` | Where the back office got to in the log, so a restart runs it again from there. Private (0600). |
//! | `recaps.sqlite3` | The recap index's blocks (`WorkService::with_recap_file`): a cache, made when the index is first built, replaced at every start and removed when the daemon stops; never read from one run to the next. Private (0600). When this directory is on a network filesystem, it is in a private folder (0700) on a local disk instead, in the temporary folder or else `$XDG_RUNTIME_DIR`, or the blocks stay in memory. On Unix that folder is named after this directory and the user, so the next start reuses the one a hard kill left and replaces its file; on Windows (or when something else has that name) its name is random, and a hard kill leaves it until the temporary folder is cleaned. |
//! | `runner/<log id>/` | The runner's index of the transcripts it watches (`pitcrew-runner`), one per hub log. |
//! | `agents/<agent id>.token` | An agent token for each agent whose sessions the runner started (a dispatch's), bound to that agent and its owner; the CLI is given its path (`PITCREW_TOKEN_FILE`). The folder is private (0700), each file too (0600). |
//! | `orchestrator.json` | The Orchestrator's conversations, per person, and the ids of every Orchestrator session started for them (`WorkService::with_orchestrator_file`): not in the event log, so a person can clear their conversations. Private (0600). One this hub cannot read is moved aside (`orchestrator.json.unreadable-<ms>`) and the hub starts with none. |
//! | `integrations.json` | The GitHub and Jira connections (no secret), each with its sync member and last sync status (`crate::integrations`). Private (0600). |
//! | `integrations/<id>.state.json`, `integrations/<id>.secret` | A connection's sync state (cursors, `ETag`s, snapshots of what it read upstream), and its stored secret when it has one. The folder is private (0700; an owner-only DACL on Windows), each file too (0600). Never in the event log. |
//! | `run/pitcrewd.sock` | The private socket (Unix). |

use anyhow::Context as _;
use pitcrew_auth::SecretToken;
use pitcrew_protocol::model::Workspace;
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::fs;
use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};

/// The longest token file read, in bytes. Tokens are about 50.
const MAX_TOKEN_FILE: u64 = 4096;
/// The longest JSON state file read (`workspace.json`, `office.json`), in bytes.
const MAX_JSON_FILE: u64 = 64 * 1024;

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

    /// The workspace's id and name.
    #[must_use]
    pub fn workspace(&self) -> PathBuf {
        self.root.join("workspace.json")
    }

    /// Where the back office got to in the log.
    #[must_use]
    pub fn office(&self) -> PathBuf {
        self.root.join("office.json")
    }

    /// The private socket's directory (Unix).
    #[must_use]
    pub fn run_dir(&self) -> PathBuf {
        self.root.join("run")
    }

    /// The runner's indexes, one folder per hub log.
    #[must_use]
    pub fn runner(&self) -> PathBuf {
        self.root.join("runner")
    }

    /// The agents' token files, for the CLIs the runner starts as them.
    #[must_use]
    pub fn agents(&self) -> PathBuf {
        self.root.join("agents")
    }

    /// The Orchestrator's conversations.
    #[must_use]
    pub fn orchestrator(&self) -> PathBuf {
        self.root.join("orchestrator.json")
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
    write_private(path, token.expose().as_bytes())
}

/// Writes the workspace's id and name to `path`, as [`write_token`] writes a token.
///
/// # Errors
/// Creating, writing or renaming fails.
pub fn write_workspace(path: &Path, workspace: &Workspace) -> io::Result<()> {
    write_json(path, workspace)
}

/// Reads a workspace file written by [`write_workspace`]: `Ok(None)` if there is none.
///
/// # Errors
/// It is not a regular file, cannot be read, or does not hold a workspace.
pub fn read_workspace(path: &Path) -> io::Result<Option<Workspace>> {
    read_json(path, "a workspace")
}

/// Writes `value` as JSON to `path`, as [`write_token`] writes a token.
///
/// # Errors
/// Creating, writing or renaming fails.
pub fn write_json<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    let json = serde_json::to_string_pretty(value).map_err(io::Error::other)?;
    write_private(path, json.as_bytes())
}

/// Reads a file written by [`write_json`]: `Ok(None)` if there is none. `what` names its content
/// in the error.
///
/// # Errors
/// It is not a regular file, cannot be read, or does not hold `what`.
pub fn read_json<T: DeserializeOwned>(path: &Path, what: &str) -> io::Result<Option<T>> {
    read_json_up_to(path, what, MAX_JSON_FILE)
}

/// [`read_json`] for a file of at most `max` bytes; a longer one does not parse.
///
/// # Errors
/// As [`read_json`].
pub fn read_json_up_to<T: DeserializeOwned>(
    path: &Path,
    what: &str,
    max: u64,
) -> io::Result<Option<T>> {
    let Some(text) = read_regular(path, max)? else {
        return Ok(None);
    };
    serde_json::from_str(&text).map(Some).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} does not hold {what}: {e}", path.display()),
        )
    })
}

/// Removes the file at `path`; one that is not there is fine.
///
/// # Errors
/// Removing it fails.
pub fn remove(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// Writes `bytes` and a newline to a new private file next to `path`, then renames it over `path`.
fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
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
        file.write_all(bytes)?;
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

/// The text of the regular file at `path`, at most `max` bytes: `Ok(None)` if there is none.
fn read_regular(path: &Path, max: u64) -> io::Result<Option<String>> {
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
    let mut text = String::new();
    fs::File::open(path)?.take(max).read_to_string(&mut text)?;
    Ok(Some(text))
}

/// Reads a token file written by [`write_token`]: `Ok(None)` if there is none. On Unix it must be
/// a regular file of ours that nobody else can read or write.
///
/// The text is a secret: never log it.
///
/// # Errors
/// It is not a private regular file, or cannot be read.
pub fn read_token(path: &Path) -> io::Result<Option<String>> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        // `read_regular` refuses anything but a regular file.
        if let Ok(meta) = fs::symlink_metadata(path)
            && meta.is_file()
            && (meta.uid() != pitcrew_auth::euid() || meta.mode() & 0o077 != 0)
        {
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
    Ok(read_regular(path, MAX_TOKEN_FILE)?.map(|text| text.trim().to_owned()))
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
    fn the_workspace_round_trips_and_a_bad_file_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("workspace.json");
        assert_eq!(read_workspace(&path).unwrap(), None);
        let workspace = Workspace {
            id: pitcrew_protocol::ids::WorkspaceId::new(),
            name: "Thesis".to_owned(),
        };
        write_workspace(&path, &workspace).unwrap();
        assert_eq!(read_workspace(&path).unwrap(), Some(workspace));
        fs::write(&path, "{\"id\":").unwrap();
        assert_eq!(
            read_workspace(&path).unwrap_err().kind(),
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
            state.workspace(),
            state.office(),
            state.run_dir(),
            state.runner(),
        ] {
            assert_eq!(path.parent(), Some(state.root()));
        }
    }
}
