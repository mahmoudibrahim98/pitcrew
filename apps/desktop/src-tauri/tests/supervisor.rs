//! The supervisor with fake `pitcrewd` scripts: it starts the daemon, restarts it with a growing
//! backoff, gives up after repeated failures, uses a daemon that already runs, and stops only
//! what it started. Also the registry's local workspace, which follows the supervisor.

#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{FakeDaemon, WORKSPACE_ID, WORKSPACE_NAME};
use pitcrew_desktop::daemon::endpoint::Endpoint;
use pitcrew_desktop::daemon::supervisor::{DaemonState, Options, Supervisor};
use pitcrew_desktop::daemon::{LocalConnector, follow};
use pitcrew_desktop::registry::{Registry, WorkspaceKind, WorkspaceState};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::watch;

/// A fake `pitcrewd`: `token show-path` prints `<dir>/device.token`; `serve` logs `start <pid>`
/// to `<dir>/log`, then does `serve`.
fn fake_pitcrewd(dir: &Path, serve: &str) -> PathBuf {
    fake_pitcrewd_with(dir, serve, r#"echo "$DIR/device.token"; exit 0"#)
}

/// A fake `pitcrewd` whose `token show-path` does `show_path`.
fn fake_pitcrewd_with(dir: &Path, serve: &str, show_path: &str) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let path = dir.join("pitcrewd");
    let script = format!(
        r#"#!/bin/sh
DIR='{dir}'
case "$*" in
  *"token show-path"*) {show_path} ;;
  *"serve --listen private"*)
    echo "start $$" >> "$DIR/log"
    {serve}
    ;;
  *) echo "unexpected: $*" >&2; exit 64 ;;
esac
"#,
        dir = dir.display(),
    );
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// Prints the ready line and runs until SIGTERM, which it logs.
const HEALTHY: &str = r#"trap 'echo "term $$" >> "$DIR/log"; exit 0' TERM
    echo "pitcrewd listening on $DIR/state/run/pitcrewd.sock"
    while true; do sleep 0.05; done"#;

/// Gets ready, then stops on its own.
const CRASHES: &str = r#"echo "pitcrewd listening on $DIR/state/run/pitcrewd.sock"
    sleep 0.2
    echo "the store is corrupt" >&2
    exit 3"#;

/// Never gets ready.
const FAILS_AT_START: &str = r#"echo "pitcrewd: another pitcrewd is already running" >&2
    exit 1"#;

/// Starts, but never prints its ready line; logs SIGTERM.
const NEVER_READY: &str = r#"trap 'echo "term $$" >> "$DIR/log"; exit 0' TERM
    while true; do sleep 0.05; done"#;

/// Waits until the log has a line starting with `prefix`.
async fn logged(dir: &Path, prefix: &str) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !log(dir).iter().any(|l| l.starts_with(prefix)) {
        assert!(Instant::now() < deadline, "no {prefix:?} in {:?}", log(dir));
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn log(dir: &Path) -> Vec<String> {
    std::fs::read_to_string(dir.join("log"))
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

fn starts(dir: &Path) -> usize {
    log(dir).iter().filter(|l| l.starts_with("start ")).count()
}

fn options(program: Result<PathBuf, String>, dir: &Path) -> Options {
    let state = dir.join("state");
    let mut options = Options::new(
        program,
        Some(state.clone()),
        Endpoint::Unix {
            dir: state.join("run"),
        },
    );
    options.first_backoff = Duration::from_millis(100);
    options.max_backoff = Duration::from_millis(400);
    options.max_failures = 4;
    options.watch_every = Duration::from_millis(100);
    options.stop_timeout = Duration::from_secs(5);
    options
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
}

/// Waits until the state matches, and returns it.
async fn until(
    state: &mut watch::Receiver<DaemonState>,
    what: &str,
    check: impl Fn(&DaemonState) -> bool,
) -> DaemonState {
    let wait = async {
        loop {
            let current = state.borrow_and_update().clone();
            if check(&current) {
                return current;
            }
            state.changed().await.unwrap();
        }
    };
    tokio::time::timeout(Duration::from_secs(30), wait)
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {what}; last {:?}", *state.borrow()))
}

#[test]
fn it_starts_the_daemon_and_stops_it_when_the_app_quits() {
    let tmp = tempfile::tempdir().unwrap();
    let program = fake_pitcrewd(tmp.path(), HEALTHY);
    let rt = runtime();
    rt.block_on(async {
        let supervisor = Supervisor::start(options(Ok(program), tmp.path()), &rt.handle().clone());
        let mut state = supervisor.state();
        let ready = until(&mut state, "ready", |s| {
            matches!(s, DaemonState::Ready { .. })
        })
        .await;
        assert_eq!(
            ready,
            DaemonState::Ready {
                started_here: true,
                token: tmp.path().join("device.token"),
            }
        );
        assert_eq!(starts(tmp.path()), 1);
        supervisor.shutdown().await;
    });
    let log = log(tmp.path());
    assert_eq!(log.len(), 2, "{log:?}");
    assert!(
        log[1].starts_with("term "),
        "it was stopped with SIGTERM: {log:?}"
    );
    assert_eq!(log[0].replace("start", "term"), log[1], "the same process");
}

#[test]
fn it_restarts_with_a_growing_backoff_then_gives_up() {
    let tmp = tempfile::tempdir().unwrap();
    let program = fake_pitcrewd(tmp.path(), CRASHES);
    let rt = runtime();
    rt.block_on(async {
        let supervisor = Supervisor::start(options(Ok(program), tmp.path()), &rt.handle().clone());
        let mut state = supervisor.state();
        let mut readies = Vec::new();
        let given_up = loop {
            let current = until(&mut state, "a change", |_| true).await;
            match current {
                DaemonState::Ready { started_here, .. } => {
                    assert!(started_here);
                    readies.push(Instant::now());
                }
                DaemonState::Unreachable { detail } => break detail,
                DaemonState::Connecting => {}
            }
            state.changed().await.unwrap();
        };
        assert_eq!(readies.len(), 4, "four runs before giving up");
        // Each run lasts about 0.2 s; the waits between them are 0.1, 0.2, then 0.4 s.
        let gaps: Vec<Duration> = readies.windows(2).map(|w| w[1] - w[0]).collect();
        assert!(
            gaps[2] >= gaps[0] + Duration::from_millis(200),
            "the backoff grows: {gaps:?}"
        );
        assert!(given_up.contains("4 times in a row"), "{given_up}");
        assert!(given_up.contains("exit status: 3"), "{given_up}");
        // What the daemon printed stays in the log; the detail (shown in the UI) is the app's.
        assert!(!given_up.contains("corrupt"), "{given_up}");

        // Given up: nothing starts any more.
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert_eq!(starts(tmp.path()), 4);
        supervisor.shutdown().await;
    });
}

#[test]
fn a_daemon_that_never_gets_ready_counts_as_a_failure() {
    let tmp = tempfile::tempdir().unwrap();
    let program = fake_pitcrewd(tmp.path(), FAILS_AT_START);
    let rt = runtime();
    rt.block_on(async {
        let supervisor = Supervisor::start(options(Ok(program), tmp.path()), &rt.handle().clone());
        let mut state = supervisor.state();
        let state = until(&mut state, "unreachable", |s| {
            matches!(s, DaemonState::Unreachable { .. })
        })
        .await;
        let DaemonState::Unreachable { detail } = state else {
            unreachable!()
        };
        assert!(detail.contains("before it was ready"), "{detail}");
        assert!(detail.contains("exit status: 1"), "{detail}");
        assert!(!detail.contains("already running"), "{detail}");
        assert_eq!(starts(tmp.path()), 4);
        supervisor.shutdown().await;
    });
}

#[test]
fn without_a_pitcrewd_the_daemon_is_unreachable() {
    let tmp = tempfile::tempdir().unwrap();
    let rt = runtime();
    rt.block_on(async {
        let supervisor = Supervisor::start(
            options(
                Err("pitcrewd is not next to the app or on PATH".into()),
                tmp.path(),
            ),
            &rt.handle().clone(),
        );
        let mut state = supervisor.state();
        let state = until(&mut state, "unreachable", |s| {
            matches!(s, DaemonState::Unreachable { .. })
        })
        .await;
        assert_eq!(
            state,
            DaemonState::Unreachable {
                detail: "pitcrewd is not next to the app or on PATH".into()
            }
        );
        supervisor.shutdown().await;
    });
}

#[test]
fn it_uses_a_running_daemon_registers_its_workspace_and_never_stops_it() {
    let tmp = tempfile::tempdir().unwrap();
    // Someone started pitcrewd by hand; its state directory is the one the app uses.
    let daemon = FakeDaemon::start(&tmp.path().join("state"), "pcd_running-daemon-token");
    std::fs::copy(daemon.token_file(), tmp.path().join("device.token")).unwrap();
    std::fs::set_permissions(
        tmp.path().join("device.token"),
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let program = fake_pitcrewd(tmp.path(), HEALTHY);
    let registry = Arc::new(Registry::in_memory());
    let rt = runtime();
    rt.block_on(async {
        let supervisor = Supervisor::start(options(Ok(program), tmp.path()), &rt.handle().clone());
        let connector = Arc::new(LocalConnector::new(daemon.endpoint()));
        registry
            .attach_local(Arc::clone(&connector) as Arc<dyn pitcrew_desktop::gateway::Connector>);
        let following = tokio::spawn(follow(supervisor.state(), connector, Arc::clone(&registry)));

        let mut state = supervisor.state();
        let ready = until(&mut state, "ready", |s| {
            matches!(s, DaemonState::Ready { .. })
        })
        .await;
        assert!(
            matches!(
                ready,
                DaemonState::Ready {
                    started_here: false,
                    ..
                }
            ),
            "{ready:?}"
        );

        // The local workspace is registered with the daemon's id and name.
        let deadline = Instant::now() + Duration::from_secs(20);
        while registry.list().is_empty() {
            assert!(Instant::now() < deadline);
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let list = registry.list();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, WORKSPACE_ID);
        assert_eq!(list[0].name, WORKSPACE_NAME);
        assert_eq!(list[0].kind, WorkspaceKind::Local);
        assert_eq!(list[0].state, WorkspaceState::Ready);

        supervisor.shutdown().await;
        let _ = following.await;
    });
    // It never started or stopped a pitcrewd of its own, and the running one still answers.
    assert!(log(tmp.path()).is_empty(), "{:?}", log(tmp.path()));
    let rt = runtime();
    rt.block_on(async {
        assert!(daemon.endpoint().connect().await.is_ok());
    });
}

#[test]
fn when_the_running_daemon_goes_away_it_starts_its_own() {
    let tmp = tempfile::tempdir().unwrap();
    let daemon = FakeDaemon::start(&tmp.path().join("state"), "pcd_running-daemon-token");
    let program = fake_pitcrewd(tmp.path(), HEALTHY);
    let rt = runtime();
    rt.block_on(async {
        let supervisor = Supervisor::start(options(Ok(program), tmp.path()), &rt.handle().clone());
        let mut state = supervisor.state();
        until(&mut state, "someone else's daemon", |s| {
            matches!(
                s,
                DaemonState::Ready {
                    started_here: false,
                    ..
                }
            )
        })
        .await;
        drop(daemon);
        until(&mut state, "its own daemon", |s| {
            matches!(
                s,
                DaemonState::Ready {
                    started_here: true,
                    ..
                }
            )
        })
        .await;
        assert_eq!(starts(tmp.path()), 1);
        supervisor.shutdown().await;
    });
    assert!(log(tmp.path()).iter().any(|l| l.starts_with("term ")));
}

#[test]
fn quitting_while_the_daemon_starts_stops_it() {
    let tmp = tempfile::tempdir().unwrap();
    let program = fake_pitcrewd(tmp.path(), NEVER_READY);
    let rt = runtime();
    rt.block_on(async {
        let mut options = options(Ok(program), tmp.path());
        options.ready_timeout = Duration::from_secs(60);
        let supervisor = Supervisor::start(options, &rt.handle().clone());
        logged(tmp.path(), "start ").await;
        let quit = Instant::now();
        supervisor.shutdown().await;
        assert!(
            quit.elapsed() < Duration::from_secs(5),
            "{:?}",
            quit.elapsed()
        );
    });
    let log = log(tmp.path());
    assert_eq!(log.len(), 2, "{log:?}");
    assert_eq!(
        log[0].replace("start", "term"),
        log[1],
        "stopped with SIGTERM"
    );
}

#[test]
fn quitting_while_asking_for_the_token_path_stops_the_daemon() {
    let tmp = tempfile::tempdir().unwrap();
    // `token show-path` hangs: the daemon it started is up but not yet Ready.
    let program = fake_pitcrewd_with(
        tmp.path(),
        HEALTHY,
        r#"echo "show-path $$" >> "$DIR/log"; sleep 60"#,
    );
    let rt = runtime();
    rt.block_on(async {
        let supervisor = Supervisor::start(options(Ok(program), tmp.path()), &rt.handle().clone());
        logged(tmp.path(), "show-path ").await;
        assert_eq!(*supervisor.state().borrow(), DaemonState::Connecting);
        let quit = Instant::now();
        supervisor.shutdown().await;
        assert!(
            quit.elapsed() < Duration::from_secs(5),
            "{:?}",
            quit.elapsed()
        );
    });
    let log = log(tmp.path());
    let start = log.iter().find(|l| l.starts_with("start ")).unwrap();
    assert!(
        log.contains(&start.replace("start", "term")),
        "the daemon it started was stopped: {log:?}"
    );
}
