//! # pitcrew-runner
//!
//! Runner service: watchers, session linking, derived events to the hub, file API.
//!
//! **Owned by stream D.** The work packages are in `docs/build/streams/D.md`. Build against
//! `pitcrew-protocol` and `pitcrew-interfaces` only, never another stream's internals.
//!
//! This first part notices every agent session on the machine and keeps a restart-safe index of
//! them ([`start`]):
//! - each configured [`SourceAdapter`] discovers transcripts; each gets a [`SessionId`] that never
//!   changes;
//! - hot transcripts (changed in the last day) have their folders watched, cold ones are checked
//!   on a slow sweep, and homes on network filesystems are polled;
//! - each change reads from the stored cursor, never from the start, and the items become
//!   `session_discovered`, `session_state_changed`, `tool_ran`, `file_edited` and `turn_ended`
//!   events for an [`EventSink`], through a bounded channel;
//! - a cursor is saved only after the sink accepts the events read before it, so a crash repeats
//!   events (with the same ids) rather than losing them.
//!
//! [`SessionId`]: pitcrew_protocol::ids::SessionId

#![forbid(unsafe_code)]

mod config;
mod derive;
mod fsinfo;
mod sink;
mod store;
mod watch;

pub use config::{EngineHome, PollMode, RunnerConfig, Timing};
pub use sink::{EventSink, SinkError};
pub use store::StoreError;

use pitcrew_interfaces::source::SourceAdapter;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

/// The protocol version this crate was built against.
pub const PROTOCOL_VERSION: u32 = pitcrew_protocol::PROTOCOL_VERSION;

/// Errors starting the runner.
#[derive(Debug, thiserror::Error)]
pub enum RunnerError {
    /// The index could not be opened.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// File notifications could not be set up.
    #[error("file watcher: {0}")]
    Notify(#[from] notify::Error),
    /// A thread could not be started.
    #[error("runner thread: {0}")]
    Thread(#[from] std::io::Error),
}

/// A running watcher. Stops when dropped.
#[derive(Debug)]
pub struct RunnerHandle {
    shared: Arc<watch::Shared>,
    stopping: Arc<AtomicBool>,
    watcher: Option<JoinHandle<()>>,
    dispatcher: Option<JoinHandle<()>>,
}

impl RunnerHandle {
    /// Runs discovery now, e.g. after the hub asks for a scan.
    pub fn rescan(&self) {
        self.shared.rescan();
    }

    /// Stops the watcher and waits for it. Batches already queued are still offered to the sink;
    /// if the sink is refusing, they are left unsaved and sent again after the next start.
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        self.stopping.store(true, Ordering::Relaxed);
        self.shared.stop();
        for handle in [self.watcher.take(), self.dispatcher.take()]
            .into_iter()
            .flatten()
        {
            if handle.join().is_err() {
                tracing::error!("a runner thread panicked");
            }
        }
    }
}

impl Drop for RunnerHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Opens the index in `config.state_dir` and starts watching.
///
/// # Errors
///
/// If the index cannot be opened, notifications cannot be set up, or a thread cannot start.
pub fn start(
    config: RunnerConfig,
    adapters: Vec<Arc<dyn SourceAdapter>>,
    sink: Arc<dyn EventSink>,
) -> Result<RunnerHandle, RunnerError> {
    let store = Arc::new(Mutex::new(store::Store::open(&config.state_dir)?));
    let (tx, rx) = std::sync::mpsc::sync_channel(config.channel_capacity.max(1));
    let shared = Arc::new(watch::Shared::default());
    let stopping = Arc::new(AtomicBool::new(false));
    let retry_max = config.timing.sink_retry_max;

    let watcher = watch::Watcher::new(watch::Setup {
        workspace: config.workspace,
        machine: config.machine,
        owner: config.owner,
        timing: config.timing,
        poll: config.poll,
        max_batch: config.max_batch_events,
        homes: config.homes,
        adapters,
        store: Arc::clone(&store),
        tx,
        shared: Arc::clone(&shared),
    })?;

    let dispatcher = {
        let stopping = Arc::clone(&stopping);
        std::thread::Builder::new()
            .name("pitcrew-runner-sink".into())
            .spawn(move || sink::dispatch(&rx, &*sink, &store, &stopping, retry_max))?
    };
    let watcher = std::thread::Builder::new()
        .name("pitcrew-runner-watch".into())
        .spawn(move || watcher.run());
    let mut handle = RunnerHandle {
        shared,
        stopping,
        watcher: None,
        dispatcher: Some(dispatcher),
    };
    // On failure the handle's drop stops the dispatcher.
    handle.watcher = Some(watcher?);
    Ok(handle)
}
