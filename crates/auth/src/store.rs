//! Where tokens live: the [`TokenStore`] trait and a JSON-file registry.
//!
//! Stream H owns no database migrations, so the first store is a small file in the daemon's
//! state directory. It sits behind [`TokenStore`] so it can move into the hub store later.

use crate::private::{
    ExclusiveLock, create_new_private_file, create_private_dir, open_private_file,
};
use crate::token::{SecretToken, TokenHash, TokenId, claimed_prefix, prefix};
use pitcrew_protocol::api::{Caller, TokenScope};
use pitcrew_protocol::model::TimestampMs;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fmt;
use std::fs;
use std::io::{self, Read as _, Write as _};
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
    /// An agent, reader or session token needs an owner, and a device token must not have one.
    #[error(
        "an agent, reader or session token needs an owner (on_behalf_of), and a device token \
         must not have one"
    )]
    InvalidCaller,
    /// No token has this id.
    #[error("no token {0}")]
    NotFound(TokenId),
    /// The OS could not supply randomness.
    #[error("could not generate a token: {0}")]
    Random(String),
    /// Another process (or another store in this one) already has the registry open.
    #[error("token registry {} is in use by another process", path.display())]
    Locked {
        /// The lock file.
        path: PathBuf,
    },
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
///
/// **Single writer.** Only the daemon opens the registry; everything else (the CLI, the desktop)
/// mints and revokes through the API. [`FileTokenStore::open`] holds an exclusive lock on
/// `tokens.lock` for the store's lifetime, so a second opener fails with
/// [`TokenError::Locked`].
///
/// **Windows:** files take the ACL of their directory, so `state_dir` must be under the user's
/// profile (e.g. `%LOCALAPPDATA%`).
#[derive(Debug)]
pub struct FileTokenStore {
    path: Option<PathBuf>,
    entries: RwLock<Vec<Entry>>,
    _lock: Option<ExclusiveLock>,
}

impl FileTokenStore {
    /// The registry's file name inside the state directory.
    pub const FILE_NAME: &'static str = "tokens.json";
    /// The lock file's name inside the state directory.
    pub const LOCK_NAME: &'static str = "tokens.lock";

    /// Opens (or creates) the registry in `state_dir` and locks it.
    ///
    /// On Unix, `state_dir` is created with mode 0700 if missing; an existing one must already be
    /// owned by us with no group or other access. The registry file must be ours, not a symlink,
    /// and not writable by others. Anything else fails closed.
    ///
    /// # Errors
    /// The directory or file is not private, the registry is locked or malformed, or I/O fails.
    pub fn open(state_dir: &Path) -> Result<Self, TokenError> {
        let io_err = |path: &Path| {
            let path = path.to_path_buf();
            move |source| TokenError::Io { path, source }
        };
        create_private_dir(state_dir).map_err(io_err(state_dir))?;
        let lock_path = state_dir.join(Self::LOCK_NAME);
        let lock = ExclusiveLock::acquire(&lock_path).map_err(|e| {
            if e.kind() == io::ErrorKind::WouldBlock {
                TokenError::Locked {
                    path: lock_path.clone(),
                }
            } else {
                io_err(&lock_path)(e)
            }
        })?;
        let path = state_dir.join(Self::FILE_NAME);
        let entries = match open_private_file(&path).map_err(io_err(&path))? {
            Some(mut file) => {
                let mut bytes = Vec::new();
                file.read_to_end(&mut bytes).map_err(io_err(&path))?;
                parse(&path, &bytes)?
            }
            None => Vec::new(),
        };
        Ok(Self {
            path: Some(path),
            entries: RwLock::new(entries),
            _lock: Some(lock),
        })
    }

    /// A registry that is never written anywhere. For tests and ephemeral development daemons.
    #[must_use]
    pub fn in_memory() -> Self {
        Self {
            path: None,
            entries: RwLock::new(Vec::new()),
            _lock: None,
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
        let claimed = claimed_prefix(token)?;
        let hash = TokenHash::of(token);
        let entries = self.entries.read().unwrap_or_else(PoisonError::into_inner);
        // Visit every entry, without stopping at a match.
        let mut found = None;
        for entry in entries.iter() {
            if entry.hash.ct_eq(&hash) {
                found = Some(entry.info.caller);
            }
        }
        found.filter(|caller| prefix(caller.scope) == claimed)
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
        (TokenScope::Device, None)
        | (TokenScope::Agent | TokenScope::Reader | TokenScope::Session(_), Some(_)) => Ok(()),
        _ => Err(TokenError::InvalidCaller),
    }
}

fn new_entry(caller: Caller) -> Result<(Entry, SecretToken), TokenError> {
    let token =
        SecretToken::generate(caller.scope).map_err(|e| TokenError::Random(e.to_string()))?;
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
    let mut ids = HashSet::new();
    let mut hashes = HashSet::new();
    file.tokens
        .into_iter()
        .map(|r| {
            let hash = TokenHash::from_hex(&r.sha256)
                .ok_or_else(|| malformed(format!("token {} has a bad sha256", r.id)))?;
            check_caller(&r.caller)
                .map_err(|_| malformed(format!("token {} has an invalid caller", r.id)))?;
            if !ids.insert(r.id) {
                return Err(malformed(format!("token {} appears twice", r.id)));
            }
            if !hashes.insert(hash.0) {
                return Err(malformed(format!("token {} repeats another's hash", r.id)));
            }
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

/// Writes to a new, uniquely named temporary file in the same directory, syncs it, and renames
/// it over the target.
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

    let tmp = path.with_extension(format!("json.{}.tmp", ulid::Ulid::generate()));
    let written = create_new_private_file(&tmp).and_then(|mut out| {
        out.write_all(&json)?;
        out.sync_all()?;
        drop(out);
        fs::rename(&tmp, path)
    });
    if let Err(e) = written {
        let _ = fs::remove_file(&tmp);
        return Err(io_err(e));
    }
    sync_parent(path);
    Ok(())
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
#[cfg(test)]
mod tests {
    use super::*;
    use pitcrew_protocol::MemberId;

    /// A fresh private (0700) directory; `tempdir()` itself may be 0755.
    fn private_tmp() -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("private");
        create_private_dir(&dir).unwrap();
        (tmp, dir)
    }

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
        // A reader's token never passes for the agent's own, nor the other way round.
        let reader = Caller {
            scope: TokenScope::Reader,
            ..agent(MemberId::new())
        };
        let (_, read) = store.mint(reader).unwrap();
        assert!(read.expose().starts_with("pcr_"));
        assert_eq!(store.verify(read.expose()), Some(reader));
        let swapped = read.expose().replacen("pcr_", "pca_", 1);
        assert_eq!(store.verify(&swapped), None);
        // A session token verifies as its session's, and never passes for an agent's token, nor
        // the other way round.
        let session = Caller {
            scope: TokenScope::Session(pitcrew_protocol::ids::SessionId::new()),
            ..agent(MemberId::new())
        };
        let (_, minted) = store.mint(session).unwrap();
        assert!(minted.expose().starts_with("pcs_"));
        assert_eq!(store.verify(minted.expose()), Some(session));
        let swapped = minted.expose().replacen("pcs_", "pca_", 1);
        assert_eq!(store.verify(&swapped), None);
        let (_, agent_token) = store.mint(agent(MemberId::new())).unwrap();
        let swapped = agent_token.expose().replacen("pca_", "pcs_", 1);
        assert_eq!(store.verify(&swapped), None);
    }

    #[test]
    fn callers_must_match_their_scope() {
        let store = FileTokenStore::in_memory();
        let mut bad_agent = agent(MemberId::new());
        bad_agent.on_behalf_of = None;
        let mut bad_device = person();
        bad_device.on_behalf_of = Some(MemberId::new());
        let bad_reader = Caller {
            scope: TokenScope::Reader,
            ..bad_agent
        };
        assert!(matches!(
            store.mint(bad_agent),
            Err(TokenError::InvalidCaller)
        ));
        assert!(matches!(
            store.mint(bad_reader),
            Err(TokenError::InvalidCaller)
        ));
        assert!(matches!(
            store.mint(bad_device),
            Err(TokenError::InvalidCaller)
        ));
        let bad_session = Caller {
            scope: TokenScope::Session(pitcrew_protocol::ids::SessionId::new()),
            ..bad_agent
        };
        assert!(matches!(
            store.mint(bad_session),
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
        let (_tmp, dir) = private_tmp();
        let state = dir.as_path().join("state");
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
        let leftovers: Vec<_> = fs::read_dir(&state)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty());

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
        let (_tmp, dir) = private_tmp();
        let state = dir.as_path().join("state");
        let store = FileTokenStore::open(&state).unwrap();
        store.mint(person()).unwrap();
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&state), 0o700);
        assert_eq!(mode(&state.join(FileTokenStore::FILE_NAME)), 0o600);
    }

    /// Writes a registry file that passes the permission checks.
    fn write_registry(dir: &Path, contents: &str) {
        let path = dir.join(FileTokenStore::FILE_NAME);
        fs::write(&path, contents).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        }
    }

    fn record(id: &str, sha256: &str, caller: &serde_json::Value) -> serde_json::Value {
        serde_json::json!({ "id": id, "sha256": sha256, "caller": caller, "created_at": 0 })
    }

    #[test]
    fn malformed_registries_are_errors() {
        let id = TokenId::new().0.to_string();
        let other_id = TokenId::new().0.to_string();
        let hash = "ab".repeat(32);
        let other_hash = "cd".repeat(32);
        let device = serde_json::to_value(person()).unwrap();
        let mut agent_without_owner = serde_json::to_value(agent(MemberId::new())).unwrap();
        agent_without_owner
            .as_object_mut()
            .unwrap()
            .remove("on_behalf_of");
        let registry = |version: u32, tokens: Vec<serde_json::Value>| {
            serde_json::json!({ "version": version, "tokens": tokens }).to_string()
        };
        let cases = [
            ("not json", "{".to_owned()),
            ("bad version", registry(2, vec![])),
            ("bad sha256", registry(1, vec![record(&id, "zz", &device)])),
            (
                "invalid caller",
                registry(1, vec![record(&id, &hash, &agent_without_owner)]),
            ),
            (
                "duplicate id",
                registry(
                    1,
                    vec![
                        record(&id, &hash, &device),
                        record(&id, &other_hash, &device),
                    ],
                ),
            ),
            (
                "duplicate hash",
                registry(
                    1,
                    vec![
                        record(&id, &hash, &device),
                        record(&other_id, &hash, &device),
                    ],
                ),
            ),
        ];
        for (name, contents) in cases {
            let (_tmp, dir) = private_tmp();
            write_registry(dir.as_path(), &contents);
            assert!(
                matches!(
                    FileTokenStore::open(dir.as_path()),
                    Err(TokenError::Malformed { .. })
                ),
                "{name}"
            );
        }

        let (_tmp, dir) = private_tmp();
        write_registry(
            dir.as_path(),
            &registry(
                1,
                vec![
                    record(&id, &hash, &device),
                    record(&other_id, &other_hash, &device),
                ],
            ),
        );
        assert_eq!(FileTokenStore::open(dir.as_path()).unwrap().list().len(), 2);
    }

    #[test]
    fn only_one_store_may_open_a_registry() {
        let (_tmp, dir) = private_tmp();
        let first = FileTokenStore::open(dir.as_path()).unwrap();
        assert!(matches!(
            FileTokenStore::open(dir.as_path()),
            Err(TokenError::Locked { .. })
        ));
        drop(first);
        FileTokenStore::open(dir.as_path()).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_registry_others_can_write_is_refused() {
        use std::os::unix::fs::PermissionsExt as _;
        let (_tmp, dir) = private_tmp();
        write_registry(dir.as_path(), r#"{"version":1,"tokens":[]}"#);
        let path = dir.as_path().join(FileTokenStore::FILE_NAME);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o620)).unwrap();
        assert!(matches!(
            FileTokenStore::open(dir.as_path()),
            Err(TokenError::Io { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_registry_is_refused() {
        let (_tmp, dir) = private_tmp();
        let elsewhere = tempfile::tempdir().unwrap();
        write_registry(elsewhere.path(), r#"{"version":1,"tokens":[]}"#);
        std::os::unix::fs::symlink(
            elsewhere.path().join(FileTokenStore::FILE_NAME),
            dir.as_path().join(FileTokenStore::FILE_NAME),
        )
        .unwrap();
        assert!(matches!(
            FileTokenStore::open(dir.as_path()),
            Err(TokenError::Io { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn an_open_state_directory_is_refused() {
        use std::os::unix::fs::PermissionsExt as _;
        let (_tmp, dir) = private_tmp();
        fs::set_permissions(dir.as_path(), fs::Permissions::from_mode(0o775)).unwrap();
        assert!(matches!(
            FileTokenStore::open(dir.as_path()),
            Err(TokenError::Io { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_write_leaves_the_store_unchanged() {
        use std::os::unix::fs::PermissionsExt as _;
        if crate::euid() == 0 {
            eprintln!(
                "skipped: running as root, which writes into a read-only directory anyway, so \
                 the write cannot be made to fail"
            );
            return;
        }
        let (_tmp, dir) = private_tmp();
        let store = FileTokenStore::open(dir.as_path()).unwrap();
        // A read-only directory makes creating the temporary file fail.
        fs::set_permissions(dir.as_path(), fs::Permissions::from_mode(0o500)).unwrap();
        let result = store.mint(person());
        fs::set_permissions(dir.as_path(), fs::Permissions::from_mode(0o700)).unwrap();
        assert!(matches!(result, Err(TokenError::Io { .. })));
        assert!(store.list().is_empty());
    }
}
