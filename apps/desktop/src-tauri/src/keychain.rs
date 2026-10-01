//! Where device tokens for remote workspaces will live (ADR-0006: "stored in the OS keychain").
//!
//! Nothing uses this yet besides its tests; remote pairing will. The local workspace's token is
//! never copied here: it stays in the daemon's own token file, read on every connection.

use crate::token::DeviceToken;
use std::collections::HashMap;
use std::fmt;
use std::sync::Mutex;

/// The keychain service under which the app keeps its tokens, one entry per workspace id.
pub const SERVICE: &str = "org.pitcrew.desktop";

/// A keychain failure. Never holds a token.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("keychain: {0}")]
pub struct KeychainError(pub String);

/// Device tokens by workspace id.
pub trait TokenStore: Send + Sync {
    /// The token for `workspace`, if there is one.
    ///
    /// # Errors
    /// The keychain cannot be read, or holds something that is not a token.
    fn get(&self, workspace: &str) -> Result<Option<DeviceToken>, KeychainError>;

    /// Stores `token` for `workspace`, replacing any other.
    ///
    /// # Errors
    /// The keychain cannot be written.
    fn set(&self, workspace: &str, token: &DeviceToken) -> Result<(), KeychainError>;

    /// Removes the token for `workspace`. Removing a missing one succeeds.
    ///
    /// # Errors
    /// The keychain cannot be written.
    fn delete(&self, workspace: &str) -> Result<(), KeychainError>;
}

/// The OS keychain: Keychain Services (macOS), Credential Manager (Windows), the Secret Service
/// (Linux, over D-Bus).
pub struct OsKeychain {
    service: String,
}

impl fmt::Debug for OsKeychain {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OsKeychain")
            .field("service", &self.service)
            .finish()
    }
}

impl Default for OsKeychain {
    fn default() -> Self {
        Self::new(SERVICE)
    }
}

impl OsKeychain {
    /// The keychain entries of `service`.
    #[must_use]
    pub fn new(service: impl Into<String>) -> Self {
        Self {
            service: service.into(),
        }
    }

    fn entry(&self, workspace: &str) -> Result<keyring::Entry, KeychainError> {
        keyring::Entry::new(&self.service, workspace).map_err(describe)
    }
}

/// A keyring error, without anything it might quote.
fn describe(e: keyring::Error) -> KeychainError {
    let what = match e {
        keyring::Error::NoEntry => "no such entry",
        keyring::Error::NoStorageAccess(_) => "the keychain is locked or unavailable",
        keyring::Error::PlatformFailure(_) => "the keychain failed",
        keyring::Error::BadEncoding(_) => "the stored token is not text",
        keyring::Error::NoDefaultStore => "there is no keychain on this system",
        _ => "the keychain refused",
    };
    KeychainError(what.into())
}

impl TokenStore for OsKeychain {
    fn get(&self, workspace: &str) -> Result<Option<DeviceToken>, KeychainError> {
        match self.entry(workspace)?.get_password() {
            Ok(text) => DeviceToken::new(text)
                .map(Some)
                .map_err(|_| KeychainError("the stored entry is not a token".into())),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(describe(e)),
        }
    }

    fn set(&self, workspace: &str, token: &DeviceToken) -> Result<(), KeychainError> {
        self.entry(workspace)?
            .set_password(token.expose())
            .map_err(describe)
    }

    fn delete(&self, workspace: &str) -> Result<(), KeychainError> {
        match self.entry(workspace)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(describe(e)),
        }
    }
}

/// Tokens in memory, for tests.
#[derive(Default)]
pub struct MemoryStore {
    tokens: Mutex<HashMap<String, DeviceToken>>,
}

impl fmt::Debug for MemoryStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let count = self.tokens.lock().map_or(0, |t| t.len());
        f.debug_struct("MemoryStore")
            .field("tokens", &count)
            .finish()
    }
}

impl TokenStore for MemoryStore {
    fn get(&self, workspace: &str) -> Result<Option<DeviceToken>, KeychainError> {
        let tokens = self
            .tokens
            .lock()
            .map_err(|_| KeychainError("poisoned".into()))?;
        Ok(tokens.get(workspace).cloned())
    }

    fn set(&self, workspace: &str, token: &DeviceToken) -> Result<(), KeychainError> {
        self.tokens
            .lock()
            .map_err(|_| KeychainError("poisoned".into()))?
            .insert(workspace.to_owned(), token.clone());
        Ok(())
    }

    fn delete(&self, workspace: &str) -> Result<(), KeychainError> {
        self.tokens
            .lock()
            .map_err(|_| KeychainError("poisoned".into()))?
            .remove(workspace);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What every store must do.
    fn behaves(store: &dyn TokenStore, workspace: &str) {
        let first = DeviceToken::new("pcd_first-token").unwrap();
        let second = DeviceToken::new("pcd_second-token").unwrap();
        store.delete(workspace).unwrap();
        assert_eq!(store.get(workspace).unwrap(), None);
        store.set(workspace, &first).unwrap();
        assert_eq!(store.get(workspace).unwrap(), Some(first));
        store.set(workspace, &second).unwrap();
        assert_eq!(store.get(workspace).unwrap(), Some(second));
        store.delete(workspace).unwrap();
        assert_eq!(store.get(workspace).unwrap(), None);
        store.delete(workspace).unwrap();
    }

    #[test]
    fn the_memory_store_keeps_tokens_by_workspace() {
        let store = MemoryStore::default();
        behaves(&store, "01JA");
        store
            .set("01JB", &DeviceToken::new("pcd_b").unwrap())
            .unwrap();
        assert_eq!(store.get("01JA").unwrap(), None);
        assert!(!format!("{store:?}").contains("pcd_"));
    }

    /// Needs a desktop session with an unlocked keychain (a Secret Service on Linux), which CI
    /// does not have: run with `cargo test -- --ignored` on a desktop.
    #[test]
    #[ignore = "needs an unlocked OS keychain"]
    fn the_os_keychain_keeps_tokens_by_workspace() {
        let store = OsKeychain::new("org.pitcrew.desktop.test");
        behaves(&store, "keychain-test-workspace");
    }
}
