//! The first run: a fresh hub (no person, no machine, no name) set up once by `POST /v1/setup`
//! (api-v1.md, "The first run"), from the desktop's onboarding or `pitcrewd init`, and then
//! working as a hub that started with a person does, without a restart.
//!
//! - **[`Signal`]**, the work model's `SetupListener`, runs under hub-work's writer lock, right
//!   after the setup's append commits. So it does nothing but hand the `SetupDone` to
//!   [`after_setup`] over a channel: no call into the `WorkService`, no append, no I/O.
//! - **[`after_setup`]**, a task of the daemon, then (the lock released):
//!   1. writes `workspace.json` (the workspace's id and the name just set; atomic, private), so a
//!      restart keeps the name;
//!   2. names the hub's machine (`WorkService::set_hub_machine`), so a dispatch for a task with no
//!      folder may run here;
//!   3. unless `--no-office`, starts the back office as a start with a person would
//!      ([`crate::office::start`]): `@office` found or added through the one writer, its run log
//!      registered, its loop from the end of the log;
//!   4. unless `--no-runner`, starts the runner on the new machine, watching the homes the daemon
//!      was started with ([`crate::runner::start`]), and attaches it to the routes and host info.
//!
//!   A part that cannot start is logged and left off until the next start, which starts it as
//!   any start with a person does.
//! - **[`Workers`]** keeps the back office's loop and the runner, whether they started with the
//!   daemon or after setup, for the stop to stop. One started once the stop has begun is stopped
//!   at once.

use crate::office::{self, Running};
use crate::runner::{Attached, Runner};
use crate::runtime::TerminalRuntime;
use crate::state::{StateDir, write_workspace};
use pitcrew_hub_work::{SetupDone, SetupListener, WorkService};
use pitcrew_runner::EngineHome;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;
use tokio::sync::oneshot;

/// The work model's setup listener: hands the result to [`after_setup`], once.
#[derive(Debug)]
pub struct Signal(Mutex<Option<oneshot::Sender<SetupDone>>>);

/// A [`Signal`] and the receiving end for [`after_setup`].
pub fn signal() -> (Signal, oneshot::Receiver<SetupDone>) {
    let (sender, receiver) = oneshot::channel();
    (Signal(Mutex::new(Some(sender))), receiver)
}

impl SetupListener for Signal {
    /// Under hub-work's writer lock: only a send on a channel, which never blocks. Nothing waits
    /// for it when the hub was not fresh at start (setup then answers `409` and never calls this).
    fn set_up(&self, done: &SetupDone) {
        let sender = self.0.lock().unwrap_or_else(PoisonError::into_inner).take();
        if let Some(sender) = sender {
            let _ = sender.send(done.clone());
        }
    }
}

/// The back office's loop and the runner, kept for the stop.
#[derive(Debug, Default)]
pub struct Workers(Mutex<Kept>);

#[derive(Debug, Default)]
struct Kept {
    /// The stop has taken what was kept: nothing more is.
    stopping: bool,
    office: Option<Running>,
    runner: Option<Runner>,
}

impl Workers {
    fn kept(&self) -> MutexGuard<'_, Kept> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Keeps the back office's loop, to stop it with the daemon; gives it back (`Some`) once the
    /// stop has begun, to be stopped at once.
    #[must_use]
    pub fn keep_office(&self, office: Running) -> Option<Running> {
        let mut kept = self.kept();
        if kept.stopping {
            return Some(office);
        }
        kept.office = Some(office);
        None
    }

    /// Keeps the runner, as [`Workers::keep_office`] does the office.
    #[must_use]
    pub fn keep_runner(&self, runner: Runner) -> Option<Runner> {
        let mut kept = self.kept();
        if kept.stopping {
            return Some(runner);
        }
        kept.runner = Some(runner);
        None
    }

    /// Whether the stop has begun.
    pub fn stopping(&self) -> bool {
        self.kept().stopping
    }

    /// For the stop: what was kept. Nothing is kept from now on.
    pub fn take(&self) -> (Option<Running>, Option<Runner>) {
        let mut kept = self.kept();
        kept.stopping = true;
        (kept.office.take(), kept.runner.take())
    }
}

/// What the hub starts once it is set up.
#[derive(Debug)]
pub struct AfterSetup {
    pub state: StateDir,
    pub work: Arc<WorkService>,
    /// Whether the back office runs (not with `--no-office`).
    pub office: bool,
    /// The runner's homes; `None` with `--no-runner`.
    pub homes: Option<Vec<EngineHome>>,
    /// The runtime its terminals run on, chosen at start.
    pub runtime: TerminalRuntime,
    /// Where the runner is attached for the routes.
    pub attached: Arc<Attached>,
    pub workers: Arc<Workers>,
    /// How long a part started after the stop began gets to stop.
    pub drain: Duration,
}

/// Waits for the setup, then starts what needed a person (see the [module docs](self)). Returns
/// at once if the work model goes away without a setup.
pub async fn after_setup(set_up: oneshot::Receiver<SetupDone>, hub: AfterSetup) {
    let Ok(done) = set_up.await else {
        return;
    };
    tracing::info!(
        workspace = %done.workspace.id,
        name = ?done.workspace.name,
        person = %done.me.handle,
        machine = %done.machine.id,
        "the workspace is set up"
    );

    let path = hub.state.workspace();
    let workspace = done.workspace.clone();
    let written = tokio::task::spawn_blocking(move || {
        write_workspace(&path, &workspace).map_err(|e| (path, e))
    })
    .await;
    match written {
        Ok(Ok(())) => {}
        Ok(Err((path, e))) => tracing::error!(
            path = %path.display(),
            error = %e,
            "cannot keep the workspace's name; it is served until this daemon stops, and is \
             \"Workspace\" after a restart"
        ),
        Err(e) => tracing::error!(error = %e, "writing the workspace's name failed"),
    }
    hub.work.set_hub_machine(done.machine.id);

    if hub.workers.stopping() {
        tracing::info!(
            "the daemon is stopping: the back office and the runner start at the next start"
        );
        return;
    }
    if hub.office {
        start_office(&hub, &done).await;
    }
    if let Some(homes) = &hub.homes {
        start_runner(&hub, &done, homes.clone()).await;
    }
}

/// Step 3: the back office, as a start with a person would run it.
async fn start_office(hub: &AfterSetup, done: &SetupDone) {
    let (state, work, owner) = (hub.state.clone(), Arc::clone(&hub.work), done.me.id);
    let started =
        tokio::task::spawn_blocking(move || office::start(&state, &work, Some(owner), false)).await;
    match started {
        Ok(Ok(Some(office))) => {
            if let Some(running) = hub.workers.keep_office(office.spawn(Arc::clone(&hub.work))) {
                running.stop(hub.drain).await;
            }
        }
        // Logged: `@office` cannot be the back office.
        Ok(Ok(None)) => {}
        Ok(Err(e)) => {
            tracing::warn!("the back office cannot start now; it starts at the next start: {e:#}")
        }
        Err(e) => tracing::error!(error = %e, "starting the back office failed"),
    }
}

/// Step 4: the runner on the new machine, attached to the routes.
async fn start_runner(hub: &AfterSetup, done: &SetupDone, homes: Vec<EngineHome>) {
    let (state, work, machine) = (hub.state.clone(), Arc::clone(&hub.work), done.machine.id);
    let runtime = hub.runtime.clone();
    let started = tokio::task::spawn_blocking(move || {
        let store = Arc::clone(work.store());
        crate::runner::start(&state, &work, &store, Some(machine), homes, &runtime)
    })
    .await;
    match started {
        Ok(Ok(Some(runner))) => {
            let parts = runner.parts();
            match hub.workers.keep_runner(runner) {
                None => hub.attached.set(parts),
                Some(runner) => runner.stop(hub.drain).await,
            }
        }
        // Logged: no machine or no person, which setup has just added, so not expected.
        Ok(Ok(None)) => {}
        Ok(Err(e)) => tracing::warn!(
            "the runner cannot start, so this hub serves without it until the next start (no \
             session is watched, hooks are only logged): {e:#}"
        ),
        Err(e) => tracing::error!(error = %e, "starting the runner failed"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pitcrew_protocol::ids::{MachineId, MemberId, WorkspaceId};
    use pitcrew_protocol::model::{Liveness, Machine, MachineKind, Member, MemberKind, Workspace};

    fn done() -> SetupDone {
        SetupDone {
            workspace: Workspace {
                id: WorkspaceId::new(),
                name: "Lab".into(),
            },
            me: Member {
                id: MemberId::new(),
                kind: MemberKind::Human,
                handle: "@lee".into(),
                name: "Lee".into(),
                owner: None,
                persona: None,
            },
            machine: Machine {
                id: MachineId::new(),
                name: "PC".into(),
                kind: MachineKind::Local,
                info: None,
                liveness: Liveness::Live,
            },
        }
    }

    /// The listener hands the first setup over and ignores any later call; with nothing
    /// listening, it does nothing.
    #[test]
    fn the_signal_hands_the_setup_over_once() {
        let (signal, mut receiver) = signal();
        let first = done();
        signal.set_up(&first);
        signal.set_up(&done());
        assert_eq!(receiver.try_recv().unwrap(), first);

        let (signal, receiver) = super::signal();
        drop(receiver);
        signal.set_up(&first);
    }

    /// Kept until the stop begins, and taken by it; handed back once it has begun, so a setup
    /// that starts the office or the runner while the daemon stops stops them itself, at once.
    #[test]
    fn what_starts_once_the_stop_has_begun_is_handed_back() {
        let tmp = tempfile::tempdir().unwrap();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let within = Duration::from_secs(10);
        runtime.block_on(async {
            // Before the stop: kept, and the stop takes them.
            let workers = Workers::default();
            assert!(!workers.stopping());
            assert!(workers.keep_office(Running::idle()).is_none());
            assert!(
                workers
                    .keep_runner(Runner::idle(&tmp.path().join("before")))
                    .is_none()
            );
            let (office, runner) = workers.take();
            assert!(workers.stopping());
            office.expect("the office was kept").stop(within).await;
            runner.expect("the runner was kept").stop(within).await;

            // Once the stop has begun: handed back, and nothing is kept for it to take.
            let workers = Workers::default();
            let (office, runner) = workers.take();
            assert!(office.is_none() && runner.is_none());
            let office = workers
                .keep_office(Running::idle())
                .expect("the office is handed back");
            let runner = workers
                .keep_runner(Runner::idle(&tmp.path().join("after")))
                .expect("the runner is handed back");
            let (office_kept, runner_kept) = workers.take();
            assert!(office_kept.is_none(), "the office was kept after the stop");
            assert!(runner_kept.is_none(), "the runner was kept after the stop");
            office.stop(within).await;
            runner.stop(within).await;
        });
    }
}
