//! The workspace registry: which workspaces the app knows, how to reach each one, and its state.
//!
//! - Saved as `workspaces.json` in the app's local data directory, written atomically (a new
//!   private file renamed over the old one) with private permissions. It holds no secret: device
//!   tokens live in the daemon's token file (local) or the OS keychain (remote, later).
//! - The state of each workspace (`connecting`, `ready`, `unreachable`, `needs_pairing`) lives in
//!   memory only. Every change to the list calls the change listener, which emits
//!   `gateway://workspaces` in the app.
//! - On first start the list is empty until the local daemon answers `GET /v1/workspace`; then
//!   the local workspace is registered with its id and name.
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
    Remote(RemoteConnection),
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

/// How to reach a remote workspace's helper. Nothing here is a secret.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteConnection {
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
}

type Listener = Arc<dyn Fn(&[GatewayWorkspace]) + Send + Sync>;

#[derive(Default)]
struct Inner {
    entries: Vec<Entry>,
    /// The local daemon's connector, attached to the local workspace once it is registered.
    local: Option<Arc<dyn Connector>>,
    /// The local daemon's state while no local workspace is registered yet.
    local_state: Option<(WorkspaceState, Option<String>)>,
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
    /// # Errors
    /// The registry file cannot be written. The registry in memory is updated anyway.
    pub fn set_local(&self, id: &str, name: &str) -> io::Result<()> {
        let mut inner = self.lock();
        let before = list_of(&inner.entries);
        let connector = inner.local.clone();
        inner.local_state = None;
        let record = WorkspaceRecord {
            id: id.to_owned(),
            name: name.to_owned(),
            kind: WorkspaceKind::Local,
            connection: Connection::Local,
        };
        let changed_record = match inner
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
                });
                true
            }
        };
        let saved = if changed_record {
            self.save(&inner.entries)
        } else {
            Ok(())
        };
        self.changed(inner, &before);
        saved
    }

    /// Sets the local workspace's state, or remembers it until the local workspace is registered.
    pub fn set_local_state(&self, state: WorkspaceState, detail: Option<String>) {
        let mut inner = self.lock();
        let before = list_of(&inner.entries);
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

    /// Adds a workspace with its connector (tests; remote workspaces later). Replaces one with the
    /// same id.
    ///
    /// # Errors
    /// The registry file cannot be written.
    pub fn insert(
        &self,
        record: WorkspaceRecord,
        connector: Option<Arc<dyn Connector>>,
        state: WorkspaceState,
    ) -> io::Result<()> {
        let mut inner = self.lock();
        let before = list_of(&inner.entries);
        inner.entries.retain(|e| e.record.id != record.id);
        inner.entries.push(Entry {
            record,
            state,
            detail: None,
            connector,
        });
        let saved = self.save(&inner.entries);
        self.changed(inner, &before);
        saved
    }

    /// Sets a workspace's state.
    pub fn set_state(&self, id: &str, state: WorkspaceState, detail: Option<String>) {
        let mut inner = self.lock();
        let before = list_of(&inner.entries);
        if let Some(entry) = inner.entries.iter_mut().find(|e| e.record.id == id) {
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

    /// Forgets workspace `id`, and returns what was saved for it.
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

fn list_of(entries: &[Entry]) -> Vec<GatewayWorkspace> {
    entries
        .iter()
        .map(|e| GatewayWorkspace {
            id: e.record.id.clone(),
            name: e.record.name.clone(),
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

    fn record(id: &str) -> WorkspaceRecord {
        WorkspaceRecord {
            id: id.into(),
            name: format!("Workspace {id}"),
            kind: WorkspaceKind::Remote,
            connection: Connection::Local,
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
            .insert(
                record("R"),
                Some(Arc::new(Nowhere)),
                WorkspaceState::NeedsPairing,
            )
            .unwrap();
        assert_eq!(
            registry.connector("R").err().unwrap().code,
            ErrorCode::NeedsPairing
        );
        registry.set_state("R", WorkspaceState::Ready, None);
        assert!(registry.connector("R").is_ok());
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
