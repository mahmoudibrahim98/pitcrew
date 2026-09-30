//! Where tokens live: the [`TokenStore`] trait and a JSON-file registry.
//!
//! Stream H owns no database migrations, so the first store is a small file in the daemon's
//! state directory. It sits behind [`TokenStore`] so it can move into the hub store later.

use crate::token::{SecretToken, TokenHash, TokenId, claimed_scope};
use pitcrew_protocol::api::{Caller, TokenScope};
use pitcrew_protocol::model::TimestampMs;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::{PoisonError, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

/// What a store knows about a token, without the token itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenInfo {
    /// Handle for revoking and rotating.
    pub id: TokenId,
    /// Who the token acts as.
    pub caller: Caller,
    /// When it was minted.
    pub created_at: TimestampMs,
}

/// Failures of a token store.
#[derive(Debug, thiserror::Error)]
pub enum TokenError {
    /// An agent token needs an owner, and a device token must not have one.
    #[error("an agent token needs an owner (on_behalf_of), and a device token must not have one")]
    InvalidCaller,
    /// No token has this id.
    #[error("no token {0}")]
    NotFound(TokenId),
    /// The OS could not supply randomness.
    #[error("could not generate a token: {0}")]
    Random(getrandom::Error),
    /// The registry file could not be read or written.
    #[error("token registry {}: {source}", path.display())]
    Io {
        /// The file or directory.
        path: PathBuf,
        /// The cause.
        #[source]
        source: io::Error,
    },
    /// The registry file is not valid.
    #[error("token registry {} is malformed: {reason}", path.display())]
    Malformed {
        /// The file.
        path: PathBuf,
        /// What is wrong.
        reason: String,
    },
}

/// A place that mints, verifies, revokes and rotates tokens.
///
/// Implementations store only token hashes, and compare them in constant time.
pub trait TokenStore: Send + Sync + fmt::Debug {
    /// The caller a raw token acts as, or `None` if it is malformed, unknown or revoked.
    fn verify(&self, token: &str) -> Option<Caller>;

    /// Mints a token for `caller`. The raw token is returned once and never stored.
    ///
    /// # Errors
    /// [`TokenError::InvalidCaller`] if an agent has no owner or a device has one; storage
    /// failures.
    fn mint(&self, caller: Caller) -> Result<(TokenInfo, SecretToken), TokenError>;

    /// Revokes a token.
    ///
    /// # Errors
    /// [`TokenError::NotFound`]; storage failures.
    fn revoke(&self, id: TokenId) -> Result<(), TokenError>;

    /// Replaces a token with a new one for the same caller, in one step: the old token stops
    /// working when the new one starts.
    ///
    /// # Errors
    /// [`TokenError::NotFound`]; storage failures.
    fn rotate(&self, id: TokenId) -> Result<(TokenInfo, SecretToken), TokenError>;

    /// Every live token, oldest first.
    fn list(&self) -> Vec<TokenInfo>;
}

#[derive(Clone, Copy, Debug)]
struct Entry {
    info: TokenInfo,
    hash: TokenHash,
}

/// On disk: one record per token, holding its SHA-256, never the token.
#[derive(Serialize, Deserialize)]
struct Record {
    id: TokenId,
    sha256: String,
    caller: Caller,
    created_at: TimestampMs,
}

#[derive(Serialize, Deserialize)]
struct RegistryFile {
    version: u32,
    tokens: Vec<Record>,
}

const FILE_VERSION: u32 = 1;

/// A token registry kept in memory and, unless created with [`FileTokenStore::in_memory`],
/// written atomically to `tokens.json` (mode 0600 on Unix) on every change.
#[derive(Debug)]
pub struct FileTokenStore {
    path: Option<PathBuf>,
    entries: RwLock<Vec<Entry>>,
}

impl FileTokenStore {
    /// The registry's file name inside the state directory.
    pub const FILE_NAME: &'static str = "tokens.json";

    /// Opens (or creates) the registry in `state_dir`. The directory is created if missing, with
    /// mode 0700 on Unix.
    ///
    /// # Errors
    /// The directory cannot be created, or the file cannot be read or is malformed.
    pub fn open(state_dir: &Path) -> Result<Self, TokenError> {
        create_private_dir(state_dir).map_err(|source| TokenError::Io {
            path: state_dir.to_path_buf(),
            source,
        })?;
        let path = state_dir.join(Self::FILE_NAME);
        let entries = match fs::read(&path) {
            Ok(bytes) => parse(&path, &bytes)?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(source) => return Err(TokenError::Io { path, source }),
        };
        Ok(Self {
            path: Some(path),
            entries: RwLock::new(entries),
        })
    }

    /// A registry that is never written anywhere. For tests and ephemeral development daemons.
    #[must_use]
    pub fn in_memory() -> Self {
        Self {
            path: None,
            entries: RwLock::new(Vec::new()),
        }
    }

    /// The registry file, if any.
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Applies `change` to a copy of the entries, persists the copy, then publishes it. A failed
    /// write leaves the store unchanged.
    fn update<T>(
        &self,
        change: impl FnOnce(&mut Vec<Entry>) -> Result<T, TokenError>,
    ) -> Result<T, TokenError> {
        let mut guard = self.entries.write().unwrap_or_else(PoisonError::into_inner);
        let mut next = guard.clone();
        let out = change(&mut next)?;
        if let Some(path) = &self.path {
            persist(path, &next)?;
        }
        *guard = next;
        Ok(out)
    }
}

impl TokenStore for FileTokenStore {
    fn verify(&self, token: &str) -> Option<Caller> {
        let scope = claimed_scope(token)?;
        let hash = TokenHash::of(token);
        let entries = self.entries.read().unwrap_or_else(PoisonError::into_inner);
        // Visit every entry, without stopping at a match.
        let mut found = None;
        for entry in entries.iter() {
            if entry.hash.ct_eq(&hash) {
                found = Some(entry.info.caller);
            }
        }
        found.filter(|caller| caller.scope == scope)
    }

    fn mint(&self, caller: Caller) -> Result<(TokenInfo, SecretToken), TokenError> {
        check_caller(&caller)?;
        let (entry, token) = new_entry(caller)?;
        self.update(|entries| {
            entries.push(entry);
            Ok(())
        })?;
        tracing::info!(token = %entry.info.id, member = %caller.member, scope = ?caller.scope, "minted a token");
        Ok((entry.info, token))
    }

    fn revoke(&self, id: TokenId) -> Result<(), TokenError> {
        self.update(|entries| {
            let before = entries.len();
            entries.retain(|e| e.info.id != id);
            if entries.len() == before {
                Err(TokenError::NotFound(id))
            } else {
                Ok(())
            }
        })?;
        tracing::info!(token = %id, "revoked a token");
        Ok(())
    }

    fn rotate(&self, id: TokenId) -> Result<(TokenInfo, SecretToken), TokenError> {
        let (info, token) = self.update(|entries| {
            let index = entries
                .iter()
                .position(|e| e.info.id == id)
                .ok_or(TokenError::NotFound(id))?;
            let (entry, token) = new_entry(entries[index].info.caller)?;
            entries[index] = entry;
            Ok((entry.info, token))
        })?;
        tracing::info!(old = %id, new = %info.id, "rotated a token");
        Ok((info, token))
    }

    fn list(&self) -> Vec<TokenInfo> {
        let entries = self.entries.read().unwrap_or_else(PoisonError::into_inner);
        entries.iter().map(|e| e.info).collect()
    }
}

fn check_caller(caller: &Caller) -> Result<(), TokenError> {
    match (caller.scope, caller.on_behalf_of) {
        (TokenScope::Device, None) | (TokenScope::Agent, Some(_)) => Ok(()),
        _ => Err(TokenError::InvalidCaller),
    }
}

fn new_entry(caller: Caller) -> Result<(Entry, SecretToken), TokenError> {
    let token = SecretToken::generate(caller.scope).map_err(TokenError::Random)?;
    let entry = Entry {
        info: TokenInfo {
            id: TokenId::new(),
            caller,
            created_at: now_ms(),
        },
        hash: TokenHash::of(token.expose()),
    };
    Ok((entry, token))
}

fn now_ms() -> TimestampMs {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| {
        TimestampMs::try_from(d.as_millis()).unwrap_or(TimestampMs::MAX)
    })
}

fn parse(path: &Path, bytes: &[u8]) -> Result<Vec<Entry>, TokenError> {
    let malformed = |reason: String| TokenError::Malformed {
        path: path.to_path_buf(),
        reason,
    };
    let file: RegistryFile = serde_json::from_slice(bytes).map_err(|e| malformed(e.to_string()))?;
    if file.version != FILE_VERSION {
        return Err(malformed(format!("unsupported version {}", file.version)));
    }
    file.tokens
        .into_iter()
        .map(|r| {
            let hash = TokenHash::from_hex(&r.sha256)
                .ok_or_else(|| malformed(format!("token {} has a bad sha256", r.id)))?;
            check_caller(&r.caller)
                .map_err(|_| malformed(format!("token {} has an invalid caller", r.id)))?;
            Ok(Entry {
                info: TokenInfo {
                    id: r.id,
                    caller: r.caller,
                    created_at: r.created_at,
                },
                hash,
            })
        })
        .collect()
}

/// Writes to a temporary file in the same directory, syncs it, and renames it over the target.
fn persist(path: &Path, entries: &[Entry]) -> Result<(), TokenError> {
    let io_err = |source| TokenError::Io {
        path: path.to_path_buf(),
        source,
    };
    let file = RegistryFile {
        version: FILE_VERSION,
        tokens: entries
            .iter()
            .map(|e| Record {
                id: e.info.id,
                sha256: e.hash.to_hex(),
                caller: e.info.caller,
                created_at: e.info.created_at,
            })
            .collect(),
    };
    let mut json = serde_json::to_vec_pretty(&file).map_err(|e| io_err(io::Error::other(e)))?;
    json.push(b'\n');

    let tmp = path.with_extension("json.tmp");
    let mut out = private_file(&tmp).map_err(io_err)?;
    out.write_all(&json).map_err(io_err)?;
    out.sync_all().map_err(io_err)?;
    drop(out);
    fs::rename(&tmp, path).map_err(io_err)?;
    sync_parent(path);
    Ok(())
}

#[cfg(unix)]
fn private_file(path: &Path) -> io::Result<fs::File> {
    use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
    let file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    // `mode` only applies when the file is created.
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    Ok(file)
}

#[cfg(not(unix))]
fn private_file(path: &Path) -> io::Result<fs::File> {
    // On Windows the file inherits the ACL of the user's profile directory.
    fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
}

#[cfg(unix)]
fn sync_parent(path: &Path) {
    // Makes the rename durable. Best effort: some filesystems refuse to sync a directory.
    if let Some(Ok(dir)) = path.parent().map(fs::File::open) {
        let _ = dir.sync_all();
    }
}

#[cfg(not(unix))]
fn sync_parent(_path: &Path) {}

/// Creates `dir` (and its parents) if missing, and makes it private: mode 0700 on Unix. Fails if
/// it is not a directory or, on Unix, cannot be made private (e.g. another user owns it).
///
/// # Errors
/// The directory cannot be created or its mode set.
pub fn create_private_dir(dir: &Path) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    let meta = fs::symlink_metadata(dir)?;
    if !meta.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not a directory", dir.display()),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pitcrew_protocol::MemberId;

    fn person() -> Caller {
        Caller {
            member: MemberId::new(),
            scope: TokenScope::Device,
            on_behalf_of: None,
        }
    }

    fn agent(owner: MemberId) -> Caller {
        Caller {
            member: MemberId::new(),
            scope: TokenScope::Agent,
            on_behalf_of: Some(owner),
        }
    }

    #[test]
    fn minted_tokens_verify_to_their_caller() {
        let store = FileTokenStore::in_memory();
        let me = person();
        let bot = agent(me.member);
        let (_, device) = store.mint(me).unwrap();
        let (_, agent_token) = store.mint(bot).unwrap();
        assert_eq!(store.verify(device.expose()), Some(me));
        assert_eq!(store.verify(agent_token.expose()), Some(bot));
        assert_eq!(store.verify("pcd_unknown"), None);
        assert_eq!(store.verify(""), None);
    }

    #[test]
    fn unknown_well_formed_tokens_fail() {
        let store = FileTokenStore::in_memory();
        store.mint(person()).unwrap();
        let stranger = SecretToken::generate(TokenScope::Device).unwrap();
        assert_eq!(store.verify(stranger.expose()), None);
    }

    #[test]
    fn a_token_with_the_wrong_prefix_fails() {
        let store = FileTokenStore::in_memory();
        let (_, token) = store.mint(person()).unwrap();
        let swapped = token.expose().replacen("pcd_", "pca_", 1);
        assert_eq!(store.verify(&swapped), None);
    }

    #[test]
    fn callers_must_match_their_scope() {
        let store = FileTokenStore::in_memory();
        let mut bad_agent = agent(MemberId::new());
        bad_agent.on_behalf_of = None;
        let mut bad_device = person();
        bad_device.on_behalf_of = Some(MemberId::new());
        assert!(matches!(
            store.mint(bad_agent),
            Err(TokenError::InvalidCaller)
        ));
        assert!(matches!(
            store.mint(bad_device),
            Err(TokenError::InvalidCaller)
        ));
    }

    #[test]
    fn revoke_and_rotate() {
        let store = FileTokenStore::in_memory();
        let me = person();
        let (info, old) = store.mint(me).unwrap();
        let (new_info, new) = store.rotate(info.id).unwrap();
        assert_ne!(new_info.id, info.id);
        assert_eq!(store.verify(old.expose()), None);
        assert_eq!(store.verify(new.expose()), Some(me));

        store.revoke(new_info.id).unwrap();
        assert_eq!(store.verify(new.expose()), None);
        assert!(store.list().is_empty());
        assert!(matches!(
            store.revoke(new_info.id),
            Err(TokenError::NotFound(_))
        ));
        assert!(matches!(
            store.rotate(info.id),
            Err(TokenError::NotFound(_))
        ));
    }

    #[test]
    fn the_registry_round_trips_and_never_holds_a_raw_token() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        let me = person();
        let bot = agent(me.member);

        let (device_info, device, agent_token, revoked) = {
            let store = FileTokenStore::open(&state).unwrap();
            let (device_info, device) = store.mint(me).unwrap();
            let (_, agent_token) = store.mint(bot).unwrap();
            let (revoked_info, revoked) = store.mint(me).unwrap();
            store.revoke(revoked_info.id).unwrap();
            (device_info, device, agent_token, revoked)
        };

        let file = fs::read_to_string(state.join(FileTokenStore::FILE_NAME)).unwrap();
        for token in [&device, &agent_token, &revoked] {
            assert!(!file.contains(token.expose()));
            assert!(!file.contains(&token.expose()[4..]));
        }
        assert!(!state.join("tokens.json.tmp").exists());

        let store = FileTokenStore::open(&state).unwrap();
        assert_eq!(store.verify(device.expose()), Some(me));
        assert_eq!(store.verify(agent_token.expose()), Some(bot));
        assert_eq!(store.verify(revoked.expose()), None);
        assert_eq!(store.list().len(), 2);
        assert_eq!(store.list()[0], device_info);
    }

    #[cfg(unix)]
    #[test]
    fn the_registry_and_its_directory_are_private() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        let store = FileTokenStore::open(&state).unwrap();
        store.mint(person()).unwrap();
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&state), 0o700);
        assert_eq!(mode(&state.join(FileTokenStore::FILE_NAME)), 0o600);
    }

    #[test]
    fn a_malformed_registry_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(FileTokenStore::FILE_NAME), "{").unwrap();
        assert!(matches!(
            FileTokenStore::open(dir.path()),
            Err(TokenError::Malformed { .. })
        ));
    }

    #[test]
    fn a_failed_write_leaves_the_store_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileTokenStore::open(dir.path()).unwrap();
        // A directory where the temporary file should go makes the write fail.
        fs::create_dir(dir.path().join("tokens.json.tmp")).unwrap();
        assert!(matches!(store.mint(person()), Err(TokenError::Io { .. })));
        assert!(store.list().is_empty());
    }
}
