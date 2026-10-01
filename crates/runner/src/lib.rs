//! # pitcrew-runner
//!
//! Runner service: watchers, session linking, derived events to the hub, file API.
//!
//! **Owned by stream D.** The work packages are in `docs/build/streams/D.md`. Build against
//! `pitcrew-protocol` and `pitcrew-interfaces` only, never another stream's internals.
//!
//! The runner notices every agent session on the machine and keeps a restart-safe index of them
//! ([`start`]):
//! - each configured [`SourceAdapter`] discovers transcripts; each gets a [`SessionId`] that never
//!   changes, keyed by the transcript's canonical path;
//! - homes and the project folders in them are watched, and so are the folders of hot transcripts
//!   (changed in the last day); every transcript is also re-checked on a slow sweep, and homes on
//!   network filesystems are polled instead;
//! - each change reads from the stored cursor, never from the start, and the items become
//!   `session_discovered`, `session_state_changed`, `tool_ran`, `file_edited` and `turn_ended`
//!   events for an [`EventSink`], through a bounded channel;
//! - a cursor is saved only after the sink accepts the events read before it, so a crash repeats
//!   events (with the same ids) rather than losing them.
//!
//! Connected to a hub in the same process (the solo case in ADR-0009):
//! - [`StoreSink`] writes the events into the hub's store, once each;
//! - [`RunnerHandle::hooks`] turns agent hooks into session state ([`RunnerHooks`]);
//! - [`RunnerHandle::terminals`] serves the API's terminals from a runtime ([`RunnerTerminals`]);
//! - [`RunnerHandle::commands`] runs hub commands ([`RunnerCommands`]);
//! - with [`Locations`], sessions are linked to workstreams by folder or branch.
//!
//! ```no_run
//! # use std::sync::Arc;
//! # fn wire(
//! #     config: pitcrew_runner::RunnerConfig,
//! #     adapters: Vec<Arc<dyn pitcrew_interfaces::source::SourceAdapter>>,
//! #     store: Arc<pitcrew_store::Store>,
//! #     runtime: Arc<dyn pitcrew_interfaces::runtime::Runtime>,
//! # ) -> Result<(), Box<dyn std::error::Error>> {
//! let sink = Arc::new(pitcrew_runner::StoreSink::new(store, config.owner));
//! let runner = pitcrew_runner::start(config, adapters, sink)?;
//! let hooks = runner.hooks(); // a pitcrew_api::HookSink
//! let terminals = runner.terminals(runtime)?; // a pitcrew_api::Terminals
//! let commands = runner.commands(&terminals);
//! # Ok(()) }
//! ```
//!
//! [`SessionId`]: pitcrew_protocol::ids::SessionId

#![forbid(unsafe_code)]

mod commands;
mod config;
mod derive;
mod fsinfo;
mod hooks;
mod link;
mod sink;
mod store;
mod store_sink;
mod terminals;
mod watch;

pub use commands::{CommandOptions, RunnerCommands};
pub use config::{EngineHome, PollMode, RunnerConfig, Timing};
pub use hooks::RunnerHooks;
pub use link::{Locations, MemoryLocations, WorkstreamLocation};
pub use sink::{EventSink, SinkError};
pub use store::StoreError;
pub use store_sink::StoreSink;
pub use terminals::{RunnerTerminals, TerminalOptions};

use pitcrew_interfaces::runtime::Runtime;
use pitcrew_interfaces::source::SourceAdapter;
use pitcrew_protocol::model::TimestampMs;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

/// The protocol version this crate was built against.
pub const PROTOCOL_VERSION: u32 = pitcrew_protocol::PROTOCOL_VERSION;

/// Errors starting the runner. Without file notifications (e.g. at the inotify instance limit)
/// the runner still starts, polling every home.
#[derive(Debug, thiserror::Error)]
pub enum RunnerError {
    /// The index could not be opened or read, or another runner holds it.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// A thread could not be started.
    #[error("runner thread: {0}")]
    Thread(#[from] std::io::Error),
}

/// A running watcher. Stops when dropped.
///
/// The hooks, terminals and commands it hands out keep working with the runner's index after it
/// stops (reported states are then ignored), and keep the index open: start a runner on the same
/// state directory again only once they are dropped.
#[derive(Debug)]
pub struct RunnerHandle {
    shared: Arc<watch::Shared>,
    store: Arc<Mutex<store::Store>>,
    homes: Vec<EngineHome>,
    stopping: Arc<AtomicBool>,
    watcher: Option<JoinHandle<()>>,
    dispatcher: Option<JoinHandle<()>>,
}

impl RunnerHandle {
    /// Runs discovery now, e.g. after the hub asks for a scan.
    pub fn rescan(&self) {
        self.shared.rescan();
    }

    /// Links every session to workstreams again; call it after the [`Locations`] changed.
    pub fn locations_changed(&self) {
        self.shared.relink();
    }

    /// The API's hook sink for this runner.
    #[must_use]
    pub fn hooks(&self) -> RunnerHooks {
        RunnerHooks::new(Arc::clone(&self.shared))
    }

    /// The API's terminals, over `runtime`, with default options.
    ///
    /// # Errors
    ///
    /// The threads for runtime calls cannot start.
    pub fn terminals(&self, runtime: Arc<dyn Runtime>) -> Result<RunnerTerminals, RunnerError> {
        self.terminals_with(runtime, TerminalOptions::default())
    }

    /// The API's terminals, over `runtime`.
    ///
    /// # Errors
    ///
    /// The threads for runtime calls cannot start.
    pub fn terminals_with(
        &self,
        runtime: Arc<dyn Runtime>,
        options: TerminalOptions,
    ) -> Result<RunnerTerminals, RunnerError> {
        Ok(RunnerTerminals::new(
            runtime,
            Arc::clone(&self.store),
            options,
        )?)
    }

    /// Runs hub commands in `terminals`, with default options.
    #[must_use]
    pub fn commands(&self, terminals: &RunnerTerminals) -> RunnerCommands {
        self.commands_with(terminals, CommandOptions::default())
    }

    /// Runs hub commands in `terminals`.
    #[must_use]
    pub fn commands_with(
        &self,
        terminals: &RunnerTerminals,
        options: CommandOptions,
    ) -> RunnerCommands {
        RunnerCommands::new(
            terminals.clone(),
            Arc::clone(&self.shared),
            self.homes.clone(),
            options,
        )
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
/// If the index cannot be opened or read (or another runner is using the state directory), or a
/// thread cannot start.
pub fn start(
    config: RunnerConfig,
    adapters: Vec<Arc<dyn SourceAdapter>>,
    sink: Arc<dyn EventSink>,
) -> Result<RunnerHandle, RunnerError> {
    let store = store::Store::open(&config.state_dir)?;
    // Starting with an empty index would give every transcript a new session id.
    let rows = store.load_all()?;
    let store = Arc::new(Mutex::new(store));
    let (tx, rx) = std::sync::mpsc::sync_channel(config.channel_capacity.max(1));
    let shared = Arc::new(watch::Shared::default());
    let stopping = Arc::new(AtomicBool::new(false));
    let retry_max = config.timing.sink_retry_max;
    let homes = config.homes.clone();

    let watcher = watch::Watcher::new(watch::Setup {
        workspace: config.workspace,
        machine: config.machine,
        owner: config.owner,
        timing: config.timing,
        poll: config.poll,
        max_batch: config.max_batch_events,
        homes: config.homes,
        adapters,
        rows,
        store: Arc::clone(&store),
        tx,
        shared: Arc::clone(&shared),
        locations: config.locations,
    });

    let dispatcher = {
        let stopping = Arc::clone(&stopping);
        let store = Arc::clone(&store);
        std::thread::Builder::new()
            .name("pitcrew-runner-sink".into())
            .spawn(move || sink::dispatch(&rx, &*sink, &store, &stopping, retry_max))?
    };
    let watcher = std::thread::Builder::new()
        .name("pitcrew-runner-watch".into())
        .spawn(move || watcher.run());
    let mut handle = RunnerHandle {
        shared,
        store,
        homes,
        stopping,
        watcher: None,
        dispatcher: Some(dispatcher),
    };
    // On failure the handle's drop stops the dispatcher.
    handle.watcher = Some(watcher?);
    Ok(handle)
}

/// Now, in ms since the epoch.
pub(crate) fn now_ms() -> TimestampMs {
    fsinfo::millis(std::time::SystemTime::now())
}
