//! `pitcrewd serve`: opens the state, wires the hub, serves API v1, and shuts down cleanly.
//!
//! Start:
//! 1. The token registry, which locks the state directory: a second daemon stops here.
//! 2. The store, with the work model's projections.
//! 3. With `--demo`: refuse a store with data, mint the tokens, seed the demo workspace.
//! 4. The device token: reused from `device.token` while it still verifies, else minted.
//! 5. The routes (`RouterParts`), the listener, and one line on stdout:
//!    `pitcrewd listening on <where>`.
//!
//! Stop (Ctrl+C or Ctrl+Break, or SIGTERM on Unix): the server stops accepting and finishes
//! in-flight requests (`pitcrew-api` closes open WebSockets with 1001), then the store closes,
//! checkpointing its WAL, and the lock is released last.

use crate::cli::{ListenArg, ServeArgs};
use crate::state::{StateDir, read_token, write_token};
use crate::terminals::NoRunner;
use anyhow::{Context as _, bail};
use axum::Extension;
use pitcrew_api::{
    Bound, EventSource, HookIntake, Listen, LogHookSink, RouterParts, StoreSource, StreamConfig,
    TerminalConfig, Terminals,
};
use pitcrew_auth::{FileTokenStore, TokenError, TokenStore};
use pitcrew_fixtures::DemoWorkspace;
use pitcrew_hub_work::WorkService;
use pitcrew_protocol::api::{Caller, HostRole, TokenScope};
use pitcrew_protocol::ids::{MemberId, WorkspaceId};
use pitcrew_protocol::model::MemberKind;
use pitcrew_store::{Store, StoreOptions};
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
    let hub = open(state, args.demo)?;
    let store = Arc::downgrade(&hub.store);
    // The lock goes last, so no other daemon opens the store while it closes.
    let lock = Arc::clone(&hub.tokens);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("pitcrewd")
        .build()
        .context("cannot start the async runtime")?;
    let served = runtime.block_on(run(hub, state, args.listen, started));
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
}

/// Steps 1–4: the token registry, the store, the demo, the device token.
fn open(state: &StateDir, demo: bool) -> anyhow::Result<Hub> {
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
    let store = Store::open_with(
        &path,
        StoreOptions::default(),
        pitcrew_hub_work::projections(),
    )
    .with_context(|| format!("cannot open the store {}", path.display()))?;
    let store = Arc::new(store);
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

    let workspace = match (
        &demo,
        store.since(0, 1).context("cannot read the store")?.first(),
    ) {
        (Some(demo), _) => demo.workspace.id,
        (None, Some(first)) => first.event.workspace,
        (None, None) => {
            tracing::warn!(
                "the store is empty and there is no way to create a workspace yet; start with \
                 --demo to try the demo workspace"
            );
            WorkspaceId::new()
        }
    };
    let work = Arc::new(WorkService::new(Arc::clone(&store), workspace));

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
    Ok(Hub {
        tokens,
        store,
        work,
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
    } = hub;
    let mut stop = Stop::listen().context("cannot listen for stop signals")?;

    let events: Arc<dyn EventSource> =
        Arc::new(StoreSource::new(Arc::clone(&store), store.log_id()));
    let hooks = HookIntake::start(Arc::new(LogHookSink), HOOK_QUEUE)
        .context("cannot start the hook intake")?;
    let terminals: Arc<dyn Terminals> = Arc::new(NoRunner::new(Arc::clone(&work)));
    let parts = RouterParts::new()
        .agent(pitcrew_api::hooks::routes(hooks))
        .agent(pitcrew_hub_work::agent_routes().layer(Extension(Arc::clone(&work))))
        .device(pitcrew_api::stream::routes(
            Arc::clone(&events),
            StreamConfig::default(),
        ))
        .device(pitcrew_api::activity::routes(events))
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

    tokio::select! {
        signal = stop.next() => tracing::info!(%signal, "stopping"),
        ended = &mut serving => {
            return match ended {
                Ok(Ok(())) => Err(anyhow::anyhow!("the server stopped on its own")),
                Ok(Err(e)) => Err(e).context("the server failed"),
                Err(e) => Err(e).context("the server task failed"),
            };
        }
    }

    // The server stops accepting, finishes in-flight requests and closes its WebSockets.
    let _ = draining.send(());
    match tokio::time::timeout(DRAIN, &mut serving).await {
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
    drop(tokens);
    close_store(store).await;
    Ok(())
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

/// The stop signals: Ctrl+C everywhere, SIGTERM on Unix, and Ctrl+Break and closing the console
/// on Windows. Registered at start, so a signal right after the ready line is not missed.
struct Stop {
    #[cfg(unix)]
    interrupt: tokio::signal::unix::Signal,
    #[cfg(unix)]
    terminate: tokio::signal::unix::Signal,
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

    fn state() -> (tempfile::TempDir, StateDir) {
        let tmp = tempfile::tempdir().unwrap();
        let state = StateDir::resolve(Some(tmp.path().join("state"))).unwrap();
        (tmp, state)
    }

    #[test]
    fn demo_mints_tokens_for_sam_and_writer_and_seeds() {
        let (_tmp, state) = state();
        let hub = open(&state, true).unwrap();
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
    }

    #[test]
    fn a_restart_keeps_the_workspace_and_the_device_token() {
        let (_tmp, state) = state();
        let (workspace, raw) = {
            let hub = open(&state, true).unwrap();
            (
                hub.work.workspace(),
                read_token(&state.device_token()).unwrap(),
            )
        };
        let hub = open(&state, false).unwrap();
        assert_eq!(hub.work.workspace(), workspace);
        assert_eq!(read_token(&state.device_token()).unwrap(), raw);
        let raw = raw.unwrap();
        assert_eq!(hub.tokens.verify(&raw).unwrap().scope, TokenScope::Device);
    }

    #[test]
    fn demo_refuses_a_store_with_data() {
        let (_tmp, state) = state();
        drop(open(&state, true).unwrap());
        let err = open(&state, true).err().unwrap();
        assert!(
            format!("{err:#}").contains("only an empty store"),
            "{err:#}"
        );
    }

    #[test]
    fn one_daemon_per_state_dir() {
        let (_tmp, state) = state();
        let _first = open(&state, false).unwrap();
        let err = open(&state, false).err().unwrap();
        assert!(format!("{err:#}").contains("already running"), "{err:#}");
    }

    #[test]
    fn a_lost_token_registry_mints_a_new_device_token() {
        let (_tmp, state) = state();
        let old = {
            let _hub = open(&state, true).unwrap();
            read_token(&state.device_token()).unwrap().unwrap()
        };
        std::fs::remove_file(state.root().join(FileTokenStore::FILE_NAME)).unwrap();
        let hub = open(&state, false).unwrap();
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
        let Some(crate::cli::Command::Serve(ServeArgs { listen, demo })) = cli.command else {
            panic!("not serve");
        };
        assert_eq!(listen, ListenArg::Private);
        assert!(!demo);
    }
}
