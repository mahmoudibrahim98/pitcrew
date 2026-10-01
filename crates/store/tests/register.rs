//! `Store::register`: adding a projection to an already-open store, catching it up without
//! dropping the lease (`docs/build/briefs/C-import-and-reopen.md`, "Projections after open,
//! without dropping the lease"). This is how the daemon should close the gap between its two
//! opens (`crates/daemon/README.md`, "Known gaps" — "The lease between the two opens"): call
//! `register` once the work model's projections have found or added `@office`, instead of
//! reopening the store with the office's run log added.

mod common;

use common::{CountBy, Gate, Gated, by_type, toy_migrations};
use pitcrew_protocol::events::Event;
use pitcrew_protocol::ids::EventId;
use pitcrew_store::{Error, FsMode, Store, StoreOptions};
use rusqlite::OptionalExtension;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::time::Duration;

fn fixture_events() -> Vec<Event> {
    pitcrew_fixtures::demo_workspace().expect("fixture").events
}

fn generated_events(n: usize) -> Vec<Event> {
    fixture_events()
        .into_iter()
        .cycle()
        .take(n)
        .map(|mut e| {
            e.id = EventId::new();
            e
        })
        .collect()
}

type Table = Vec<(String, i64, i64)>;

/// The `toy_by_type` table computed directly from `events`, the same way `tests/projections.rs`'s
/// `expected` does for both toy tables, kept to just the one this file needs.
fn expected_by_type(events: &[Event]) -> Table {
    use std::collections::BTreeMap;
    let mut map: BTreeMap<String, (i64, i64)> = BTreeMap::new();
    for (e, rev) in events.iter().zip(1i64..) {
        let key = by_type(&pitcrew_store::StoredEvent {
            rev: u64::try_from(rev).expect("rev"),
            event: e.clone(),
        });
        let entry = map.entry(key).or_default();
        entry.0 += 1;
        entry.1 = rev;
    }
    map.into_iter().map(|(k, (n, last))| (k, n, last)).collect()
}

fn toy_by_type_table(store: &Store) -> Table {
    store
        .read(|c| -> pitcrew_store::Result<Table> {
            let mut stmt = c.prepare("SELECT key, n, last FROM toy_by_type ORDER BY key")?;
            Ok(stmt
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<rusqlite::Result<Table>>()?)
        })
        .expect("read")
}

fn open(path: &Path) -> Store {
    Store::open_with_migrations(path, StoreOptions::default(), &toy_migrations(), Vec::new())
        .expect("open without projections")
}

#[test]
fn register_catches_up_from_before_it_was_added_and_keeps_receiving_events() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("store.db");
    let store = open(&path);
    let before_events = fixture_events();
    store
        .append(&before_events)
        .expect("append before register");

    store
        .register(Box::new(CountBy::types()))
        .expect("register");
    assert_eq!(toy_by_type_table(&store), expected_by_type(&before_events));

    // Appends after register keep applying to it, same as a projection present from the start.
    let more = generated_events(5);
    store.append(&more).expect("append after register");
    let all: Vec<Event> = before_events.into_iter().chain(more).collect();
    assert_eq!(toy_by_type_table(&store), expected_by_type(&all));
}

#[test]
fn registering_the_same_name_twice_is_refused_and_changes_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("store.db");
    let store = Store::open_with_migrations(
        &path,
        StoreOptions::default(),
        &toy_migrations(),
        vec![Box::new(CountBy::types())],
    )
    .expect("open");
    let events = fixture_events();
    store.append(&events).expect("append");
    let before = toy_by_type_table(&store);

    let err = store
        .register(Box::new(CountBy::types()))
        .expect_err("duplicate name");
    assert!(
        matches!(&err, Error::DuplicateProjection { name } if name == "toy.by_type"),
        "{err:?}"
    );
    assert_eq!(toy_by_type_table(&store), before);

    // The store still works normally afterwards.
    let more = generated_events(2);
    store.append(&more).expect("append");
}

#[test]
fn register_keeps_the_lease_generation_in_network_mode() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("store.db");
    let mut options = StoreOptions::default();
    options.fs = FsMode::Network;
    let store =
        Store::open_with_migrations(&path, options, &toy_migrations(), Vec::new()).expect("open");
    let gen1 = dir.path().join("store.db.lease.1");
    assert!(gen1.exists(), "open must take generation 1");

    store.append(&fixture_events()[..3]).expect("append");
    store
        .register(Box::new(CountBy::types()))
        .expect("register");

    assert!(
        gen1.exists(),
        "register must not release or replace the lease"
    );
    assert!(
        !dir.path().join("store.db.lease.2").exists(),
        "register must never take a new generation: it is still the same open Store"
    );
    assert_eq!(
        toy_by_type_table(&store),
        expected_by_type(&fixture_events()[..3])
    );
}

/// Pauses `register`'s catch-up (its first `reset` call) mid-flight and proves two things at
/// once: an append attempted while the catch-up is paused queues behind it rather than running
/// ahead (it shares the one write lock), and once both finish, the projection reflects every
/// event — the ones already in the log before `register` started, and the one appended while its
/// catch-up was paused.
#[test]
fn an_append_queued_behind_registers_catch_up_is_still_seen() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("store.db");
    let store = open(&path);
    let before_events = fixture_events();
    store
        .append(&before_events)
        .expect("append before register");
    let store = Arc::new(store);

    let gate = Gate::new();
    let projection = Gated {
        inner: CountBy::types(),
        gate: Arc::clone(&gate),
    };
    gate.armed.store(true, Ordering::SeqCst);

    let register = {
        let store = Arc::clone(&store);
        std::thread::spawn(move || store.register(Box::new(projection)))
    };
    // register's catch-up (reset) is now parked here, still holding the store's one write lock.
    gate.barrier.wait();

    let more = generated_events(3);
    let (tx, rx) = mpsc::channel();
    {
        let store = Arc::clone(&store);
        let more = more.clone();
        std::thread::spawn(move || {
            let result = store.append(&more);
            let _ = tx.send(result);
        });
    }
    // The append needs the same write lock register's catch-up holds, so it must still be
    // waiting; a short timeout (not a hang) proves it did not run ahead of the registration.
    assert!(
        matches!(
            rx.recv_timeout(Duration::from_millis(300)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ),
        "an append must queue behind register's catch-up, not run ahead of it"
    );

    gate.barrier.wait(); // let reset, and so register, finish
    register.join().expect("thread").expect("register");
    let append_result = rx
        .recv_timeout(Duration::from_secs(10))
        .expect("the queued append must finish once register releases the write lock");
    append_result.expect("append");

    let all: Vec<Event> = before_events.into_iter().chain(more).collect();
    assert_eq!(
        toy_by_type_table(&store),
        expected_by_type(&all),
        "the projection must reflect the log before register and the append queued during its catch-up"
    );
}

fn checkpoint(store: &Store, name: &str) -> Option<(u32, i64)> {
    store
        .read(|c| -> pitcrew_store::Result<Option<(u32, i64)>> {
            Ok(c.query_row(
                "SELECT version, rev FROM projection_state WHERE name = ?1",
                [name],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?)
        })
        .expect("read")
}

/// A catch-up that fails partway (here, on the third of several events, the same way
/// `tests/projections.rs`'s `a_failing_apply_rolls_back_the_append` fails an append) registers
/// nothing: no checkpoint row at all for the name, so it is free to register again, and a working
/// projection registered under the same name afterwards catches up normally.
#[test]
fn a_failing_catch_up_registers_nothing_and_the_name_can_be_retried() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("store.db");
    let store = open(&path);
    let events = fixture_events();
    store.append(&events).expect("append before register");

    // events[2] is a task_moved (as tests/projections.rs's own picky-projection tests rely on).
    let picky = CountBy {
        fail_on: Some("task_moved"),
        ..CountBy::types()
    };
    let err = store
        .register(Box::new(picky))
        .expect_err("the catch-up must fail on the picky event");
    assert!(
        matches!(&err, Error::Projection { name, .. } if name == "toy.by_type"),
        "{err:?}"
    );

    // Nothing was registered: no checkpoint row at all (there was none before this attempt, and
    // the failed sync's own transaction rolled back), so the name is free to register again.
    assert_eq!(
        checkpoint(&store, "toy.by_type"),
        None,
        "a failed register must leave no checkpoint behind"
    );

    store
        .register(Box::new(CountBy::types()))
        .expect("re-registering the same name now succeeds");
    assert_eq!(toy_by_type_table(&store), expected_by_type(&events));
}
