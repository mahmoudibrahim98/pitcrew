//! The workspace registry: which workspaces the app knows, how to reach each one, and its state.
//!
//! - Saved as `workspaces.json` in the app's local data directory, written atomically (a new
//!   private file renamed over the old one) with private permissions. It holds no secret: device
//!   tokens live in the daemon's token file (local) or the OS keychain (remote, later).
//! - The state of each workspace (`connecting`, `ready`, `unreachable`, `needs_pairing`) lives in
//!   memory only. Every change to the list calls the change listener, which emits
//!   `gateway://workspaces` in the app.
//! - On first start the list is empty until the local daemon answers `GET /v1/workspace`; then
//!   the local workspace is registered with its id and name. An id a remote workspace holds is
//!   refused, and kept: once that remote is gone, the local workspace comes back with it.
//! - A remote workspace ([`Connection::Remote`]) keeps how to reach its machine: the host, the
//!   launcher, the helper's root and platform, for SLURM the site recipe, the job options and the
//!   last hop, and the transport the tunnel found worth remembering. No secret: its device token
//!   is in the OS keychain ([`crate::keychain`]).

use crate::gateway::{Connector, GatewayError};
use pitcrew_remote::Transport;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

/// The file's name in the app's local data directory.
pub const FILE_NAME: &str = "workspaces.json";
/// The file format's version.
const VERSION: u32 = 1;
/// The longest registry file read.
const MAX_FILE: u64 = 4 * 1024 * 1024;

/// Where a workspace's daemon runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceKind {
    /// On this machine.
    Local,
    /// On another machine, through an SSH tunnel.
    Remote,
}

/// How to reach a workspace's daemon.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Connection {
    /// The person's own `pitcrewd` on this machine, over its private socket or pipe, with the
    /// token from the file `pitcrewd token show-path` names.
    Local,
    /// A helper on another machine, through the tunnel of `pitcrew-remote`, with the token kept
    /// in the OS keychain.
    Remote(Box<RemoteConnection>),
}

/// How a remote workspace's helper was started, and so how it is found, checked and stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LauncherKind {
    /// In the background (`setsid nohup`).
    Direct,
    /// In its own tmux session.
    Tmux,
    /// As a SLURM batch job on a compute node.
    Slurm,
}

impl LauncherKind {
    /// Its name, as the contract and `endpoint.json` write it.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::Tmux => "tmux",
            Self::Slurm => "slurm",
        }
    }
}

/// How the login node reaches a SLURM job's compute node (a site recipe's `last_hop`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HopKind {
    /// `ssh <node>` from the login node.
    #[default]
    Ssh,
    /// `srun --jobid <id> --overlap` from the login node.
    Srun,
}

/// The job options the person asked for (the contract's `RemotePlanRequest.job`), as asked.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct JobRequest {
    /// `--partition`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partition: Option<String>,
    /// `--account`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    /// `--qos`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub qos: Option<String>,
    /// `--time`, as SLURM writes it (`08:00:00`, `2-00:00:00`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time: Option<String>,
    /// `--cpus-per-task`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpus: Option<u32>,
    /// `--mem`, e.g. `8G`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<String>,
    /// GPUs for `--gres`: a count (`2`), or a type and count (`a100:2`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpus: Option<String>,
}

/// A local distribution, bound into a plan and persisted for reconnect.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum WslTarget {
    /// WSL2; WSL1 is rejected before probing.
    Wsl { distro: String },
}

/// How to reach a remote workspace's helper. Nothing here is a secret.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteConnection {
    /// Local WSL target; absent for existing SSH entries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<WslTarget>,
    /// The host, as given to ssh (a `Host` of the person's ssh config, or a name they typed).
    pub host: String,
    /// How the helper was started.
    pub launcher: LauncherKind,
    /// Where PitCrew lives on the machine (`~/.pitcrew`, absolute).
    pub root: String,
    /// The machine's platform, as `pitcrew_remote::Platform::target` names it.
    pub platform: String,
    /// The SLURM site recipe the job was made from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub site: Option<String>,
    /// The SLURM job options the person asked for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job: Option<JobRequest>,
    /// How the login node reaches the job's node (SLURM).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_hop: Option<HopKind>,
    /// The transport worth remembering for this machine (`pitcrew_remote::Connector::transport`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport: Option<Transport>,
}

/// A workspace as saved.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceRecord {
    /// The daemon's workspace id (a ULID), as in `/w/$ws/…`.
    pub id: String,
    /// Its name.
    pub name: String,
    /// Local or remote.
    pub kind: WorkspaceKind,
    /// How to reach its daemon.
    pub connection: Connection,
}

/// A workspace's state, as the contract names them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceState {
    /// Being reached.
    Connecting,
    /// Requests go through.
    Ready,
    /// Cannot be reached; `detail` says why.
    Unreachable,
    /// Needs pairing before anything goes through.
    NeedsPairing,
}

/// A workspace as `gateway_workspaces` returns it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct GatewayWorkspace {
    /// The daemon's workspace id.
    pub id: String,
    /// Its name.
    pub name: String,
    /// WSL distribution, shown as `wsl:<distro>`; absent for older workspace entries.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// `local` or `remote`.
    pub kind: WorkspaceKind,
    /// Its state.
    pub state: WorkspaceState,
    /// Why it is unreachable or needs pairing, for people to read.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct RegistryFile {
    version: u32,
    workspaces: Vec<WorkspaceRecord>,
}

struct Entry {
    record: WorkspaceRecord,
    state: WorkspaceState,
    detail: Option<String>,
    connector: Option<Arc<dyn Connector>>,
    /// The [`Registry::claim_remote`] that put it in (0: none, it was loaded or is the local
    /// workspace). Attaching another connector keeps it.
    claim: u64,
}

/// Why [`Registry::claim_remote`] refused an id: another workspace holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Taken {
    /// The name it is held under.
    pub name: String,
    /// Whether that is the local workspace.
    pub local: bool,
}

/// What [`Registry::claim_remote`] did, to undo it ([`Registry::unclaim`]).
pub struct Claimed {
    id: String,
    previous: Option<Entry>,
    /// The claim's generation: its entry is the one at `id` that carries it.
    claim: u64,
}

impl fmt::Debug for Claimed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Claimed")
            .field("id", &self.id)
            .field("replaced", &self.previous.is_some())
            .finish()
    }
}

type Listener = Arc<dyn Fn(&[GatewayWorkspace]) + Send + Sync>;

#[derive(Default)]
struct Inner {
    entries: Vec<Entry>,
    /// The local daemon's connector, attached to the local workspace once it is registered.
    local: Option<Arc<dyn Connector>>,
    /// The local daemon's state while no local workspace is registered yet.
    local_state: Option<(WorkspaceState, Option<String>)>,
    /// The id and name the local daemon reported while a remote workspace held that id: taken
    /// once that remote is gone, unless the daemon's state changed meanwhile.
    refused_local: Option<(String, String)>,
    /// The last claim's generation ([`Entry::claim`]).
    claims: u64,
}

/// What registering the local workspace did.
enum Local {
    /// A remote workspace holds the id: nothing was registered.
    Refused,
    /// Registered, or updated; whether its saved record changed.
    Registered { changed: bool },
}

/// The registry.
pub struct Registry {
    file: Option<PathBuf>,
    inner: Mutex<Inner>,
    listener: Mutex<Option<Listener>>,
}

impl fmt::Debug for Registry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Registry")
            .field("file", &self.file)
            .field("workspaces", &self.list())
            .finish_non_exhaustive()
    }
}

impl Registry {
    /// A registry kept only in memory (tests).
    #[must_use]
    pub fn in_memory() -> Self {
        Self {
            file: None,
            inner: Mutex::default(),
            listener: Mutex::default(),
        }
    }

    /// Loads the registry saved at `file`. A missing file is an empty registry. A file that cannot
    /// be read as a registry is moved aside (to `<file>.invalid`) and the registry starts empty,
    /// so a bad file never stops the app and is never silently overwritten.
    #[must_use]
    pub fn load(file: PathBuf) -> Self {
        let records = match read(&file) {
            Ok(records) => records,
            Err(e) => {
                let aside = file.with_extension("json.invalid");
                tracing::warn!(file = %file.display(), error = %e, aside = %aside.display(), "the workspace registry is unreadable; starting empty");
                if let Err(e) = std::fs::rename(&file, &aside) {
                    tracing::warn!(error = %e, "cannot move the unreadable registry aside");
                }
                Vec::new()
            }
        };
        let entries = records
            .into_iter()
            .map(|record| Entry {
                record,
                state: WorkspaceState::Connecting,
                detail: None,
                connector: None,
                claim: 0,
            })
            .collect();
        Self {
            file: Some(file),
            inner: Mutex::new(Inner {
                entries,
                ..Inner::default()
            }),
            listener: Mutex::default(),
        }
    }

    /// Calls `listener` with the whole list whenever it changes.
    pub fn on_change(&self, listener: impl Fn(&[GatewayWorkspace]) + Send + Sync + 'static) {
        *self
            .listener
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::new(listener));
    }

    /// The workspaces, in order.
    #[must_use]
    pub fn list(&self) -> Vec<GatewayWorkspace> {
        list_of(&self.lock().entries)
    }

    /// How to reach workspace `id`.
    ///
    /// # Errors
    /// `unknown_workspace`; `needs_pairing`; `unreachable` when it has no connection yet.
    pub fn connector(&self, id: &str) -> Result<Arc<dyn Connector>, GatewayError> {
        let inner = self.lock();
        let entry = inner
            .entries
            .iter()
            .find(|e| e.record.id == id)
            .ok_or_else(|| GatewayError::unknown_workspace(id))?;
        if entry.state == WorkspaceState::NeedsPairing {
            return Err(GatewayError::needs_pairing(
                entry
                    .detail
                    .clone()
                    .unwrap_or_else(|| "pair this workspace first".into()),
            ));
        }
        entry
            .connector
            .clone()
            .ok_or_else(|| GatewayError::unreachable("this workspace has no connection yet"))
    }

    /// Sets the local daemon's connector, and attaches it to the local workspace if there is one.
    pub fn attach_local(&self, connector: Arc<dyn Connector>) {
        let mut inner = self.lock();
        for entry in &mut inner.entries {
            if entry.record.kind == WorkspaceKind::Local {
                entry.connector = Some(Arc::clone(&connector));
            }
        }
        inner.local = Some(connector);
    }

    /// Registers the local workspace with the id and name its daemon reported, or updates the
    /// registered one, and marks it ready. There is one local workspace: if the daemon now reports
    /// another id (its state was reset), the entry takes the new id.
    ///
    /// An id a remote workspace holds is refused: the local workspace (or, on first start, the
    /// local daemon's state) is `unreachable`, saying so, and nothing is replaced. The id and name
    /// are kept, and registered once that remote workspace is removed (unless the local daemon's
    /// state changes first).
    ///
    /// # Errors
    /// The registry file cannot be written. The registry in memory is updated anyway.
    pub fn set_local(&self, id: &str, name: &str) -> io::Result<()> {
        let mut inner = self.lock();
        let before = list_of(&inner.entries);
        let saved = match register_local(&mut inner, id, name) {
            Local::Registered { changed: true } => self.save(&inner.entries),
            Local::Registered { changed: false } | Local::Refused => Ok(()),
        };
        self.changed(inner, &before);
        saved
    }

    /// The local workspace the daemon reported while a remote workspace held its id, registered
    /// now if that id is free. Called with the lock held, after an entry went.
    fn take_refused_local(&self, inner: &mut Inner) {
        let Some((id, name)) = inner.refused_local.clone() else {
            return;
        };
        if inner.entries.iter().any(|e| e.record.id == id) {
            return;
        }
        tracing::info!(workspace = %id, "the remote workspace that held the local id is gone; registering the local workspace");
        if let Local::Registered { changed: true } = register_local(inner, &id, &name)
            && let Err(e) = self.save(&inner.entries)
        {
            tracing::warn!(workspace = %id, error = %e, "the local workspace is registered but not saved");
        }
    }

    /// Sets the local workspace's state, or remembers it until the local workspace is registered.
    /// A refused id kept for later ([`Registry::set_local`]) is dropped: the daemon moved on.
    pub fn set_local_state(&self, state: WorkspaceState, detail: Option<String>) {
        let mut inner = self.lock();
        let before = list_of(&inner.entries);
        inner.refused_local = None;
        let mut found = false;
        for entry in &mut inner.entries {
            if entry.record.kind == WorkspaceKind::Local {
                entry.state = state;
                entry.detail.clone_from(&detail);
                found = true;
            }
        }
        if !found {
            if let Some(detail) = &detail {
                tracing::info!(
                    ?state,
                    detail,
                    "the local daemon has no workspace registered yet"
                );
            }
            inner.local_state = Some((state, detail));
        }
        self.changed(inner, &before);
    }

    /// The local daemon's state while no local workspace is registered (first start).
    #[must_use]
    pub fn pending_local_state(&self) -> Option<(WorkspaceState, Option<String>)> {
        self.lock().local_state.clone()
    }

    /// Registers remote workspace `record` (a [`Connection::Remote`]) with its connector, in
    /// `state`, unless its id is another workspace's: the check and the insert are one step.
    ///
    /// The id comes from the remote hub (`GET /v1/workspace`), so it is not trusted: a hub may
    /// claim the id of a workspace already here. That is refused when the id is the local
    /// workspace's, or a remote one's on another machine (another host or root). Only the same
    /// machine may take its id again (pairing it again), and then its entry is replaced.
    ///
    /// # Errors
    /// [`Taken`], with the name the id is held under; nothing changed.
    pub fn claim_remote(
        &self,
        record: WorkspaceRecord,
        connector: Arc<dyn Connector>,
        state: WorkspaceState,
    ) -> Result<Claimed, Taken> {
        let Connection::Remote(new) = &record.connection else {
            return Err(Taken {
                name: record.name.clone(),
                local: true,
            });
        };
        let mut inner = self.lock();
        let before = list_of(&inner.entries);
        let held = inner.entries.iter().position(|e| e.record.id == record.id);
        if let Some(at) = held {
            let old = &inner.entries[at].record;
            let same_machine = match &old.connection {
                Connection::Remote(old) => {
                    old.host == new.host && old.root == new.root && old.target == new.target
                }
                Connection::Local => false,
            };
            if old.kind == WorkspaceKind::Local || !same_machine {
                return Err(Taken {
                    name: old.name.clone(),
                    local: old.kind == WorkspaceKind::Local,
                });
            }
        }
        let id = record.id.clone();
        inner.claims += 1;
        let claim = inner.claims;
        let entry = Entry {
            record,
            state,
            detail: None,
            connector: Some(connector),
            claim,
        };
        let previous = match held {
            Some(at) => Some(std::mem::replace(&mut inner.entries[at], entry)),
            None => {
                inner.entries.push(entry);
                None
            }
        };
        if let Err(e) = self.save(&inner.entries) {
            tracing::warn!(workspace = %id, error = %e, "the workspace is added but the registry is not saved");
        }
        self.changed(inner, &before);
        Ok(Claimed {
            id,
            previous,
            claim,
        })
    }

    /// Undoes [`Registry::claim_remote`]: the entry it replaced comes back, or the one it added
    /// goes. Only while the entry at its id is still the claim's own (its generation; attaching
    /// another connector keeps it): one that was removed meanwhile stays removed, and one that
    /// replaced it stays.
    pub fn unclaim(&self, claimed: Claimed) {
        let mut inner = self.lock();
        let before = list_of(&inner.entries);
        let Some(at) = inner
            .entries
            .iter()
            .position(|e| e.record.id == claimed.id && e.claim == claimed.claim)
        else {
            tracing::info!(workspace = %claimed.id, "the claim's entry is gone or replaced; nothing to undo");
            return;
        };
        match claimed.previous {
            Some(previous) => inner.entries[at] = previous,
            None => {
                inner.entries.remove(at);
                self.take_refused_local(&mut inner);
            }
        }
        if let Err(e) = self.save(&inner.entries) {
            tracing::warn!(workspace = %claimed.id, error = %e, "cannot save the workspace registry");
        }
        self.changed(inner, &before);
    }

    /// Takes `name` for remote workspace `id` (its hub's own, cleaned), if it differs.
    ///
    /// # Errors
    /// The registry file cannot be written. The registry in memory is updated anyway.
    pub fn rename_remote(&self, id: &str, name: &str) -> io::Result<()> {
        let mut inner = self.lock();
        let before = list_of(&inner.entries);
        let renamed = inner
            .entries
            .iter_mut()
            .find(|e| e.record.id == id && e.record.kind == WorkspaceKind::Remote)
            .is_some_and(|entry| {
                if entry.record.name == name {
                    return false;
                }
                entry.record.name = name.to_owned();
                true
            });
        let saved = if renamed {
            tracing::info!(workspace = %id, "the hub's workspace has a new name");
            self.save(&inner.entries)
        } else {
            Ok(())
        };
        self.changed(inner, &before);
        saved
    }

    /// Sets a workspace's state.
    pub fn set_state(&self, id: &str, state: WorkspaceState, detail: Option<String>) {
        self.set_state_of(id, None, state, detail);
    }

    /// Sets remote workspace `id`'s state; nothing if `id` is not a remote one's. What follows a
    /// remote connection uses this, so a link that outlived its workspace cannot set the state of
    /// the local workspace that took its id.
    pub fn set_remote_state(&self, id: &str, state: WorkspaceState, detail: Option<String>) {
        self.set_state_of(id, Some(WorkspaceKind::Remote), state, detail);
    }

    fn set_state_of(
        &self,
        id: &str,
        kind: Option<WorkspaceKind>,
        state: WorkspaceState,
        detail: Option<String>,
    ) {
        let mut inner = self.lock();
        let before = list_of(&inner.entries);
        if let Some(entry) = inner
            .entries
            .iter_mut()
            .find(|e| e.record.id == id && kind.is_none_or(|k| e.record.kind == k))
        {
            entry.state = state;
            entry.detail = detail;
        }
        self.changed(inner, &before);
    }

    /// Workspace `id` as saved.
    #[must_use]
    pub fn record(&self, id: &str) -> Option<WorkspaceRecord> {
        self.lock()
            .entries
            .iter()
            .find(|e| e.record.id == id)
            .map(|e| e.record.clone())
    }

    /// Every workspace as saved, in order.
    #[must_use]
    pub fn records(&self) -> Vec<WorkspaceRecord> {
        self.lock()
            .entries
            .iter()
            .map(|e| e.record.clone())
            .collect()
    }

    /// Gives workspace `id` its connector (a remote workspace loaded from the file).
    pub fn attach(&self, id: &str, connector: Arc<dyn Connector>) {
        if let Some(entry) = self.lock().entries.iter_mut().find(|e| e.record.id == id) {
            entry.connector = Some(connector);
        }
    }

    /// Gives remote workspace `id` its connector; nothing if `id` is not a remote one's (the
    /// local workspace that took its id back keeps the local daemon's).
    pub fn attach_remote(&self, id: &str, connector: Arc<dyn Connector>) {
        if let Some(entry) = self
            .lock()
            .entries
            .iter_mut()
            .find(|e| e.record.id == id && e.record.kind == WorkspaceKind::Remote)
        {
            entry.connector = Some(connector);
        }
    }

    /// Forgets workspace `id`, and returns what was saved for it. If the local daemon reported
    /// that id while this workspace held it, the local workspace is registered now.
    ///
    /// # Errors
    /// The registry file cannot be written. The workspace is gone from memory anyway.
    pub fn remove(&self, id: &str) -> io::Result<Option<WorkspaceRecord>> {
        let mut inner = self.lock();
        let before = list_of(&inner.entries);
        let Some(at) = inner.entries.iter().position(|e| e.record.id == id) else {
            return Ok(None);
        };
        let entry = inner.entries.remove(at);
        self.take_refused_local(&mut inner);
        let saved = self.save(&inner.entries);
        self.changed(inner, &before);
        saved.map(|()| Some(entry.record))
    }

    /// Remembers the transport the tunnel found for remote workspace `id`, if it changed.
    ///
    /// # Errors
    /// The registry file cannot be written.
    pub fn set_transport(&self, id: &str, transport: Transport) -> io::Result<()> {
        let mut inner = self.lock();
        let changed = inner
            .entries
            .iter_mut()
            .find(|e| e.record.id == id)
            .is_some_and(|entry| match &mut entry.record.connection {
                Connection::Remote(remote) if remote.transport != Some(transport) => {
                    remote.transport = Some(transport);
                    true
                }
                _ => false,
            });
        if changed {
            self.save(&inner.entries)
        } else {
            Ok(())
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Calls the listener, outside the lock, if the list differs from `before`.
    fn changed(&self, inner: MutexGuard<'_, Inner>, before: &[GatewayWorkspace]) {
        let after = list_of(&inner.entries);
        drop(inner);
        if after == before {
            return;
        }
        let listener = self
            .listener
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let Some(listener) = listener {
            listener(&after);
        }
    }

    fn save(&self, entries: &[Entry]) -> io::Result<()> {
        let Some(file) = &self.file else {
            return Ok(());
        };
        let saved = RegistryFile {
            version: VERSION,
            workspaces: entries.iter().map(|e| e.record.clone()).collect(),
        };
        let json = serde_json::to_vec_pretty(&saved).map_err(io::Error::other)?;
        write_private(file, &json).inspect_err(|e| {
            tracing::warn!(file = %file.display(), error = %e, "cannot save the workspace registry");
        })
    }
}

/// Registers the local workspace as its daemon reported it ([`Registry::set_local`]), with the
/// lock held.
fn register_local(inner: &mut Inner, id: &str, name: &str) -> Local {
    if let Some(remote) = inner
        .entries
        .iter()
        .find(|e| e.record.id == id && e.record.kind != WorkspaceKind::Local)
    {
        let detail = format!(
            "this computer's hub reports the id of the remote workspace {}; remove that one \
             first",
            crate::gateway::error::shorten(&remote.record.name)
        );
        tracing::warn!(workspace = %id, "the local daemon reports the id of a remote workspace; not registering it");
        let mut found = false;
        for entry in &mut inner.entries {
            if entry.record.kind == WorkspaceKind::Local {
                entry.state = WorkspaceState::Unreachable;
                entry.detail = Some(detail.clone());
                found = true;
            }
        }
        if !found {
            inner.local_state = Some((WorkspaceState::Unreachable, Some(detail)));
        }
        inner.refused_local = Some((id.to_owned(), name.to_owned()));
        return Local::Refused;
    }
    let connector = inner.local.clone();
    inner.local_state = None;
    inner.refused_local = None;
    let record = WorkspaceRecord {
        id: id.to_owned(),
        name: name.to_owned(),
        kind: WorkspaceKind::Local,
        connection: Connection::Local,
    };
    let changed = match inner
        .entries
        .iter_mut()
        .find(|e| e.record.kind == WorkspaceKind::Local)
    {
        Some(entry) => {
            if entry.record.id != id {
                tracing::warn!(old = %entry.record.id, new = %id, "the local daemon hosts another workspace now");
            }
            let changed = entry.record != record;
            entry.record = record;
            entry.state = WorkspaceState::Ready;
            entry.detail = None;
            entry.connector = connector;
            changed
        }
        None => {
            tracing::info!(workspace = %id, "registered the local workspace");
            inner.entries.push(Entry {
                record,
                state: WorkspaceState::Ready,
                detail: None,
                connector,
                claim: 0,
            });
            true
        }
    };
    Local::Registered { changed }
}

fn list_of(entries: &[Entry]) -> Vec<GatewayWorkspace> {
    entries
        .iter()
        .map(|e| GatewayWorkspace {
            id: e.record.id.clone(),
            name: e.record.name.clone(),
            host: match &e.record.connection {
                Connection::Remote(remote) => remote
                    .target
                    .as_ref()
                    .map(|WslTarget::Wsl { distro }| format!("wsl:{distro}")),
                _ => None,
            },
            kind: e.record.kind,
            state: e.state,
            detail: e.detail.clone(),
        })
        .collect()
}

fn read(file: &Path) -> io::Result<Vec<WorkspaceRecord>> {
    use std::io::Read as _;
    let mut text = Vec::new();
    match std::fs::File::open(file) {
        Ok(f) => {
            f.take(MAX_FILE).read_to_end(&mut text)?;
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    }
    let saved: RegistryFile =
        serde_json::from_slice(&text).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    if saved.version != VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unknown registry version {}", saved.version),
        ));
    }
    Ok(saved.workspaces)
}

/// Writes `bytes` to a new private file next to `path` (mode 600 on Unix; on Windows it inherits
/// the user-only ACL of the profile's local data folder), flushes it, and renames it over `path`.
///
/// # Errors
/// Creating the directory, writing or renaming fails.
pub fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    write_atomic(path, bytes, 0o600)
}

/// Writes `bytes` to a new file next to `path` with `mode & 0o777` on Unix (exactly, whatever the umask),
/// flushes it, and renames it over `path`. A link at `path` is replaced, not followed: callers
/// that must leave links alone check first.
///
/// # Errors
/// Creating the directory, writing or renaming fails.
pub fn write_atomic(path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no directory"))?;
    std::fs::create_dir_all(dir)?;
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no file name"))?;
    let mut tmp_name = std::ffi::OsString::from(".");
    tmp_name.push(name);
    tmp_name.push(format!(".{}.tmp", std::process::id()));
    let tmp = dir.join(tmp_name);
    let _ = std::fs::remove_file(&tmp);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        // Never wider than asked while it is written; exactly `mode` before it is renamed.
        options.mode(mode & 0o600);
    }
    #[cfg(not(unix))]
    let _ = mode;
    let written = options.open(&tmp).and_then(|mut f| {
        f.write_all(bytes)?;
        f.sync_all()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            f.set_permissions(std::fs::Permissions::from_mode(mode & 0o777))?;
        }
        drop(f);
        std::fs::rename(&tmp, path)
    });
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway::{BoxFuture, Connected, ErrorCode};

    struct Nowhere;
    impl Connector for Nowhere {
        fn connect(&self) -> BoxFuture<'_, Result<Connected, GatewayError>> {
            Box::pin(async { Err(GatewayError::unreachable("nowhere")) })
        }
    }

    #[test]
    fn the_local_workspace_is_registered_saved_and_reloaded() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("data").join(FILE_NAME);
        let registry = Registry::load(file.clone());
        assert!(registry.list().is_empty());
        let events = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&events);
        registry.on_change(move |list| seen.lock().unwrap().push(list.to_vec()));
        registry.attach_local(Arc::new(Nowhere));

        registry.set_local_state(WorkspaceState::Connecting, None);
        assert!(events.lock().unwrap().is_empty(), "nothing to show yet");
        assert_eq!(
            registry.pending_local_state(),
            Some((WorkspaceState::Connecting, None))
        );

        registry
            .set_local("01J00000000000000000000000", "Demo Lab")
            .unwrap();
        let list = registry.list();
        assert_eq!(
            serde_json::to_value(&list).unwrap(),
            serde_json::json!([{ "id": "01J00000000000000000000000", "name": "Demo Lab", "kind": "local", "state": "ready" }])
        );
        assert_eq!(events.lock().unwrap().len(), 1);
        assert!(registry.connector("01J00000000000000000000000").is_ok());

        registry.set_local_state(WorkspaceState::Unreachable, Some("pitcrewd stopped".into()));
        let last = events.lock().unwrap().last().unwrap().clone();
        assert_eq!(last[0].state, WorkspaceState::Unreachable);
        assert_eq!(last[0].detail.as_deref(), Some("pitcrewd stopped"));
        // The same state again is not a change.
        registry.set_local_state(WorkspaceState::Unreachable, Some("pitcrewd stopped".into()));
        assert_eq!(events.lock().unwrap().len(), 2);

        let text = std::fs::read_to_string(&file).unwrap();
        assert!(!text.contains("state"), "states are not saved: {text}");
        let reloaded = Registry::load(file);
        let list = reloaded.list();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "Demo Lab");
        assert_eq!(list[0].state, WorkspaceState::Connecting);
        assert_eq!(
            reloaded.connector(&list[0].id).err().unwrap().code,
            ErrorCode::Unreachable,
            "no connector until the app attaches one"
        );
    }

    #[test]
    fn a_new_local_id_replaces_the_old_one() {
        let registry = Registry::in_memory();
        registry.set_local("A", "One").unwrap();
        registry.set_local("B", "Two").unwrap();
        let list = registry.list();
        assert_eq!(list.len(), 1);
        assert_eq!((list[0].id.as_str(), list[0].name.as_str()), ("B", "Two"));
    }

    #[test]
    fn unknown_and_unpaired_workspaces_fail() {
        let registry = Registry::in_memory();
        assert_eq!(
            registry.connector("nope").err().unwrap().code,
            ErrorCode::UnknownWorkspace
        );
        registry
            .claim_remote(remote("R"), Arc::new(Nowhere), WorkspaceState::NeedsPairing)
            .unwrap();
        assert_eq!(
            registry.connector("R").err().unwrap().code,
            ErrorCode::NeedsPairing
        );
        registry.set_state("R", WorkspaceState::Ready, None);
        assert!(registry.connector("R").is_ok());
    }

    fn remote(id: &str) -> WorkspaceRecord {
        WorkspaceRecord {
            id: id.into(),
            name: "Cluster".into(),
            kind: WorkspaceKind::Remote,
            connection: Connection::Remote(Box::new(RemoteConnection {
                target: None,
                host: "hpc-login".into(),
                launcher: LauncherKind::Slurm,
                root: "/home/sam/.pitcrew".into(),
                platform: "x86_64-unknown-linux-musl".into(),
                site: Some("generic".into()),
                job: Some(JobRequest {
                    partition: Some("gpu".into()),
                    time: Some("08:00:00".into()),
                    ..JobRequest::default()
                }),
                last_hop: Some(HopKind::Srun),
                transport: None,
            })),
        }
    }

    #[test]
    fn remote_workspaces_are_saved_reloaded_and_removed() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join(FILE_NAME);
        let registry = Registry::load(file.clone());
        let events = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&events);
        registry.on_change(move |list| seen.lock().unwrap().push(list.to_vec()));
        registry
            .claim_remote(remote("01JR"), Arc::new(Nowhere), WorkspaceState::Ready)
            .unwrap();
        let text = std::fs::read_to_string(&file).unwrap();
        let saved: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            saved["workspaces"][0]["connection"],
            serde_json::json!({
                "type": "remote", "host": "hpc-login", "launcher": "slurm",
                "root": "/home/sam/.pitcrew", "platform": "x86_64-unknown-linux-musl",
                "site": "generic", "job": { "partition": "gpu", "time": "08:00:00" },
                "lastHop": "srun"
            })
        );

        // The transport is remembered once, and saved.
        registry.set_transport("01JR", Transport::Stdio).unwrap();
        registry.set_transport("01JR", Transport::Stdio).unwrap();
        let reloaded = Registry::load(file.clone());
        let Some(WorkspaceRecord {
            connection: Connection::Remote(saved),
            ..
        }) = reloaded.record("01JR")
        else {
            panic!("not reloaded");
        };
        assert_eq!(saved.transport, Some(Transport::Stdio));
        assert_eq!(reloaded.list()[0].state, WorkspaceState::Connecting);
        assert!(
            reloaded.connector("01JR").is_err(),
            "no connector until attached"
        );
        reloaded.attach("01JR", Arc::new(Nowhere));
        assert!(reloaded.connector("01JR").is_ok());

        // Removing forgets it, saves, and says so.
        let before = events.lock().unwrap().len();
        let removed = registry.remove("01JR").unwrap().unwrap();
        assert_eq!(removed.id, "01JR");
        assert!(registry.list().is_empty());
        assert_eq!(events.lock().unwrap().len(), before + 1);
        assert_eq!(registry.remove("01JR").unwrap(), None);
        assert!(Registry::load(file).list().is_empty());
    }

    fn remote_on(id: &str, host: &str, root: &str) -> WorkspaceRecord {
        let mut record = remote(id);
        if let Connection::Remote(r) = &mut record.connection {
            r.host = host.into();
            r.root = root.into();
        }
        record
    }

    #[test]
    fn a_remote_cannot_take_another_workspaces_id() {
        let registry = Registry::in_memory();
        registry.attach_local(Arc::new(Nowhere));
        registry.set_local("01JL", "Here").unwrap();
        registry
            .claim_remote(
                remote_on("01JR", "hpc-login", "/home/sam/.pitcrew"),
                Arc::new(Nowhere),
                WorkspaceState::Ready,
            )
            .unwrap();
        // The local workspace's id.
        let taken = registry
            .claim_remote(
                remote_on("01JL", "evil", "/home/sam/.pitcrew"),
                Arc::new(Nowhere),
                WorkspaceState::Ready,
            )
            .unwrap_err();
        assert_eq!(
            taken,
            Taken {
                name: "Here".into(),
                local: true
            }
        );
        // A remote's id, from another host or another root.
        for (host, root) in [
            ("evil", "/home/sam/.pitcrew"),
            ("hpc-login", "/scratch/sam/.pitcrew"),
        ] {
            let taken = registry
                .claim_remote(
                    remote_on("01JR", host, root),
                    Arc::new(Nowhere),
                    WorkspaceState::Ready,
                )
                .unwrap_err();
            assert_eq!(taken.name, "Cluster");
            assert!(!taken.local);
        }
        let Some(WorkspaceRecord {
            connection: Connection::Remote(kept),
            ..
        }) = registry.record("01JR")
        else {
            panic!("the remote is gone");
        };
        assert_eq!(kept.host, "hpc-login", "nothing changed");
        assert_eq!(registry.record("01JL").unwrap().kind, WorkspaceKind::Local);

        // The same machine again (pairing it again) replaces its entry; undoing puts it back.
        let mut again = remote_on("01JR", "hpc-login", "/home/sam/.pitcrew");
        again.name = "Cluster, again".into();
        let claimed = registry
            .claim_remote(again, Arc::new(Nowhere), WorkspaceState::Connecting)
            .unwrap();
        assert_eq!(registry.record("01JR").unwrap().name, "Cluster, again");
        registry.unclaim(claimed);
        assert_eq!(registry.record("01JR").unwrap().name, "Cluster");
        assert_eq!(registry.list().len(), 2);
        // A new id is added; undoing removes it.
        let claimed = registry
            .claim_remote(
                remote_on("01JN", "gpu-box", "/home/sam/.pitcrew"),
                Arc::new(Nowhere),
                WorkspaceState::Ready,
            )
            .unwrap();
        assert_eq!(registry.list().len(), 3);
        registry.unclaim(claimed);
        assert_eq!(registry.list().len(), 2);
    }

    #[test]
    fn the_local_daemon_cannot_take_a_remotes_id() {
        let registry = Registry::in_memory();
        registry
            .claim_remote(remote("01JR"), Arc::new(Nowhere), WorkspaceState::Ready)
            .unwrap();
        // First start: no local workspace yet.
        registry.set_local("01JR", "Mine").unwrap();
        assert_eq!(registry.list().len(), 1);
        assert_eq!(registry.list()[0].kind, WorkspaceKind::Remote);
        let (state, detail) = registry.pending_local_state().unwrap();
        assert_eq!(state, WorkspaceState::Unreachable);
        assert!(detail.unwrap().contains("remote workspace"));
        // With a local workspace already: it is not replaced.
        registry.set_local("01JL", "Here").unwrap();
        registry.set_local("01JR", "Mine").unwrap();
        let local = registry
            .list()
            .into_iter()
            .find(|w| w.kind == WorkspaceKind::Local)
            .unwrap();
        assert_eq!(local.id, "01JL");
        assert_eq!(local.state, WorkspaceState::Unreachable);
        assert_eq!(registry.record("01JR").unwrap().kind, WorkspaceKind::Remote);
    }

    /// The local daemon reported an id a remote held, and was refused: once that remote is
    /// removed (or its claim undone), the local workspace comes back with that id and name,
    /// ready and saved. Not when the daemon's state changed meanwhile.
    #[test]
    fn the_local_workspace_comes_back_once_the_remote_is_gone() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join(FILE_NAME);
        let registry = Registry::load(file.clone());
        registry.attach_local(Arc::new(Nowhere));
        registry
            .claim_remote(remote("01JR"), Arc::new(Nowhere), WorkspaceState::Ready)
            .unwrap();
        // First start: refused, with no local workspace yet.
        registry.set_local("01JR", "Mine").unwrap();
        assert_eq!(
            registry.pending_local_state().unwrap().0,
            WorkspaceState::Unreachable
        );
        registry.remove("01JR").unwrap();
        let list = registry.list();
        assert_eq!(list.len(), 1, "{list:?}");
        assert_eq!(
            (list[0].id.as_str(), list[0].name.as_str(), list[0].kind),
            ("01JR", "Mine", WorkspaceKind::Local)
        );
        assert_eq!(list[0].state, WorkspaceState::Ready);
        assert!(registry.connector("01JR").is_ok(), "the local connector");
        assert_eq!(registry.pending_local_state(), None);
        assert_eq!(
            Registry::load(file).record("01JR").unwrap().kind,
            WorkspaceKind::Local,
            "saved"
        );

        // With a local workspace already, refused again: the old local entry takes the id once
        // the claim that held it is undone.
        let registry = Registry::in_memory();
        registry.attach_local(Arc::new(Nowhere));
        registry.set_local("01JL", "Here").unwrap();
        let claimed = registry
            .claim_remote(remote("01JX"), Arc::new(Nowhere), WorkspaceState::Ready)
            .unwrap();
        registry.set_local("01JX", "Here, reset").unwrap();
        assert_eq!(registry.record("01JL").unwrap().kind, WorkspaceKind::Local);
        registry.unclaim(claimed);
        let list = registry.list();
        assert_eq!(list.len(), 1, "{list:?}");
        assert_eq!(
            (list[0].id.as_str(), list[0].name.as_str(), list[0].state),
            ("01JX", "Here, reset", WorkspaceState::Ready)
        );

        // The daemon's state changed after the refusal: what it reported then is not taken.
        let registry = Registry::in_memory();
        registry
            .claim_remote(remote("01JR"), Arc::new(Nowhere), WorkspaceState::Ready)
            .unwrap();
        registry.set_local("01JR", "Mine").unwrap();
        registry.set_local_state(WorkspaceState::Connecting, None);
        registry.remove("01JR").unwrap();
        assert!(registry.list().is_empty());
    }

    /// Undoing a claim touches only the claim's own entry: one removed meanwhile stays removed
    /// (the entry it replaced does not come back), and one that replaced it stays. A connector
    /// attached to it meanwhile (a retry) keeps it the claim's own.
    #[test]
    fn unclaim_touches_only_its_own_entry() {
        let registry = Registry::in_memory();
        let claimed = registry
            .claim_remote(remote("01JA"), Arc::new(Nowhere), WorkspaceState::Ready)
            .unwrap();
        registry.attach("01JA", Arc::new(Nowhere));
        registry.unclaim(claimed);
        assert!(registry.list().is_empty(), "still its own: undone");

        let registry = Registry::in_memory();
        let mut first = remote("01JR");
        first.name = "First".into();
        registry
            .claim_remote(first, Arc::new(Nowhere), WorkspaceState::Ready)
            .unwrap();
        // Paired again (same machine), then removed before the pairing is undone.
        let claimed = registry
            .claim_remote(
                remote("01JR"),
                Arc::new(Nowhere),
                WorkspaceState::Connecting,
            )
            .unwrap();
        registry.remove("01JR").unwrap();
        registry.unclaim(claimed);
        assert!(registry.list().is_empty(), "nothing restored");

        // Paired again twice: undoing the first claim leaves the second's entry.
        let claimed = registry
            .claim_remote(remote("01JR"), Arc::new(Nowhere), WorkspaceState::Ready)
            .unwrap();
        let mut second = remote("01JR");
        second.name = "Second".into();
        registry
            .claim_remote(second, Arc::new(Nowhere), WorkspaceState::Ready)
            .unwrap();
        registry.unclaim(claimed);
        assert_eq!(registry.record("01JR").unwrap().name, "Second");
    }

    /// A remote's state setter never touches the local workspace, even with the same id (a link
    /// that outlived its remote, whose id the local workspace took back).
    #[test]
    fn a_remotes_state_never_lands_on_the_local_workspace() {
        let registry = Registry::in_memory();
        registry.attach_local(Arc::new(Nowhere));
        registry.set_local("01JL", "Here").unwrap();
        registry.set_remote_state(
            "01JL",
            WorkspaceState::Unreachable,
            Some("a remote's".into()),
        );
        let local = registry.list().remove(0);
        assert_eq!((local.state, local.detail), (WorkspaceState::Ready, None));
        registry
            .claim_remote(remote("01JR"), Arc::new(Nowhere), WorkspaceState::Ready)
            .unwrap();
        registry.set_remote_state("01JR", WorkspaceState::Connecting, None);
        assert_eq!(registry.list()[1].state, WorkspaceState::Connecting);
    }

    #[test]
    fn a_remote_takes_its_hubs_new_name() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join(FILE_NAME);
        let registry = Registry::load(file.clone());
        let events = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&events);
        registry.on_change(move |list| seen.lock().unwrap().push(list.to_vec()));
        registry.attach_local(Arc::new(Nowhere));
        registry.set_local("01JL", "Here").unwrap();
        registry
            .claim_remote(remote("01JR"), Arc::new(Nowhere), WorkspaceState::Ready)
            .unwrap();
        let before = events.lock().unwrap().len();
        registry.rename_remote("01JR", "Thesis lab").unwrap();
        assert_eq!(registry.record("01JR").unwrap().name, "Thesis lab");
        assert_eq!(events.lock().unwrap().len(), before + 1);
        // The same name again is no change; the local workspace is not a remote's to rename.
        registry.rename_remote("01JR", "Thesis lab").unwrap();
        registry.rename_remote("01JL", "Hijacked").unwrap();
        assert_eq!(events.lock().unwrap().len(), before + 1);
        assert_eq!(registry.record("01JL").unwrap().name, "Here");
        assert_eq!(
            Registry::load(file).record("01JR").unwrap().name,
            "Thesis lab"
        );
    }

    #[test]
    fn an_unreadable_file_is_moved_aside() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join(FILE_NAME);
        std::fs::write(&file, "{ not json").unwrap();
        let registry = Registry::load(file.clone());
        assert!(registry.list().is_empty());
        assert!(!file.exists());
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("workspaces.json.invalid")).unwrap(),
            "{ not json"
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_file_is_private_and_replaced_atomically() {
        use std::os::unix::fs::PermissionsExt as _;
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join(FILE_NAME);
        std::fs::write(&file, "old").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        write_private(&file, b"new").unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "new");
        let mode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let names: Vec<_> = std::fs::read_dir(tmp.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, vec![FILE_NAME]);
    }
}
