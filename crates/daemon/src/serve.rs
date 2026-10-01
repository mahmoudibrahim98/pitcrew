//! `pitcrewd serve`: opens the state, wires the hub, serves API v1, and shuts down cleanly.
//!
//! Start:
//! 1. The token registry, which locks the state directory: a second daemon stops here.
//! 2. The store with the work model's projections, to learn the workspace (`workspace.json` holds
//!    its name) and, with `--demo`, to refuse a store with data. Unless `--no-office`, also who
//!    the back office acts as, `@office` ([`crate::office::member`]); when it can run, the store
//!    is opened again with the office's run log as well, which needs that member. When it cannot,
//!    `office.json` is removed.
//! 3. The one `WorkService` for the store, with the hub's own machine (the workspace's local one).
//!    It has no dispatcher until the runner link exists, so a dispatch answers 503 and records
//!    nothing.
//! 4. With `--demo`: mint the tokens, seed the demo workspace.
//! 5. The device token: reused from `device.token` while it still verifies, else minted.
//! 6. The back office's loop, the routes (`RouterParts`, with the activity index), the listener,
//!    and one line on stdout: `pitcrewd listening on <where>`.
//!
//! Stop (Ctrl+C or Ctrl+Break, or SIGTERM or SIGHUP on Unix): the server stops accepting and
//! finishes in-flight requests (`pitcrew-api` closes open WebSockets with 1001) while the back
//! office finishes its run in progress and saves where it got to; then the store closes,
//! checkpointing its WAL, and the lock is released last.

use crate::cli::{ListenArg, ServeArgs};
use crate::no_runner::NoRunner;
use crate::office::Office;
use crate::refs::WorkRefs;
use crate::state::{StateDir, read_token, read_workspace, write_token, write_workspace};
use anyhow::{Context as _, bail};
use axum::Extension;
use pitcrew_api::{
    Activity, Bound, EventRefs, EventSource, HookIntake, Listen, LogHookSink, RouterParts,
    StoreSource, StreamConfig, TerminalConfig, Terminals,
};
use pitcrew_auth::{FileTokenStore, TokenError, TokenStore};
use pitcrew_fixtures::DemoWorkspace;
use pitcrew_hub_work::{BackOffice, WorkService};
use pitcrew_protocol::api::{Caller, HostRole, TokenScope};
use pitcrew_protocol::ids::{MemberId, WorkspaceId};
use pitcrew_protocol::model::{MachineKind, MemberKind, Workspace};
use pitcrew_store::{Projection, Store, StoreOptions};
use std::io::Write as _;
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Hook events queued for the sink before new ones are dropped.
const HOOK_QUEUE: usize = 1024;
/// How long the server gets to finish in-flight requests and close its sockets after a stop
/// signal.
const DRAIN: Duration = Duration::from_secs(10);
/// How long whatever still holds the store after the server stopped gets to let go of it.
const RELEASE: Duration = Duration::from_secs(3);

/// Runs `pitcrewd serve` until a stop signal.
///
/// # Errors
/// Anything that prevents starting: the state directory is in use or not private, the store
/// cannot be opened, `--demo` on a store with data, the listener cannot bind.
pub fn serve(state: &StateDir, args: &ServeArgs) -> anyhow::Result<ExitCode> {
    let started = Instant::now();
    #[cfg(not(unix))]
    if let ListenArg::Unix(path) = &args.listen {
        bail!(
            "--listen unix:{} is for Unix; here use `private` (the named pipe) or \
             `tcp:127.0.0.1:<port>`",
            path.display()
        );
    }
    let hub = open(state, args.demo, !args.no_office)?;
    let store = Arc::downgrade(&hub.store);
    // The lock goes last, so no other daemon opens the store while it closes.
    let lock = Arc::clone(&hub.tokens);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("pitcrewd")
        .build()
        .context("cannot start the async runtime")?;
    let served = runtime.block_on(run(hub, state, args.listen.clone(), started));
    // Stopping the runtime drops the tasks that still hold the store, such as streams the server
    // did not close; the store closes with the last of them.
    drop(runtime);
    if store.upgrade().is_none() {
        tracing::info!("store closed");
    } else {
        tracing::warn!("the store is still open at exit");
    }
    drop(lock);
    served?;
    tracing::info!("stopped");
    Ok(ExitCode::SUCCESS)
}

/// Everything opened before serving.
struct Hub {
    /// Holds the state directory's lock for as long as it lives.
    tokens: Arc<FileTokenStore>,
    store: Arc<Store>,
    work: Arc<WorkService>,
    /// The back office, unless it is off.
    office: Option<Office>,
}

/// Steps 1–5: the token registry, the store, the workspace and its service, the back office's
/// member, the demo, the device token. Without `office`, the back office does not run.
fn open(state: &StateDir, demo: bool, office: bool) -> anyhow::Result<Hub> {
    let tokens = match FileTokenStore::open(state.root()) {
        Ok(tokens) => Arc::new(tokens),
        Err(TokenError::Locked { path }) => bail!(
            "another pitcrewd is already running on {} (it holds {}); stop it, or pass another \
             --state-dir",
            state.root().display(),
            path.display()
        ),
        Err(e) => {
            return Err(e).with_context(|| {
                format!("cannot open the state directory {}", state.root().display())
            });
        }
    };

    let path = state.store();
    // The work model alone first: the back office's run log needs its member before the store
    // opens with it, and the member may have to be found or added in the store.
    let store = open_store(&path, pitcrew_hub_work::projections())?;
    let latest = store.latest_rev().context("cannot read the store")?;

    let demo = if demo {
        if latest > 0 {
            bail!(
                "--demo seeds only an empty store, and {} already holds {latest} events; start \
                 without --demo, or pass another --state-dir",
                path.display()
            );
        }
        Some(pitcrew_fixtures::demo_workspace().context("the demo workspace does not parse")?)
    } else {
        None
    };

    let workspace = hosted_workspace(state, &store, demo.as_ref())?;
    let member = if office {
        crate::office::member(&store, &workspace, demo.as_ref())?
    } else {
        tracing::info!("the back office is off (--no-office)");
        None
    };
    // Built once: its run log in the store and `run_office` must use the same settings.
    let back_office = member.map(|m| Arc::new(BackOffice::new(m)));
    let store = match &back_office {
        Some(back_office) => {
            // Closed first, so this process has one connection to the file again.
            drop(store);
            open_store(
                &path,
                pitcrew_hub_work::projections_with_office(back_office),
            )?
        }
        None => {
            // Whatever is appended while the office is off is never acted on later.
            crate::office::forget(state);
            store
        }
    };
    let store = Arc::new(store);

    // The one writer of this store (hub-work's "One writer"): everything shares this `Arc`.
    // No dispatcher until the runner link exists: a dispatch then answers 503 and records
    // nothing, rather than appending a dispatch that can only fail.
    let work = WorkService::new(Arc::clone(&store), workspace);
    let machines = match &demo {
        Some(demo) => demo.machines.clone(),
        None => work.machines().context("cannot list the machines")?,
    };
    let work = Arc::new(
        match machines.iter().find(|m| m.kind == MachineKind::Local) {
            Some(machine) => {
                tracing::info!(machine = %machine.id, name = %machine.name, "the hub's own machine");
                work.with_hub_machine(machine.id)
            }
            None => {
                tracing::warn!(
                    "the workspace has no local machine, so a dispatch for a task without a \
                     folder answers 503 until the runner adds this one"
                );
                work
            }
        },
    );

    // Tokens before seeding: if minting fails, the store stays empty and `--demo` can be retried.
    match &demo {
        Some(demo) => {
            let person = demo_person(demo)?;
            device_token(state, &*tokens, Some(person), || Ok(person))?;
            demo_agent_token(state, &*tokens, demo, person)?;
            let seeded = work.seed(demo).context("cannot seed the demo workspace")?;
            tracing::info!(
                workspace = %demo.workspace.id,
                events = seeded.len(),
                "seeded the demo workspace"
            );
        }
        None => {
            device_token(state, &*tokens, None, || first_person(&work))?;
        }
    }
    let office = back_office
        .map(|back_office| Office::new(back_office, state, &store, demo.is_some()))
        .transpose()?;
    Ok(Hub {
        tokens,
        store,
        work,
        office,
    })
}

/// Opens the store at `path` with `projections`.
fn open_store(path: &Path, projections: Vec<Box<dyn Projection>>) -> anyhow::Result<Store> {
    Store::open_with(path, StoreOptions::default(), projections)
        .with_context(|| format!("cannot open the store {}", path.display()))
}

/// What a workspace is called when its name is not known.
const UNNAMED: &str = "Workspace";

/// The workspace this hub hosts. With `--demo`, the demo's, written to `workspace.json` (before
/// seeding, so a retried `--demo` writes it again). Otherwise the one the store's events belong
/// to, named by `workspace.json` when that file names the same workspace; the event log does not
/// hold names.
fn hosted_workspace(
    state: &StateDir,
    store: &Store,
    demo: Option<&DemoWorkspace>,
) -> anyhow::Result<Workspace> {
    let path = state.workspace();
    if let Some(demo) = demo {
        write_workspace(&path, &demo.workspace)
            .with_context(|| format!("cannot write {}", path.display()))?;
        return Ok(demo.workspace.clone());
    }
    let saved = read_workspace(&path).with_context(|| format!("cannot read {}", path.display()))?;
    let logged = store
        .since(0, 1)
        .context("cannot read the store")?
        .first()
        .map(|first| first.event.workspace);
    Ok(match (logged, saved) {
        (Some(id), Some(saved)) if saved.id == id => saved,
        (Some(id), saved) => {
            tracing::warn!(
                workspace = %id,
                file = %path.display(),
                names_another = saved.is_some(),
                "the workspace's name is not known; calling it {UNNAMED:?}"
            );
            Workspace {
                id,
                name: UNNAMED.to_owned(),
            }
        }
        (None, Some(saved)) => saved,
        (None, None) => {
            tracing::warn!(
                "the store is empty and there is no way to create a workspace yet; start with \
                 --demo to try the demo workspace"
            );
            Workspace {
                id: WorkspaceId::new(),
                name: UNNAMED.to_owned(),
            }
        }
    })
}

/// The device token in `device.token`: reused while it verifies as a device token (of `member`,
/// when given), else minted for `member` or `owner()` and written there.
fn device_token(
    state: &StateDir,
    tokens: &dyn TokenStore,
    member: Option<MemberId>,
    owner: impl FnOnce() -> anyhow::Result<MemberId>,
) -> anyhow::Result<Caller> {
    let path = state.device_token();
    let existing = read_token(&path)
        .with_context(|| format!("cannot read the device token {}", path.display()))?;
    if let Some(raw) = existing {
        match tokens.verify(&raw) {
            Some(caller)
                if caller.scope == TokenScope::Device
                    && member.is_none_or(|m| m == caller.member) =>
            {
                tracing::info!(path = %path.display(), member = %caller.member, "using the device token");
                return Ok(caller);
            }
            _ => tracing::warn!(
                path = %path.display(),
                "the device token there is unknown, revoked or someone else's; minting a new one"
            ),
        }
    }
    let caller = Caller {
        member: member.map_or_else(owner, Ok)?,
        scope: TokenScope::Device,
        on_behalf_of: None,
    };
    mint_into(tokens, caller, &path)?;
    Ok(caller)
}

/// With `--demo`: a token for the demo person's first agent (`@writer`), like the mock hub's
/// `dev-agent-token`.
fn demo_agent_token(
    state: &StateDir,
    tokens: &dyn TokenStore,
    demo: &DemoWorkspace,
    person: MemberId,
) -> anyhow::Result<()> {
    let Some(agent) = demo
        .members
        .iter()
        .find(|m| m.kind == MemberKind::Agent && m.owner == Some(person))
    else {
        return Ok(());
    };
    let caller = Caller {
        member: agent.id,
        scope: TokenScope::Agent,
        on_behalf_of: Some(person),
    };
    mint_into(tokens, caller, &state.demo_agent_token())?;
    tracing::info!(agent = %agent.handle, "the demo agent's token is in {}", state.demo_agent_token().display());
    Ok(())
}

/// Mints a token for `caller` and writes it to `path`. Only its id and path are logged.
fn mint_into(tokens: &dyn TokenStore, caller: Caller, path: &Path) -> anyhow::Result<()> {
    let (info, token) = tokens.mint(caller).context("cannot mint a token")?;
    write_token(path, &token).with_context(|| format!("cannot write {}", path.display()))?;
    tracing::info!(token = %info.id, path = %path.display(), "wrote the token's file");
    Ok(())
}

/// The demo's person: the device token acts as them, as `dev-device-token` does on the mock hub.
fn demo_person(demo: &DemoWorkspace) -> anyhow::Result<MemberId> {
    demo.members
        .iter()
        .find(|m| m.kind == MemberKind::Human)
        .map(|m| m.id)
        .context("the demo workspace has no person")
}

/// The workspace's first person, who owns this desktop in the solo case. A store without one gets
/// a new member id, which nothing knows yet.
fn first_person(work: &WorkService) -> anyhow::Result<MemberId> {
    let members = work.members().context("cannot list the members")?;
    Ok(match members.iter().find(|m| m.kind == MemberKind::Human) {
        Some(person) => person.id,
        None => {
            tracing::warn!(
                "the workspace has no person yet; the device token acts as a new member that \
                 nothing knows, so GET /v1/me answers 404"
            );
            MemberId::new()
        }
    })
}

/// Step 5 onwards: serve until a stop signal, then shut down in order.
async fn run(
    hub: Hub,
    state: &StateDir,
    listen: ListenArg,
    started: Instant,
) -> anyhow::Result<()> {
    let Hub {
        tokens,
        store,
        work,
        office,
    } = hub;
    let mut stop = Stop::listen().context("cannot listen for stop signals")?;
    let office_work = Arc::clone(&work);

    let events: Arc<dyn EventSource> =
        Arc::new(StoreSource::new(Arc::clone(&store), store.log_id()));
    let hooks = HookIntake::start(Arc::new(LogHookSink), HOOK_QUEUE)
        .context("cannot start the hook intake")?;
    let terminals: Arc<dyn Terminals> = Arc::new(NoRunner::new(Arc::clone(&work)));
    // The activity index (`project=`, `workstream=`, and wider `task=` and `session=` matches).
    let refs: Arc<dyn EventRefs> = Arc::new(WorkRefs(Arc::clone(&work)));
    let parts = RouterParts::new()
        .agent(pitcrew_api::hooks::routes(hooks))
        .agent(pitcrew_hub_work::agent_routes().layer(Extension(Arc::clone(&work))))
        .device(pitcrew_api::stream::routes(
            Arc::clone(&events),
            StreamConfig::default(),
        ))
        .device(Activity::new(events).with_refs(refs).routes())
        .device(pitcrew_api::terminal::routes(
            terminals,
            TerminalConfig::default(),
        ))
        .device(pitcrew_hub_work::device_routes().layer(Extension(work)));
    let info = pitcrew_api::local_host_info(env!("CARGO_PKG_VERSION"), vec![HostRole::Hub], vec![]);

    let (listen, dev) = match listen {
        ListenArg::Private => (
            Listen::private_default(state.run_dir())
                .context("cannot pick the private transport")?,
            false,
        ),
        // `pitcrew-api` binds `pitcrewd.sock` (the file name `unix:` requires) in this directory,
        // which it makes private, and removes it when the server stops.
        ListenArg::Unix(path) => {
            let path = std::path::absolute(&path)
                .with_context(|| format!("cannot resolve the socket path {}", path.display()))?;
            let dir = path
                .parent()
                .with_context(|| format!("{} has no directory", path.display()))?
                .to_path_buf();
            (Listen::Unix { dir }, false)
        }
        ListenArg::Tcp(addr) => (Listen::DevTcp { addr }, true),
    };
    let bound = Bound::bind(&listen)
        .await
        .with_context(|| format!("cannot listen on {}", describe(&listen)))?;
    let at = bound.describe();
    let token_store: Arc<dyn TokenStore> = Arc::clone(&tokens) as Arc<dyn TokenStore>;
    let mut app = pitcrew_api::router(info, token_store, parts);
    if dev {
        app = app.layer(axum::middleware::from_fn(crate::cors::cors));
    }
    // The back office subscribes to the store's appends before anything is served (and its first
    // run covers whatever was appended before).
    let office = office.map(|office| office.spawn(office_work));
    let (draining, drain) = tokio::sync::oneshot::channel::<()>();
    let mut serving = tokio::spawn(bound.serve(app, async move {
        let _ = drain.await;
    }));

    tracing::info!(
        %at,
        state = %state.root().display(),
        ms = started.elapsed().as_millis(),
        "ready"
    );
    // Supervisors and tests wait for this line.
    let mut stdout = std::io::stdout().lock();
    writeln!(stdout, "pitcrewd listening on {at}").context("cannot write to stdout")?;
    stdout.flush().context("cannot write to stdout")?;
    drop(stdout);

    let failed = tokio::select! {
        signal = stop.next() => {
            tracing::info!(%signal, "stopping");
            None
        }
        ended = &mut serving => Some(match ended {
            Ok(Ok(())) => anyhow::anyhow!("the server stopped on its own"),
            Ok(Err(e)) => anyhow::Error::new(e).context("the server failed"),
            Err(e) => anyhow::Error::new(e).context("the server task failed"),
        }),
    };

    // The server stops accepting, finishes in-flight requests and closes its WebSockets, while the
    // back office finishes its run in progress; both let go of the store before it closes.
    let _ = draining.send(());
    let office_stopped = async {
        if let Some(office) = office {
            office.stop(DRAIN).await;
        }
    };
    if failed.is_some() {
        office_stopped.await;
    } else {
        tokio::join!(office_stopped, finish(&mut serving));
    }
    drop(tokens);
    close_store(store).await;
    failed.map_or(Ok(()), Err)
}

/// Waits for the server to finish, at most [`DRAIN`].
async fn finish(serving: &mut tokio::task::JoinHandle<std::io::Result<()>>) {
    match tokio::time::timeout(DRAIN, &mut *serving).await {
        Ok(Ok(Ok(()))) => {}
        Ok(Ok(Err(e))) => tracing::warn!(error = %e, "the server failed while stopping"),
        Ok(Err(e)) => tracing::warn!(error = %e, "the server task failed while stopping"),
        Err(_) => {
            tracing::warn!(
                seconds = DRAIN.as_secs(),
                "requests still running; stopping anyway"
            );
            serving.abort();
        }
    }
}

/// Waits briefly for everything else (a read still on the blocking pool, a socket still closing)
/// to let go of the store, then closes it: the writer connection closes last and checkpoints the
/// WAL, so only `hub.db` remains. If something still holds it, it closes with that holder when
/// the runtime stops.
async fn close_store(store: Arc<Store>) {
    let deadline = Instant::now() + RELEASE;
    let mut store = store;
    loop {
        match Arc::try_unwrap(store) {
            Ok(unique) => {
                drop(unique);
                return;
            }
            Err(shared) if Instant::now() >= deadline => {
                tracing::warn!(
                    holders = Arc::strong_count(&shared) - 1,
                    "the store is still in use after the server stopped; closing it with the \
                     runtime"
                );
                return;
            }
            Err(shared) => {
                store = shared;
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
    }
}

fn describe(listen: &Listen) -> String {
    match listen {
        Listen::Unix { dir } => format!("the private socket in {}", dir.display()),
        Listen::Pipe { name } => format!("the named pipe {name}"),
        Listen::DevTcp { addr } => format!("tcp:{addr}"),
    }
}

/// The stop signals: Ctrl+C everywhere; SIGTERM and SIGHUP on Unix (closing a tmux pane or an
/// SSH session hangs up, which would otherwise end the process without removing its socket); and
/// Ctrl+Break and closing the console on Windows. Registered at start, so a signal right after
/// the ready line is not missed.
struct Stop {
    #[cfg(unix)]
    interrupt: tokio::signal::unix::Signal,
    #[cfg(unix)]
    terminate: tokio::signal::unix::Signal,
    #[cfg(unix)]
    hangup: tokio::signal::unix::Signal,
    #[cfg(windows)]
    ctrl_c: tokio::signal::windows::CtrlC,
    #[cfg(windows)]
    ctrl_break: tokio::signal::windows::CtrlBreak,
    #[cfg(windows)]
    ctrl_close: tokio::signal::windows::CtrlClose,
}

impl Stop {
    fn listen() -> std::io::Result<Self> {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{SignalKind, signal};
            Ok(Self {
                interrupt: signal(SignalKind::interrupt())?,
                terminate: signal(SignalKind::terminate())?,
                hangup: signal(SignalKind::hangup())?,
            })
        }
        #[cfg(windows)]
        {
            use tokio::signal::windows;
            Ok(Self {
                ctrl_c: windows::ctrl_c()?,
                ctrl_break: windows::ctrl_break()?,
                ctrl_close: windows::ctrl_close()?,
            })
        }
    }

    /// The next stop signal's name.
    async fn next(&mut self) -> &'static str {
        #[cfg(unix)]
        {
            tokio::select! {
                _ = self.interrupt.recv() => "SIGINT",
                _ = self.terminate.recv() => "SIGTERM",
                _ = self.hangup.recv() => "SIGHUP",
            }
        }
        #[cfg(windows)]
        {
            tokio::select! {
                _ = self.ctrl_c.recv() => "Ctrl+C",
                _ = self.ctrl_break.recv() => "Ctrl+Break",
                _ = self.ctrl_close.recv() => "console closed",
            }
        }
    }
}

impl std::fmt::Debug for Stop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Stop").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::ServeArgs;
    use pitcrew_protocol::events::{Event, EventBody};

    fn state() -> (tempfile::TempDir, StateDir) {
        let tmp = tempfile::tempdir().unwrap();
        let state = StateDir::resolve(Some(tmp.path().join("state"))).unwrap();
        (tmp, state)
    }

    /// The demo's `@office`.
    const OFFICE: &str = "01JB000000000000000MEM0006";

    #[test]
    fn demo_mints_tokens_for_sam_and_writer_and_seeds() {
        let (_tmp, state) = state();
        let hub = open(&state, true, true).unwrap();
        let raw = read_token(&state.device_token()).unwrap().unwrap();
        let device = hub.tokens.verify(&raw).unwrap();
        let demo = pitcrew_fixtures::demo_workspace().unwrap();
        assert_eq!(device.member, demo_person(&demo).unwrap());
        assert_eq!(device.scope, TokenScope::Device);

        let raw = read_token(&state.demo_agent_token()).unwrap().unwrap();
        let agent = hub.tokens.verify(&raw).unwrap();
        assert_eq!(agent.scope, TokenScope::Agent);
        assert_eq!(agent.on_behalf_of, Some(device.member));
        let writer = hub.work.member(&agent.member).unwrap();
        assert_eq!(writer.handle, "@writer");

        assert_eq!(hub.work.workspace(), demo.workspace.id);
        assert_eq!(
            hub.work.tasks(&Default::default()).unwrap().len(),
            demo.tasks.len()
        );

        // The back office is the demo's @office, and looks at the seed too.
        let office = hub.office.as_ref().unwrap();
        assert_eq!(office.member(), OFFICE.parse().unwrap());
        assert_eq!(office.last(), 0);
    }

    #[test]
    fn a_restart_keeps_the_workspace_and_the_device_token() {
        let (_tmp, state) = state();
        let (workspace, raw) = {
            let hub = open(&state, true, true).unwrap();
            (
                hub.work.workspace(),
                read_token(&state.device_token()).unwrap(),
            )
        };
        let hub = open(&state, false, true).unwrap();
        assert_eq!(hub.work.workspace(), workspace);
        let demo = pitcrew_fixtures::demo_workspace().unwrap();
        assert_eq!(
            hub.work.workspace_at().unwrap().workspace,
            demo.workspace,
            "the name survives a restart"
        );
        assert_eq!(read_token(&state.device_token()).unwrap(), raw);
        let raw = raw.unwrap();
        assert_eq!(hub.tokens.verify(&raw).unwrap().scope, TokenScope::Device);
        drop(hub);

        // Without the file, the workspace is still the store's, unnamed.
        std::fs::remove_file(state.workspace()).unwrap();
        let hub = open(&state, false, true).unwrap();
        let named = hub.work.workspace_at().unwrap().workspace;
        assert_eq!(named.id, workspace);
        assert_eq!(named.name, UNNAMED);
    }

    #[test]
    fn demo_refuses_a_store_with_data() {
        let (_tmp, state) = state();
        drop(open(&state, true, true).unwrap());
        let err = open(&state, true, true).err().unwrap();
        assert!(
            format!("{err:#}").contains("only an empty store"),
            "{err:#}"
        );
    }

    #[test]
    fn one_daemon_per_state_dir() {
        let (_tmp, state) = state();
        let _first = open(&state, false, true).unwrap();
        let err = open(&state, false, true).err().unwrap();
        assert!(format!("{err:#}").contains("already running"), "{err:#}");
    }

    #[test]
    fn a_lost_token_registry_mints_a_new_device_token() {
        let (_tmp, state) = state();
        let old = {
            let _hub = open(&state, true, true).unwrap();
            read_token(&state.device_token()).unwrap().unwrap()
        };
        std::fs::remove_file(state.root().join(FileTokenStore::FILE_NAME)).unwrap();
        let hub = open(&state, false, true).unwrap();
        let new = read_token(&state.device_token()).unwrap().unwrap();
        assert_ne!(new, old);
        let caller = hub.tokens.verify(&new).unwrap();
        // The workspace's person, found in the work model.
        let demo = pitcrew_fixtures::demo_workspace().unwrap();
        assert_eq!(caller.member, demo_person(&demo).unwrap());
        assert_eq!(hub.tokens.verify(&old), None);
    }

    #[test]
    fn serve_args_default_to_the_private_transport() {
        use clap::Parser as _;
        let cli = crate::cli::Cli::try_parse_from(["pitcrewd", "serve"]).unwrap();
        let Some(crate::cli::Command::Serve(ServeArgs {
            listen,
            demo,
            no_office,
        })) = cli.command
        else {
            panic!("not serve");
        };
        assert_eq!(listen, ListenArg::Private);
        assert!(!demo);
        assert!(!no_office);
    }

    /// Appends `bodies` to the hub's store directly, as `author` (on behalf of `owner`), as a
    /// runner link or another writer would.
    fn append(
        store: &Store,
        workspace: WorkspaceId,
        author: MemberId,
        owner: Option<MemberId>,
        bodies: Vec<EventBody>,
    ) -> pitcrew_store::RevRange {
        let events: Vec<Event> = bodies
            .into_iter()
            .map(|body| Event {
                on_behalf_of: owner,
                ..Event::now(workspace, author, body)
            })
            .collect();
        store.append(&events).unwrap()
    }

    fn members_called(work: &WorkService, handle: &str) -> Vec<pitcrew_protocol::model::Member> {
        work.members()
            .unwrap()
            .into_iter()
            .filter(|m| m.handle == handle)
            .collect()
    }

    #[test]
    fn a_workspace_without_office_gets_one_owned_by_its_person_once() {
        let (_tmp, state) = state();
        // An empty store: no person to own the office, so it is off.
        let hub = open(&state, false, true).unwrap();
        assert!(hub.office.is_none());
        assert!(hub.work.members().unwrap().is_empty());
        let workspace = hub.work.workspace();
        let person = pitcrew_protocol::model::Member {
            id: MemberId::new(),
            kind: MemberKind::Human,
            handle: "@lee".into(),
            name: "Lee".into(),
            owner: None,
            persona: None,
        };
        append(
            &hub.store,
            workspace,
            person.id,
            None,
            vec![EventBody::MemberAdded {
                member: person.clone(),
            }],
        );
        drop(hub);

        // The first start with a person adds @office, owned by them.
        let hub = open(&state, false, true).unwrap();
        let office = hub.office.as_ref().unwrap().member();
        let found = members_called(&hub.work, crate::office::HANDLE);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, office);
        assert_eq!(found[0].kind, MemberKind::Agent);
        assert_eq!(found[0].owner, Some(person.id));
        let latest = hub.store.latest_rev().unwrap();
        assert_eq!(latest, 2, "one member_added for the office");
        // Without office.json, it starts at the end of the log.
        assert_eq!(hub.office.as_ref().unwrap().last(), latest);
        drop(hub);

        // A restart reuses it.
        let hub = open(&state, false, true).unwrap();
        assert_eq!(hub.office.as_ref().unwrap().member(), office);
        assert_eq!(hub.store.latest_rev().unwrap(), latest);
        drop(hub);

        // With the office off, nothing is added, and its progress is forgotten.
        crate::state::write_json(
            &state.office(),
            &serde_json::json!({ "log": "x", "done": 1 }),
        )
        .unwrap();
        let hub = open(&state, false, false).unwrap();
        assert!(hub.office.is_none());
        assert!(!state.office().exists());
        assert_eq!(hub.store.latest_rev().unwrap(), latest);
    }

    /// The loop over the demo: a finished dispatch moves its task to review as @office, and a
    /// restart that runs the range again (as after a crash before `office.json` was saved)
    /// appends nothing twice.
    #[test]
    fn the_office_loop_acts_once_across_a_restart() {
        use pitcrew_hub_work::TaskRef;
        use pitcrew_protocol::model::{DispatchOutcome, Mover, TaskStatus};
        let (_tmp, state) = state();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let demo = pitcrew_fixtures::demo_workspace().unwrap();
        let sam = demo_person(&demo).unwrap();
        let writer: MemberId = "01JB000000000000000MEM0002".parse().unwrap();
        let office: MemberId = OFFICE.parse().unwrap();
        let pap1 = TaskRef::parse("PAP-1").unwrap();
        let status = |work: &WorkService| work.task(&pap1).unwrap().status;
        let office_moves = |work: &WorkService| {
            work.store()
                .since(0, usize::MAX)
                .unwrap()
                .into_iter()
                .filter(|e| {
                    e.event.author == office && matches!(e.event.body, EventBody::TaskMoved { .. })
                })
                .count()
        };

        let Hub {
            tokens,
            store,
            work,
            office: back_office,
        } = open(&state, true, true).unwrap();
        let finished = runtime.block_on(async {
            let running = back_office.unwrap().spawn(Arc::clone(&work));
            assert_eq!(status(&work), TaskStatus::InProgress);
            let finished = append(
                work.store(),
                work.workspace(),
                writer,
                Some(sam),
                vec![EventBody::DispatchFinished {
                    dispatch: "01JB000000000000000DSP0001".parse().unwrap(),
                    outcome: DispatchOutcome::Succeeded,
                    summary: Some("§3.2 is drafted.".into()),
                }],
            );
            let deadline = Instant::now() + Duration::from_secs(20);
            while status(&work) != TaskStatus::Review {
                assert!(Instant::now() < deadline, "PAP-1 never moved to review");
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            // Let the office look at what it appended, then stop it.
            tokio::time::sleep(Duration::from_millis(300)).await;
            running.stop(Duration::from_secs(20)).await;
            finished
        });
        let moved: Vec<Event> = work
            .store()
            .since(finished.to_rev, usize::MAX)
            .unwrap()
            .into_iter()
            .map(|e| e.event)
            .filter(|e| matches!(e.body, EventBody::TaskMoved { .. }))
            .collect();
        assert_eq!(moved.len(), 1, "{moved:?}");
        assert_eq!(moved[0].author, office);
        assert_eq!(moved[0].on_behalf_of, Some(sam));
        assert!(matches!(
            moved[0].body,
            EventBody::TaskMoved {
                to: TaskStatus::Review,
                mover: Mover::BackOffice { .. },
                ..
            }
        ));
        let latest = work.store().latest_rev().unwrap();
        let log = work.store().log_id().to_owned();
        let saved: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(state.office()).unwrap()).unwrap();
        assert_eq!(saved["log"], log.as_str());
        assert!(saved["done"].as_u64().unwrap() >= finished.to_rev);
        drop((work, store, tokens));

        // As if it had crashed before saving: the range with the finished dispatch runs again.
        crate::state::write_json(
            &state.office(),
            &serde_json::json!({ "log": log, "done": finished.from_rev - 1 }),
        )
        .unwrap();
        let Hub {
            tokens: _tokens,
            store: _store,
            work,
            office: back_office,
        } = open(&state, false, true).unwrap();
        let back_office = back_office.unwrap();
        assert_eq!(back_office.last(), finished.from_rev - 1);
        runtime.block_on(async {
            // The first run covers the range before the loop looks for a stop.
            let running = back_office.spawn(Arc::clone(&work));
            running.stop(Duration::from_secs(20)).await;
        });
        assert_eq!(work.store().latest_rev().unwrap(), latest, "appended again");
        assert_eq!(office_moves(&work), 1);
        assert_eq!(status(&work), TaskStatus::Review);
        let saved: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(state.office()).unwrap()).unwrap();
        assert_eq!(saved["done"], latest);
    }

    /// `office.json` as JSON, if it is a file.
    fn progress(state: &StateDir) -> Option<serde_json::Value> {
        let text = std::fs::read_to_string(state.office()).ok()?;
        serde_json::from_str(&text).ok()
    }

    /// Where the office starts is saved before the loop runs: a `--demo` start that fails after
    /// seeding (say its listener cannot bind) still gets its pass over the seed next time.
    #[test]
    fn the_start_point_is_saved_before_the_loop_runs() {
        let (_tmp, state) = state();
        let hub = open(&state, true, true).unwrap();
        let log = hub.store.log_id().to_owned();
        assert_eq!(
            progress(&state),
            Some(serde_json::json!({ "log": log, "done": 0 }))
        );
        // The loop never ran.
        drop(hub);
        let hub = open(&state, false, true).unwrap();
        assert_eq!(hub.office.as_ref().unwrap().last(), 0);
    }

    /// A workspace whose `@office` is not an agent of its person keeps the office off: the members
    /// are `members`, added by the first.
    fn office_stays_off_with(members: &[pitcrew_protocol::model::Member]) {
        let (_tmp, state) = state();
        let hub = open(&state, false, true).unwrap();
        let workspace = hub.work.workspace();
        let bodies = members
            .iter()
            .map(|m| EventBody::MemberAdded { member: m.clone() })
            .collect();
        let added = append(&hub.store, workspace, members[0].id, None, bodies);
        drop(hub);
        crate::state::write_json(
            &state.office(),
            &serde_json::json!({ "log": "x", "done": 1 }),
        )
        .unwrap();

        let hub = open(&state, false, true).unwrap();
        assert!(hub.office.is_none(), "{members:?}");
        assert_eq!(
            hub.store.latest_rev().unwrap(),
            added.to_rev,
            "no member was added"
        );
        assert_eq!(members_called(&hub.work, crate::office::HANDLE).len(), 1);
        assert!(!state.office().exists(), "the office is off: no progress");
    }

    #[test]
    fn an_office_that_is_a_person_or_not_the_persons_agent_stays_off() {
        use pitcrew_protocol::model::Member;
        let person = |handle: &str| Member {
            id: MemberId::new(),
            kind: MemberKind::Human,
            handle: handle.to_owned(),
            name: handle.trim_start_matches('@').to_owned(),
            owner: None,
            persona: None,
        };
        let agent = |owner: Option<MemberId>| Member {
            kind: MemberKind::Agent,
            owner,
            ..person(crate::office::HANDLE)
        };
        let (lee, kim) = (person("@lee"), person("@kim"));
        // A person holds the handle.
        office_stays_off_with(&[lee.clone(), person(crate::office::HANDLE)]);
        // Another person's agent.
        office_stays_off_with(&[lee.clone(), kim.clone(), agent(Some(kim.id))]);
        // No one's agent.
        office_stays_off_with(&[lee.clone(), agent(None)]);
    }

    /// A save that fails does not count: it is tried again a second later, and when the loop
    /// stops, until it is written.
    #[test]
    fn a_failed_save_is_tried_again() {
        use pitcrew_hub_work::TaskRef;
        use pitcrew_protocol::model::{DispatchOutcome, TaskStatus};
        let (_tmp, state) = state();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let demo = pitcrew_fixtures::demo_workspace().unwrap();
        let sam = demo_person(&demo).unwrap();
        let writer: MemberId = "01JB000000000000000MEM0002".parse().unwrap();
        let Hub {
            tokens: _tokens,
            store: _store,
            work,
            office: back_office,
        } = open(&state, true, true).unwrap();
        // A directory where office.json goes: every save fails while it is there.
        let block = || {
            let _ = std::fs::remove_file(state.office());
            std::fs::create_dir(state.office()).unwrap();
        };
        let unblock = || std::fs::remove_dir(state.office()).unwrap();
        let saved = |rev: u64| progress(&state).is_some_and(|p| p["done"] == rev);
        let latest = || work.store().latest_rev().unwrap();
        let wait_for = |what: &str, holds: &dyn Fn() -> bool| {
            let deadline = Instant::now() + Duration::from_secs(20);
            while !holds() {
                assert!(Instant::now() < deadline, "never: {what}");
                std::thread::sleep(Duration::from_millis(20));
            }
        };
        // The log stays as it is for a second: the office has looked at all of it, its own
        // appends included.
        let settle = || {
            let mut seen = latest();
            let mut since = Instant::now();
            while since.elapsed() < Duration::from_secs(1) {
                std::thread::sleep(Duration::from_millis(50));
                let now = latest();
                if now != seen {
                    (seen, since) = (now, Instant::now());
                }
            }
            seen
        };
        let pap1 = TaskRef::parse("PAP-1").unwrap();
        block();
        runtime.block_on(async {
            let running = back_office.unwrap().spawn(Arc::clone(&work));
            append(
                work.store(),
                work.workspace(),
                writer,
                Some(sam),
                vec![EventBody::DispatchFinished {
                    dispatch: "01JB000000000000000DSP0001".parse().unwrap(),
                    outcome: DispatchOutcome::Succeeded,
                    summary: None,
                }],
            );
            wait_for("PAP-1 moves to review", &|| {
                work.task(&pap1).unwrap().status == TaskStatus::Review
            });
            let now = settle();
            // Saves failed meanwhile, at least once a second.
            assert!(state.office().is_dir());

            // Once it can be written, it is, without another run.
            unblock();
            wait_for("a save after the failures", &|| saved(now));

            // And the stop saves what failed saves could not.
            block();
            append(
                work.store(),
                work.workspace(),
                sam,
                None,
                vec![EventBody::CommentPosted {
                    task: Some(work.task(&pap1).unwrap().id),
                    workstream: None,
                    text: "Looks good.".into(),
                    mentions: Vec::new(),
                }],
            );
            settle();
            assert!(state.office().is_dir());
            unblock();
            running.stop(Duration::from_secs(20)).await;
        });
        assert!(saved(latest()), "{:?}", progress(&state));
    }
}
