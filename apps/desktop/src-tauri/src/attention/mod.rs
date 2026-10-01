//! "Needs you": for each ready workspace, the open asks addressed to the person, followed by the
//! gateway itself (not the webview), so the tray can count them and the app can notify while the
//! window is closed.
//!
//! - **One subscription per workspace** ([`watch`]): its own stream from the daemon, with the token
//!   the gateway already holds, resumed with `since` when it breaks.
//! - **The person** is found through `GET /v1/me`; only open asks addressed to them count, as in
//!   their Inbox ([`tracker`]).
//! - **Bounded:** at most one watcher per workspace, a bounded set of asks per workspace, size
//!   limits on everything read.

pub mod tracker;
pub mod watch;

pub use tracker::{Count, NewAsk};
pub use watch::Limits;

use crate::registry::{GatewayWorkspace, Registry, WorkspaceState};
use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard};
use tokio::sync::Notify;

/// Where "needs you" goes: the tray and the notifications.
pub trait AttentionSink: Send + Sync + 'static {
    /// A workspace's count changed; [`Attention::counts`] has the new ones.
    fn counts_changed(&self);
    /// New asks for the person arrived in `workspace`, in one frame of its stream.
    fn new_asks(&self, workspace: &str, asks: Vec<NewAsk>);
}

/// The watchers and their counts.
pub struct Attention {
    registry: Arc<Registry>,
    shared: Arc<Shared>,
    watchers: Mutex<HashMap<String, Watcher>>,
    limits: Limits,
    runtime: tokio::runtime::Handle,
}

impl fmt::Debug for Attention {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Attention")
            .field("counts", &self.counts())
            .finish_non_exhaustive()
    }
}

struct Shared {
    counts: Mutex<HashMap<String, Count>>,
    sink: Arc<dyn AttentionSink>,
}

impl watch::Report for Shared {
    fn count(&self, workspace: &str, count: Count) {
        let changed = {
            let mut counts = self
                .counts
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            counts.insert(workspace.to_owned(), count) != Some(count)
        };
        if changed {
            self.sink.counts_changed();
        }
    }

    fn new_asks(&self, workspace: &str, asks: Vec<NewAsk>) {
        self.sink.new_asks(workspace, asks);
    }
}

struct Watcher {
    task: tokio::task::JoinHandle<()>,
    poke: Arc<Notify>,
    ready: bool,
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Attention {
    /// Follows the workspaces in `registry` (call [`Self::sync`] when its list changes), on
    /// `runtime`, reporting to `sink`.
    #[must_use]
    pub fn new(
        registry: Arc<Registry>,
        sink: Arc<dyn AttentionSink>,
        runtime: tokio::runtime::Handle,
    ) -> Self {
        Self::with_limits(registry, sink, runtime, Limits::default())
    }

    /// With other limits (tests).
    #[must_use]
    pub fn with_limits(
        registry: Arc<Registry>,
        sink: Arc<dyn AttentionSink>,
        runtime: tokio::runtime::Handle,
        limits: Limits,
    ) -> Self {
        Self {
            registry,
            shared: Arc::new(Shared {
                counts: Mutex::default(),
                sink,
            }),
            watchers: Mutex::default(),
            limits,
            runtime,
        }
    }

    /// Brings the watchers in line with the workspace list: one for each workspace that has been
    /// ready, none for workspaces that are gone. A workspace that becomes ready again reconnects
    /// at once.
    pub fn sync(&self, list: &[GatewayWorkspace]) {
        let mut gone = false;
        {
            let mut watchers = self.lock();
            watchers.retain(|id, _| {
                let keep = list.iter().any(|w| &w.id == id);
                gone |= !keep;
                keep
            });
            for workspace in list {
                let ready = workspace.state == WorkspaceState::Ready;
                match watchers.get_mut(&workspace.id) {
                    Some(watcher) => {
                        if ready && !watcher.ready {
                            watcher.poke.notify_one();
                        }
                        watcher.ready = ready;
                    }
                    None if ready => {
                        let poke = Arc::new(Notify::new());
                        let task = self.runtime.spawn(watch::watch(
                            workspace.id.clone(),
                            Arc::clone(&self.registry),
                            Arc::clone(&poke),
                            Arc::clone(&self.shared) as Arc<dyn watch::Report>,
                            self.limits,
                        ));
                        tracing::debug!(workspace = %workspace.id, "watching for asks");
                        watchers.insert(workspace.id.clone(), Watcher { task, poke, ready });
                    }
                    None => {}
                }
            }
        }
        if gone {
            let mut counts = self
                .shared
                .counts
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            counts.retain(|id, _| list.iter().any(|w| &w.id == id));
            drop(counts);
            self.shared.sink.counts_changed();
        }
    }

    /// Each watched workspace's count, once known.
    #[must_use]
    pub fn counts(&self) -> HashMap<String, Count> {
        self.shared
            .counts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// How many workspaces are watched.
    #[must_use]
    pub fn watching(&self) -> usize {
        self.lock().len()
    }

    /// Stops every watcher.
    pub fn stop(&self) {
        self.lock().clear();
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<String, Watcher>> {
        self.watchers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl Drop for Attention {
    fn drop(&mut self) {
        self.stop();
    }
}
