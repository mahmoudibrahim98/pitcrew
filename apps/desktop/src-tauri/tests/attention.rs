//! "Needs you" against a fake daemon: the snapshot, asks raised and answered, asks for other
//! members ignored, resuming with `since`, a new event log, the bound, and one watcher per ready
//! workspace.

#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{FakeDaemon, OTHER, SAM, WORKSPACE_ID, WORKSPACE_NAME, WRITER, ask, ask_id};
use pitcrew_desktop::attention::{Attention, AttentionSink, Count, Limits, NewAsk};
use pitcrew_desktop::registry::{
    Connection, GatewayWorkspace, Registry, WorkspaceKind, WorkspaceRecord, WorkspaceState,
};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const TOKEN: &str = "pcd_attention-test-token-0123456789";

#[derive(Default)]
struct Recorder {
    asks: Mutex<Vec<(String, NewAsk)>>,
    changes: Mutex<usize>,
}

impl AttentionSink for Recorder {
    fn counts_changed(&self) {
        *self.changes.lock().unwrap() += 1;
    }

    fn new_ask(&self, workspace: &str, ask: NewAsk) {
        self.asks.lock().unwrap().push((workspace.to_owned(), ask));
    }
}

impl Recorder {
    fn titles(&self) -> Vec<String> {
        self.asks
            .lock()
            .unwrap()
            .iter()
            .map(|(_, a)| a.title.clone())
            .collect()
    }
}

fn wait_until(what: &str, check: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !check() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn count(attention: &Attention) -> Option<Count> {
    attention.counts().get(WORKSPACE_ID).copied()
}

fn open(n: usize) -> Option<Count> {
    Some(Count {
        open: n,
        more: false,
    })
}

fn list(registry: &Registry) -> Vec<GatewayWorkspace> {
    registry.list()
}

#[test]
fn needs_you_follows_the_daemon() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let daemon = FakeDaemon::start(&tmp.path().join("state"), TOKEN);
    // Before anyone watches: one ask for the person, one for someone else, one answered.
    daemon.raise(ask(1, WRITER, SAM));
    daemon.raise(ask(2, WRITER, OTHER));
    daemon.raise(ask(9, WRITER, SAM));
    daemon.answer(9);

    let registry = Arc::new(Registry::in_memory());
    registry
        .insert(
            WorkspaceRecord {
                id: WORKSPACE_ID.into(),
                name: WORKSPACE_NAME.into(),
                kind: WorkspaceKind::Local,
                connection: Connection::Local,
            },
            Some(Arc::new(daemon.connector())),
            WorkspaceState::Connecting,
        )
        .unwrap();
    let sink = Arc::new(Recorder::default());
    let attention = Attention::with_limits(
        Arc::clone(&registry),
        Arc::clone(&sink) as Arc<dyn AttentionSink>,
        rt.handle().clone(),
        Limits {
            max_open: 3,
            first_backoff: Duration::from_millis(50),
            max_backoff: Duration::from_millis(200),
            refetch_every: Duration::ZERO,
            ..Limits::default()
        },
    );

    // Not ready: not watched.
    attention.sync(&list(&registry));
    assert_eq!(attention.watching(), 0);
    registry.set_state(WORKSPACE_ID, WorkspaceState::Ready, None);
    attention.sync(&list(&registry));
    attention.sync(&list(&registry));
    assert_eq!(attention.watching(), 1, "one watcher per workspace");

    // The snapshot: only the person's open ask counts, and it is not news.
    wait_until("the snapshot", || count(&attention) == open(1));
    assert_eq!(daemon.streams(), [None]);
    assert!(sink.asks.lock().unwrap().is_empty());
    assert!(*sink.changes.lock().unwrap() >= 1);

    // Raised for the person: counted and reported, with the asker's name.
    daemon.raise(ask(3, WRITER, SAM));
    wait_until("ask 3", || count(&attention) == open(2));
    {
        let asks = sink.asks.lock().unwrap();
        assert_eq!(asks.len(), 1);
        let (workspace, new) = &asks[0];
        assert_eq!(workspace, WORKSPACE_ID);
        assert_eq!(new.id.0.to_string(), ask_id(3));
        assert_eq!(new.from.as_deref(), Some("Writer"));
        assert_eq!(new.title, "Question 3");
    }
    // For someone else, and other events: ignored. Answered: no longer counted.
    daemon.raise(ask(4, WRITER, OTHER));
    daemon.other_event();
    daemon.answer(1);
    wait_until("ask 1 answered", || count(&attention) == open(1));
    assert_eq!(sink.titles(), ["Question 3"]);

    // The stream breaks; what was raised meanwhile comes through `since`.
    let rev = daemon.rev();
    daemon.drop_streams();
    daemon.raise(ask(5, WRITER, SAM));
    wait_until("ask 5 after the reconnect", || count(&attention) == open(2));
    let streams = daemon.streams();
    assert_eq!(streams[0], None);
    assert!(
        streams[1..].contains(&Some(rev.to_string())),
        "resumed with since={rev}: {streams:?}"
    );
    assert_eq!(sink.titles(), ["Question 3", "Question 5"]);
    let snapshots = daemon.seen.lock().unwrap().ask_reads;
    assert_eq!(snapshots, 1, "resuming needs no snapshot");

    // The bound (3 here): past it the count says "more", and every ask is still news.
    daemon.raise(ask(6, WRITER, SAM));
    daemon.raise(ask(7, WRITER, SAM));
    wait_until("the bound", || {
        count(&attention)
            == Some(Count {
                open: 3,
                more: true,
            })
    });
    wait_until("asks 6 and 7 reported", || sink.titles().len() == 4);
    // An answer for an ask not kept: the count is unsure, so a snapshot sets it right.
    daemon.answer(7);
    wait_until("the snapshot after the bound", || {
        count(&attention) == open(3)
    });
    assert_eq!(daemon.seen.lock().unwrap().ask_reads, 2);

    // A new event log: everything is fetched again.
    daemon.answer(6);
    wait_until("ask 6 answered", || count(&attention) == open(2));
    daemon.new_log("log-2");
    daemon.drop_streams();
    wait_until("the snapshot after the new log", || {
        daemon.seen.lock().unwrap().ask_reads == 3
    });
    wait_until("the count after the new log", || {
        count(&attention) == open(2)
    });
    daemon.raise(ask(8, WRITER, SAM));
    wait_until("ask 8 in the new log", || count(&attention) == open(3));

    // The workspace goes away: no watcher, no count.
    attention.sync(&[]);
    assert_eq!(attention.watching(), 0);
    assert!(attention.counts().is_empty());
    drop(attention);
    rt.shutdown_timeout(Duration::from_secs(2));
}

#[test]
fn an_unreachable_daemon_is_retried_and_ready_reconnects_at_once() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    // A connector to where the daemon will be, before it is there.
    let daemon = FakeDaemon::start(&state, TOKEN);
    let connector = Arc::new(daemon.connector());
    drop(daemon);
    let registry = Arc::new(Registry::in_memory());
    registry
        .insert(
            WorkspaceRecord {
                id: WORKSPACE_ID.into(),
                name: WORKSPACE_NAME.into(),
                kind: WorkspaceKind::Local,
                connection: Connection::Local,
            },
            Some(connector),
            WorkspaceState::Ready,
        )
        .unwrap();
    let sink = Arc::new(Recorder::default());
    let attention = Attention::with_limits(
        Arc::clone(&registry),
        Arc::clone(&sink) as Arc<dyn AttentionSink>,
        rt.handle().clone(),
        Limits {
            // Long waits: only a poke makes it try again soon.
            first_backoff: Duration::from_secs(60),
            max_backoff: Duration::from_secs(60),
            ..Limits::default()
        },
    );
    attention.sync(&list(&registry));
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(count(&attention), None, "nothing to count yet");

    // The daemon comes up, and the workspace is ready again: it connects at once.
    let daemon = FakeDaemon::start(&state, TOKEN);
    daemon.raise(ask(1, WRITER, SAM));
    registry.set_state(WORKSPACE_ID, WorkspaceState::Unreachable, None);
    attention.sync(&list(&registry));
    registry.set_state(WORKSPACE_ID, WorkspaceState::Ready, None);
    attention.sync(&list(&registry));
    wait_until("the count", || count(&attention) == open(1));
    drop(attention);
    rt.shutdown_timeout(Duration::from_secs(2));
}

/// A runtime, and the attention of a registry holding `daemon`'s workspace, ready.
fn watching(daemon: &FakeDaemon, limits: Limits) -> (tokio::runtime::Runtime, Attention) {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let registry = Arc::new(Registry::in_memory());
    registry
        .insert(
            WorkspaceRecord {
                id: WORKSPACE_ID.into(),
                name: WORKSPACE_NAME.into(),
                kind: WorkspaceKind::Local,
                connection: Connection::Local,
            },
            Some(Arc::new(daemon.connector())),
            WorkspaceState::Ready,
        )
        .unwrap();
    let attention = Attention::with_limits(
        Arc::clone(&registry),
        Arc::new(Recorder::default()),
        rt.handle().clone(),
        limits,
    );
    attention.sync(&list(&registry));
    (rt, attention)
}

#[test]
fn a_snapshot_that_keeps_failing_backs_off_to_the_cap() {
    let tmp = tempfile::tempdir().unwrap();
    let daemon = FakeDaemon::start(&tmp.path().join("state"), TOKEN);
    daemon.raise(ask(1, WRITER, SAM));
    // Every hello is fine; every snapshot fails.
    daemon.fail_asks(true);
    let (rt, attention) = watching(
        &daemon,
        Limits {
            first_backoff: Duration::from_millis(50),
            max_backoff: Duration::from_millis(400),
            ..Limits::default()
        },
    );
    std::thread::sleep(Duration::from_millis(2600));
    let times = daemon.stream_times();
    let gaps: Vec<u128> = times
        .windows(2)
        .map(|w| w[1].duration_since(w[0]).as_millis())
        .collect();
    // 50, 100, 200, then 400 ms: about 9 attempts, not one every 50 ms (about 40).
    assert!((5..=12).contains(&times.len()), "attempts: {gaps:?}");
    assert!(gaps[0] >= 40 && gaps[1] >= 80 && gaps[2] >= 160, "{gaps:?}");
    assert!(
        gaps[3..].iter().all(|g| (320..900).contains(g)),
        "capped: {gaps:?}"
    );
    assert_eq!(count(&attention), None);

    // It works again: counted, and the wait starts again from the first one.
    daemon.fail_asks(false);
    wait_until("the count", || count(&attention) == open(1));
    let opened = daemon.stream_times().len();
    let dropped = Instant::now();
    daemon.drop_streams();
    wait_until("the reconnect", || daemon.stream_times().len() > opened);
    let back = *daemon.stream_times().last().unwrap();
    assert!(
        back.duration_since(dropped) < Duration::from_millis(300),
        "reconnected after {:?}",
        back.duration_since(dropped)
    );
    drop(attention);
    rt.shutdown_timeout(Duration::from_secs(2));
}

#[test]
fn too_many_asks_read_as_more_and_are_not_fetched_again_soon() {
    let tmp = tempfile::tempdir().unwrap();
    let daemon = FakeDaemon::start(&tmp.path().join("state"), TOKEN);
    for n in 1..=10 {
        daemon.raise(ask(n, WRITER, SAM));
    }
    let (rt, attention) = watching(
        &daemon,
        Limits {
            max_open: 3,
            // /v1/me and /v1/members fit; ten asks do not.
            max_body: 600,
            first_backoff: Duration::from_millis(50),
            too_large_backoff: Duration::from_secs(30),
            ..Limits::default()
        },
    );
    wait_until("the count", || {
        count(&attention)
            == Some(Count {
                open: 3,
                more: true,
            })
    });
    std::thread::sleep(Duration::from_millis(600));
    assert_eq!(daemon.stream_times().len(), 1, "no hot retry");
    assert_eq!(daemon.seen.lock().unwrap().ask_reads, 1);
    drop(attention);
    rt.shutdown_timeout(Duration::from_secs(2));
}
