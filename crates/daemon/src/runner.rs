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

use crate::agents::HubAgents;
use crate::cli::HomeArg;
use crate::state::StateDir;
use crate::terminals::NoRuntime;
use crate::transcripts::{Found, Recorded};
use anyhow::{Context as _, bail};
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
use std::sync::Arc;
use std::time::Duration;

/// The runner, running.
#[derive(Debug)]
pub struct Runner {
    handle: RunnerHandle,
    machine: MachineId,
    found: Arc<Found>,
}

impl Runner {
    /// The API's hook sink.
    pub fn hooks(&self) -> RunnerHooks {
        self.handle.hooks()
    }

    /// The runner's terminals, over [`NoRuntime`] until `crates/runtime` has a runtime.
    ///
    /// # Errors
    /// The threads for runtime calls cannot start.
    pub fn terminals(&self) -> anyhow::Result<RunnerTerminals> {
        self.handle
            .terminals(Arc::new(NoRuntime))
            .context("cannot start the runner's terminals")
    }

    /// The machine it runs on.
    pub fn machine(&self) -> MachineId {
        self.machine
    }

    /// The transcripts its discoveries found.
    pub fn found(&self) -> Arc<Found> {
        Arc::clone(&self.found)
    }

    /// Stops the watcher and waits for it, at most `within`: what it already read is still handed
    /// to the store first. Then it holds the store no more.
    pub async fn stop(self, within: Duration) {
        let handle = self.handle;
        let stopping = tokio::task::spawn_blocking(move || handle.stop());
        match tokio::time::timeout(within, stopping).await {
            Ok(Ok(())) => tracing::info!("the runner stopped"),
            Ok(Err(e)) => tracing::warn!(error = %e, "stopping the runner failed"),
            Err(_) => tracing::warn!(
                seconds = within.as_secs(),
                "the runner is still stopping; going on"
            ),
        }
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
/// The runner's index cannot be opened, or its threads cannot start.
pub fn start(
    state: &StateDir,
    work: &Arc<WorkService>,
    store: &Arc<Store>,
    machine: Option<MachineId>,
    homes: Vec<EngineHome>,
) -> anyhow::Result<Option<Runner>> {
    let Some(machine) = machine else {
        tracing::warn!(
            "the runner is off: the workspace has no local machine yet for its sessions to run on"
        );
        return Ok(None);
    };
    let Some(owner) = person(work)? else {
        tracing::warn!("the runner is off: the workspace has no person yet to own what it reports");
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
    config.homes = homes;
    if config.homes.is_empty() {
        tracing::info!(%machine, "the runner watches no home (--demo without --homes)");
    } else {
        let watched: Vec<String> = config
            .homes
            .iter()
            .map(|h| format!("{:?}={}", h.engine, h.path.display()))
            .collect();
        tracing::info!(%machine, homes = ?watched, "the runner watches these homes");
    }
    let sink = Arc::new(StoreSink::new(Arc::clone(store), owner));
    let handle = pitcrew_runner::start(config, adapters, sink).with_context(|| {
        format!(
            "cannot start the runner (its index is in {})",
            dir.display()
        )
    })?;
    Ok(Some(Runner {
        handle,
        machine,
        found,
    }))
}

/// The workspace's first person: they own this desktop, and author what the runner reports.
fn person(work: &WorkService) -> anyhow::Result<Option<MemberId>> {
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
