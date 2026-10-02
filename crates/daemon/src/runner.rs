//! The runner in this process (ADR-0009's solo case): it watches this machine's agent homes and
//! writes what the sessions do into the hub's store, takes the agents' hooks, and finds sessions'
//! terminals and transcripts.
//!
//! - **Homes** ([`homes`]): `--homes` when given; otherwise none with `--demo`, so a demo never
//!   shows the person's own sessions; otherwise this user's own, as each adapter finds them
//!   (`CLAUDE_CONFIG_DIR` or `~/.claude`, `CODEX_HOME` or `~/.codex`, `$XDG_DATA_HOME/opencode`
//!   or `~/.local/share/opencode`).
//! - **Events** go into the store through the runner's `StoreSink`, authored by the workspace's
//!   person (its first, whom the device token acts as).
//! - **Hooks**: who may change a session is decided by [`HubAgents`] over the hub's tables.
//! - **Its machine** is the hub's own (the workspace's first local machine). Without one, or
//!   without a person, the runner stays off (logged), as the back office does without a person.
//! - **Its index** is `runner/<log id>/` in the state directory: one per hub log, so a new store
//!   learns every session from the start instead of from cursors saved for another log.
//! - **A runner that cannot start** (its index cannot be opened or is locked, a thread cannot
//!   start) does not stop the hub: `serve` warns with the reason and the folder, and serves
//!   without it, as with no machine or no person.
//! - **When.** With the daemon, when the workspace has a person and a local machine; otherwise
//!   once it is set up (`crate::setup`), without a restart. The routes reach it through
//!   [`Attached`], which is empty until then.

use crate::agents::HubAgents;
use crate::cli::HomeArg;
use crate::state::StateDir;
use crate::terminals::NoRuntime;
use crate::transcripts::{Found, Recorded};
use anyhow::{Context as _, bail};
use pitcrew_api::{HookEvent, HookSink, LogHookSink};
use pitcrew_hub_work::WorkService;
use pitcrew_ingest::claude::ClaudeAdapter;
use pitcrew_ingest::codex::CodexAdapter;
use pitcrew_ingest::opencode::OpenCodeAdapter;
use pitcrew_interfaces::source::SourceAdapter;
use pitcrew_protocol::ids::{MachineId, MemberId};
use pitcrew_protocol::model::{Engine, MemberKind};
use pitcrew_runner::{
    EngineHome, RunnerConfig, RunnerHandle, RunnerHooks, RunnerTerminals, StoreSink,
};
use pitcrew_store::Store;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

/// What the routes use of a running runner.
#[derive(Clone, Debug)]
pub struct Parts {
    /// The machine it runs on.
    pub machine: MachineId,
    /// The API's hook sink.
    pub hooks: RunnerHooks,
    /// The runner's terminals, over [`NoRuntime`] until `crates/runtime` has a runtime.
    pub terminals: RunnerTerminals,
    /// The transcripts its discoveries found.
    pub found: Arc<Found>,
    /// Whether it watches at least one home (`GET /v1/host/info`'s `watch` capability).
    pub watches: bool,
}

/// The runner the routes reach, once there is one: set once, when it starts (with the daemon, or
/// once the workspace is set up), and never unset. Empty, the hub serves as with `--no-runner`.
#[derive(Debug, Default)]
pub struct Attached(OnceLock<Parts>);

impl Attached {
    /// One that already has `parts`.
    #[cfg(test)]
    pub fn with(parts: Parts) -> Self {
        let attached = Self::default();
        attached.set(parts);
        attached
    }

    /// The runner's parts, if it runs.
    pub fn get(&self) -> Option<&Parts> {
        self.0.get()
    }

    /// Attaches the runner. A second runner is not attached (there is one per process).
    pub fn set(&self, parts: Parts) {
        if self.0.set(parts).is_err() {
            tracing::warn!("a second runner was not attached");
        }
    }
}

/// The API's hook sink: the runner's once it runs; before that hooks are only logged (debug).
#[derive(Debug)]
pub struct Hooks(pub Arc<Attached>);

impl HookSink for Hooks {
    fn deliver(&self, event: HookEvent) {
        match self.0.get() {
            Some(runner) => runner.hooks.deliver(event),
            None => LogHookSink.deliver(event),
        }
    }
}

/// The runner, running. Stop it with [`Runner::stop`]; dropped without that (a start that
/// fails after the runner started), it is stopped on a thread of its own, so the drop never
/// waits for a watcher that does not answer.
#[derive(Debug)]
pub struct Runner {
    /// `None` once stopping.
    handle: Option<RunnerHandle>,
    parts: Parts,
}

impl Runner {
    /// What the routes use of it.
    pub fn parts(&self) -> Parts {
        self.parts.clone()
    }

    /// A real runner that watches no home and keeps what it reports nowhere, its index in
    /// `dir`.
    #[cfg(test)]
    pub fn idle(dir: &std::path::Path) -> Self {
        use pitcrew_protocol::events::Event;
        use pitcrew_protocol::ids::WorkspaceId;
        use pitcrew_runner::{EventSink, SinkError};

        #[derive(Debug)]
        struct Nowhere;
        impl EventSink for Nowhere {
            fn accept(&self, _events: &[Event]) -> Result<(), SinkError> {
                Ok(())
            }
        }

        let machine = MachineId::new();
        let config = RunnerConfig::new(WorkspaceId::new(), machine, MemberId::new(), dir);
        let handle = pitcrew_runner::start(config, Vec::new(), Arc::new(Nowhere))
            .expect("an idle runner starts");
        let terminals = handle
            .terminals(Arc::new(NoRuntime))
            .expect("its terminals start");
        Self {
            parts: Parts {
                machine,
                hooks: handle.hooks(),
                terminals,
                found: Arc::new(Found::default()),
                watches: false,
            },
            handle: Some(handle),
        }
    }

    /// Stops the watcher and waits for it, at most `within`: what it already read is still handed
    /// to the store first. Then it holds the store no more.
    ///
    /// A watcher that does not stop in time (stuck in a discovery or a read on a filesystem that
    /// does not answer) is left behind, still holding the store, and ends with the process: the
    /// stop runs on the blocking pool, which `serve` waits for only so long.
    pub async fn stop(mut self, within: Duration) {
        let Some(handle) = self.handle.take() else {
            return;
        };
        let stopping = tokio::task::spawn_blocking(move || handle.stop());
        match tokio::time::timeout(within, stopping).await {
            Ok(Ok(())) => tracing::info!("the runner stopped"),
            Ok(Err(e)) => tracing::warn!(error = %e, "stopping the runner failed"),
            Err(_) => tracing::warn!(
                seconds = within.as_secs(),
                "the runner is still stopping (a discovery or a read of a transcript has not \
                 returned); it is left behind and ends with the process"
            ),
        }
    }
}

impl Drop for Runner {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            stop_aside(handle);
        }
    }
}

/// Stops `handle` on a thread of its own, without waiting: a watcher stuck on a filesystem that
/// does not answer then holds up nothing, and ends with the process.
fn stop_aside(handle: RunnerHandle) {
    let stopping = std::thread::Builder::new()
        .name("pitcrewd-runner-stop".into())
        .spawn(move || handle.stop());
    if let Err(e) = stopping {
        // The handle went with the closure, and was stopped where it was dropped.
        tracing::warn!(error = %e, "cannot start a thread to stop the runner");
    }
}

/// The homes to watch: `given` (`--homes`) when any, else none with `demo`, else `defaults()`.
/// The same home is watched once.
pub fn homes(
    given: &[HomeArg],
    demo: bool,
    defaults: impl FnOnce() -> Vec<(Engine, PathBuf)>,
) -> Vec<EngineHome> {
    let all = if !given.is_empty() {
        given.iter().flat_map(HomeArg::homes).collect()
    } else if demo {
        Vec::new()
    } else {
        defaults()
    };
    let mut homes: Vec<EngineHome> = Vec::new();
    for (engine, path) in all {
        let home = EngineHome { engine, path };
        if !homes.contains(&home) {
            homes.push(home);
        }
    }
    homes
}

/// This user's own homes, as each adapter finds them (environment variables first).
pub fn default_homes() -> Vec<(Engine, PathBuf)> {
    pitcrew_ingest::scan::default_homes()
        .into_iter()
        .map(|h| (h.engine, h.home))
        .collect()
}

/// Starts the runner on `machine`, watching `homes`, writing to `store` through `work`'s
/// workspace. `None` when it cannot run here yet (no machine or no person; logged).
///
/// # Errors
/// The runner's index cannot be opened (its folder is in the error), or its threads cannot
/// start. `serve` then goes on without the runner.
pub fn start(
    state: &StateDir,
    work: &Arc<WorkService>,
    store: &Arc<Store>,
    machine: Option<MachineId>,
    homes: Vec<EngineHome>,
) -> anyhow::Result<Option<Runner>> {
    let Some(machine) = machine else {
        tracing::warn!(
            "the runner is off: the workspace has no local machine yet for its sessions to run \
             on; it starts once the workspace is set up"
        );
        return Ok(None);
    };
    let Some(owner) = person(work)? else {
        tracing::warn!(
            "the runner is off: the workspace has no person yet to own what it reports; it \
             starts once the workspace is set up"
        );
        return Ok(None);
    };
    let log = store.log_id();
    if log.is_empty() || !log.bytes().all(|b| b.is_ascii_alphanumeric()) {
        bail!("the store's log id {log:?} cannot name the runner's index folder");
    }
    let dir = state.runner().join(log);

    let found = Arc::new(Found::default());
    let adapters: Vec<Arc<dyn SourceAdapter>> = [
        Arc::new(ClaudeAdapter::new()) as Arc<dyn SourceAdapter>,
        Arc::new(CodexAdapter::new()),
        Arc::new(OpenCodeAdapter::new()),
    ]
    .into_iter()
    .map(|adapter| Arc::new(Recorded::new(adapter, Arc::clone(&found))) as Arc<dyn SourceAdapter>)
    .collect();

    let mut config = RunnerConfig::new(work.workspace(), machine, owner, &dir)
        .with_agents(Arc::new(HubAgents::new(Arc::clone(work))));
    let watched: Vec<String> = homes
        .iter()
        .map(|h| format!("{:?}={}", h.engine, h.path.display()))
        .collect();
    let watches = !homes.is_empty();
    config.homes = homes;
    let sink = Arc::new(StoreSink::new(Arc::clone(store), owner));
    let handle = pitcrew_runner::start(config, adapters, sink).with_context(|| {
        format!(
            "cannot start the runner with its index in {}",
            dir.display()
        )
    })?;
    let terminals = match handle.terminals(Arc::new(NoRuntime)) {
        Ok(terminals) => terminals,
        Err(e) => {
            stop_aside(handle);
            return Err(anyhow::Error::new(e).context("cannot start the runner's terminals"));
        }
    };
    if watches {
        tracing::info!(%machine, homes = ?watched, "the runner watches these homes");
    } else {
        tracing::info!(%machine, "the runner watches no home (--demo without --homes)");
    }
    Ok(Some(Runner {
        parts: Parts {
            machine,
            hooks: handle.hooks(),
            terminals,
            found,
            watches,
        },
        handle: Some(handle),
    }))
}

/// The workspace's first person: they own this desktop, author what the runner reports, and own
/// the back office's member. `None` before the workspace is set up.
pub fn person(work: &WorkService) -> anyhow::Result<Option<MemberId>> {
    Ok(work
        .members()
        .context("cannot list the members")?
        .into_iter()
        .find(|m| m.kind == MemberKind::Human)
        .map(|m| m.id))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home(engine: Engine, path: &str) -> EngineHome {
        EngineHome {
            engine,
            path: PathBuf::from(path),
        }
    }

    fn defaults() -> Vec<(Engine, PathBuf)> {
        vec![(Engine::Claude, PathBuf::from("/me/.claude"))]
    }

    /// A demo never watches the person's own homes: with `--demo` and no `--homes`, none.
    #[test]
    fn a_demo_watches_no_home_unless_given_some() {
        let never = || -> Vec<(Engine, PathBuf)> { panic!("the person's homes were looked up") };
        assert!(homes(&[], true, never).is_empty());

        let given = [
            HomeArg::Engine(Engine::Codex, PathBuf::from("/t/codex")),
            HomeArg::Root(PathBuf::from("/t/home")),
            HomeArg::Engine(Engine::Codex, PathBuf::from("/t/codex")),
        ];
        let expected = [
            home(Engine::Codex, "/t/codex"),
            home(Engine::Claude, "/t/home/.claude"),
            home(Engine::Codex, "/t/home/.codex"),
            home(Engine::OpenCode, "/t/home/.local/share/opencode"),
        ];
        assert_eq!(homes(&given, true, never), expected);
        assert_eq!(homes(&given, false, never), expected);
        // Without --demo and --homes, the person's own.
        assert_eq!(
            homes(&[], false, defaults),
            [home(Engine::Claude, "/me/.claude")]
        );
    }
}
