//! Runner configuration.

use crate::agents::SessionAgents;
use crate::link::Locations;
use pitcrew_protocol::ids::{MachineId, MemberId, WorkspaceId};
use pitcrew_protocol::model::Engine;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// One CLI's home on this machine, e.g. `~/.claude` or a `CLAUDE_CONFIG_DIR`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EngineHome {
    /// The CLI.
    pub engine: Engine,
    /// Its home folder, passed to the adapter's `discover`.
    pub path: PathBuf,
}

/// Whether to watch with filesystem notifications or by polling `stat`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PollMode {
    /// Poll homes on network filesystems (NFS, SMB, Lustre, 9p, …); notify elsewhere.
    #[default]
    Auto,
    /// Always poll, as for a network filesystem.
    Always,
    /// Never poll; always use notifications (unless they cannot be set up at all).
    Never,
}

/// Timings. The defaults suit a laptop; tests shorten them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Timing {
    /// Wait after the first change to a file before reading it, so a burst is one read.
    pub debounce: Duration,
    /// Transcripts modified within this window are hot: their folders are watched.
    pub hot_window: Duration,
    /// How often every transcript is re-checked by size and mtime (cold ones, and a safety net
    /// for missed notifications). Homes polled as network filesystems use four times this.
    pub cold_interval: Duration,
    /// How often the adapters' `discover` runs again, to find sessions in new folders. Homes
    /// polled as network filesystems use four times this.
    pub rediscover_interval: Duration,
    /// Fastest poll of a hot transcript on a polled filesystem.
    pub poll_min: Duration,
    /// Slowest poll of a hot transcript; the interval doubles up to this while nothing changes.
    pub poll_max: Duration,
    /// Longest wait between retries when the sink refuses a batch.
    pub sink_retry_max: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            debounce: Duration::from_millis(100),
            hot_window: Duration::from_secs(24 * 60 * 60),
            cold_interval: Duration::from_secs(30),
            rediscover_interval: Duration::from_secs(15),
            poll_min: Duration::from_millis(500),
            poll_max: Duration::from_secs(8),
            sink_retry_max: Duration::from_secs(5),
        }
    }
}

/// Everything the runner needs to start.
#[derive(Clone, Debug)]
pub struct RunnerConfig {
    /// The workspace events belong to.
    pub workspace: WorkspaceId,
    /// This machine.
    pub machine: MachineId,
    /// Authors events for unnamed sessions. The hub may re-stamp it.
    pub owner: MemberId,
    /// CLI homes to discover and watch.
    pub homes: Vec<EngineHome>,
    /// Where the runner keeps its SQLite index.
    pub state_dir: PathBuf,
    /// Notifications or polling.
    pub poll: PollMode,
    /// Timings.
    pub timing: Timing,
    /// Batches the watcher may queue for the sink before it waits (backpressure).
    pub channel_capacity: usize,
    /// Most events in one batch handed to the sink.
    pub max_batch_events: usize,
    /// Workstream locations to link sessions to. Without them, nothing is linked.
    pub locations: Option<Arc<dyn Locations>>,
    /// Who runs each session, to decide whose hooks may change it (see
    /// [`RunnerHooks`](crate::RunnerHooks)). Without it, every hook is refused.
    pub agents: Option<Arc<dyn SessionAgents>>,
    /// Cache layouts for concrete file adapters on notification-backed homes; on Unix this also
    /// checks quiet OpenCode databases and falls back when SQLite side files exist.
    /// Leave false for custom adapters whose discovery can change without directory changes.
    pub cache_file_discovery: bool,
    /// Concrete Claude/Codex JSONL adapters exhaust items when their byte cursor reaches EOF.
    /// Leave false for custom adapters and cursors that count logical items instead of bytes.
    pub byte_file_cursors: bool,
    /// Round notification deadlines up to this grid after debounce; zero disables grouping.
    /// Adds less than this duration. Polling and hook reports retain their deadlines.
    pub notification_window: Duration,
}

impl RunnerConfig {
    /// A configuration with no homes and default timings.
    #[must_use]
    pub fn new(
        workspace: WorkspaceId,
        machine: MachineId,
        owner: MemberId,
        state_dir: impl Into<PathBuf>,
    ) -> Self {
        Self {
            workspace,
            machine,
            owner,
            homes: Vec::new(),
            state_dir: state_dir.into(),
            poll: PollMode::Auto,
            timing: Timing::default(),
            channel_capacity: 64,
            max_batch_events: 256,
            locations: None,
            agents: None,
            cache_file_discovery: false,
            byte_file_cursors: false,
            notification_window: Duration::ZERO,
        }
    }

    /// Adds a CLI home.
    #[must_use]
    pub fn with_home(mut self, engine: Engine, path: impl Into<PathBuf>) -> Self {
        self.homes.push(EngineHome {
            engine,
            path: path.into(),
        });
        self
    }

    /// Links sessions to these workstream locations.
    #[must_use]
    pub fn with_locations(mut self, locations: Arc<dyn Locations>) -> Self {
        self.locations = Some(locations);
        self
    }

    /// Decides whose hooks may change a session by its agent, as `agents` tells it.
    #[must_use]
    pub fn with_agents(mut self, agents: Arc<dyn SessionAgents>) -> Self {
        self.agents = Some(agents);
        self
    }
}
