//! What the hub keeps about its integrations, in its state directory (never the event log):
//!
//! | Path | What |
//! |---|---|
//! | `integrations.json` | The connections (no secret), each one's sync member, status and the upstream titles of linked scopes, and how far the outward-write planner has read the log. Private (0600). |
//! | `integrations/<id>.state.json` | One connection's sync state (`pitcrew_sync_github::SyncState` or `pitcrew_sync_jira::SyncState`: cursors, `ETag`s, snapshots). Private (0600). |
//! | `integrations/<id>.secret` | Its stored secret, when it has one (see `secret.rs`). |

use crate::state::{read_json_up_to, remove, write_json};
use pitcrew_protocol::ids::{IntegrationId, MemberId};
use pitcrew_protocol::integrations::{CredentialSource, IntegrationSettings, SyncStatus};
use pitcrew_protocol::model::TimestampMs;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

/// The largest `integrations.json` read.
const MAX_SAVED: u64 = 4 * 1024 * 1024;
/// The largest sync state read: snapshots of up to a few thousand items, bodies capped at 64 KiB.
const MAX_STATE: u64 = 256 * 1024 * 1024;

/// One connection, as kept.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub id: IntegrationId,
    pub name: String,
    pub settings: IntegrationSettings,
    pub credential: CredentialSource,
    pub interval_minutes: u32,
    pub added_by: MemberId,
    pub added_at: TimestampMs,
    /// The member this connection's sync acts as: `@sync` (or `@tracker-sync`), an agent of
    /// `added_by`. `None` in a file written before it was kept here; the next sync finds or adds
    /// it.
    #[serde(default)]
    pub sync_member: Option<MemberId>,
    /// The last sync's status (`running` is never kept).
    #[serde(default)]
    pub status: SyncStatus,
    /// Upstream titles of milestones and epics, by key (`owner/repo#milestone:1`, `DEMO-5`).
    #[serde(default)]
    pub titles: BTreeMap<String, String>,
    /// The link keys each repository or project was last synced with: when they change, that
    /// scope's issues are read again from the start.
    #[serde(default)]
    pub linked: BTreeMap<String, Vec<String>>,
}

/// `integrations.json`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Saved {
    /// The connections, oldest first.
    #[serde(default)]
    pub integrations: Vec<Record>,
    /// The last revision of the event log the outward-write planner has read (`writes.rs`);
    /// `None` until it first runs, which starts it at the log's end.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub writes_rev: Option<u64>,
}

/// The files: `integrations.json` and the `integrations/` folder.
#[derive(Clone, Debug)]
pub struct Files {
    saved: PathBuf,
    dir: PathBuf,
}

impl Files {
    /// The files in the state directory `root`.
    #[must_use]
    pub fn new(root: &Path) -> Self {
        Self {
            saved: root.join("integrations.json"),
            dir: root.join("integrations"),
        }
    }

    /// The folder for sync states and secrets.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn state(&self, id: &IntegrationId) -> PathBuf {
        self.dir.join(format!("{}.state.json", id.0))
    }

    /// The saved connections; none when there is no file yet.
    ///
    /// # Errors
    /// The file cannot be read, or does not hold them.
    pub fn load(&self) -> io::Result<Saved> {
        Ok(read_json_up_to(&self.saved, "the integrations", MAX_SAVED)?.unwrap_or_default())
    }

    /// Saves the connections.
    ///
    /// # Errors
    /// Writing fails.
    pub fn save(&self, saved: &Saved) -> io::Result<()> {
        let mut saved = saved.clone();
        for record in &mut saved.integrations {
            record.status.running = false;
        }
        write_json(&self.saved, &saved)
    }

    /// One connection's sync state; `None` when it has none (or a file that no longer parses,
    /// which then starts a fresh sync).
    #[must_use]
    pub fn load_state<T: DeserializeOwned>(&self, id: &IntegrationId) -> Option<T> {
        match read_json_up_to(&self.state(id), "a sync state", MAX_STATE) {
            Ok(state) => state,
            Err(e) => {
                tracing::warn!(integration = %id, error = %e, "the sync state is unreadable; syncing afresh");
                None
            }
        }
    }

    /// Saves one connection's sync state.
    ///
    /// # Errors
    /// The folder cannot be made private, or writing fails.
    pub fn save_state<T: Serialize>(&self, id: &IntegrationId, state: &T) -> io::Result<()> {
        super::secret::private_dir(&self.dir)?;
        write_json(&self.state(id), state)
    }

    /// Forgets one connection's sync state.
    ///
    /// # Errors
    /// Removing it fails.
    pub fn remove_state(&self, id: &IntegrationId) -> io::Result<()> {
        remove(&self.state(id))
    }
}
