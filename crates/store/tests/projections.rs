mod common;

use common::{CountBy, both, by_author, by_type, toy_migrations};
use pitcrew_protocol::events::Event;
use pitcrew_protocol::ids::EventId;
use pitcrew_store::sql::{self, Transaction};
use pitcrew_store::{
    BoxError, Error, EventFilter, Projection, RevRange, Store, StoreOptions, StoredEvent,
};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier, mpsc};
use std::time::Duration;

fn open(path: &Path, projections: Vec<Box<dyn Projection>>) -> Result<Store, Error> {
    Store::open_with_migrations(
        path,
        StoreOptions::default(),
        &toy_migrations(),
        projections,
    )
}

type Table = Vec<(String, i64, i64)>;

fn rows(conn: &sql::Connection, name: &str) -> sql::Result<Table> {
    let mut stmt = conn.prepare(&format!("SELECT key, n, last FROM {name} ORDER BY key"))?;
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect()
}

fn table(store: &Store, name: &str) -> Table {
    store
        .read(|conn| -> pitcrew_store::Result<Table> { Ok(rows(conn, name)?) })
        .expect("read")
}

/// A table read straight from the file, for when no store is open.
fn raw_table(path: &Path, name: &str) -> Table {
    rows(&sql::Connection::open(path).expect("raw"), name).expect("rows")
}

fn tables(store: &Store) -> (Table, Table) {
    (table(store, "toy_by_type"), table(store, "toy_by_author"))
}

/// The tables computed directly from the events, rev 1 onwards.
fn expected(events: &[Event]) -> (Table, Table) {
    let count = |key: fn(&StoredEvent) -> String| -> Table {
        let mut map: BTreeMap<String, (i64, i64)> = BTreeMap::new();
        for (e, rev) in events.iter().zip(1i64..) {
            let stored = StoredEvent {
                rev: u64::try_from(rev).expect("rev"),
                event: e.clone(),
            };
            let entry = map.entry(key(&stored)).or_default();
            entry.0 += 1;
            entry.1 = rev;
        }
        map.into_iter().map(|(k, (n, last))| (k, n, last)).collect()
    };
    (count(by_type), count(by_author))
}

fn checkpoint(path: &Path, name: &str) -> Option<(u32, u64)> {
    let conn = sql::Connection::open(path).expect("raw");
    conn.query_row(
        "SELECT version, rev FROM projection_state WHERE name = ?1",
        [name],
        |r| Ok((r.get(0)?, r.get::<_, i64>(1)?.try_into().expect("rev"))),
    )
    .ok()
}

fn fixture_events() -> Vec<Event> {
    pitcrew_fixtures::demo_workspace()
        .expect("fixture parses")
        .events
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

/// Appends `events` in batches with projections registered, then checks the tables against a
/// direct count, a rebuild, and (if `late`) a store that only met the projections after the fact.
fn incremental_equals_rebuilt(events: &[Event], batch: usize, late: bool) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("store.db");
    let store = open(&path, both()).expect("open");
    for chunk in events.chunks(batch) {
        store.append(chunk).expect("append");
    }
    let incremental = tables(&store);
    assert_eq!(incremental, expected(events));
    let total = u64::try_from(events.len()).expect("fits");
    assert_eq!(checkpoint(&path, "toy.by_type"), Some((1, total)));

    store.rebuild("toy.by_type").expect("rebuild");
    store.rebuild("toy.by_author").expect("rebuild");
    assert_eq!(tables(&store), incremental);
    drop(store);
    if !late {
        return;
    }

    // The log first, the projections later: the open builds them from scratch.
    let path = dir.path().join("late.db");
    let store = open(&path, Vec::new()).expect("open");
    for chunk in events.chunks(batch) {
        store.append(chunk).expect("append");
    }
    drop(store);
    let store = open(&path, both()).expect("reopen");
    assert_eq!(tables(&store), incremental);
}

#[test]
fn fixture_incremental_equals_rebuilt() {
    incremental_equals_rebuilt(&fixture_events(), 4, true);
}

#[test]
fn generated_50k_incremental_equals_rebuilt() {
    // More than one replay batch, and batches that do not divide it evenly. The open-time
    // rebuild shares `Store::rebuild`'s replay, so the fixture run covers it.
    incremental_equals_rebuilt(&generated_events(50_000), 333, false);
}

#[test]
fn bumping_the_version_rebuilds_on_open() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("store.db");
    let events = fixture_events();
    drop({
        let store = open(&path, both()).expect("open");
        store.append(&events).expect("append");
        store
    });
    // Tamper with the table, as a changed `apply` would leave it out of date.
    sql::Connection::open(&path)
        .expect("raw")
        .execute(
            "INSERT INTO toy_by_type (key, n, last) VALUES ('junk', 1, 1)",
            [],
        )
        .expect("tamper");

    // Same version: kept as is.
    let store = open(&path, both()).expect("reopen");
    assert!(tables(&store).0.iter().any(|row| row.0 == "junk"));
    drop(store);

    let bumped = CountBy {
        version: 2,
        ..CountBy::types()
    };
    let store = open(&path, vec![Box::new(bumped), Box::new(CountBy::authors())]).expect("reopen");
    assert_eq!(tables(&store), expected(&events));
    drop(store);
    assert_eq!(checkpoint(&path, "toy.by_type"), Some((2, 15)));
    assert_eq!(checkpoint(&path, "toy.by_author"), Some((1, 15)));
}

#[test]
fn a_projection_behind_the_log_catches_up() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("store.db");
    let events = fixture_events();
    drop({
        let store = open(&path, both()).expect("open");
        store.append(&events[..5]).expect("append");
        store
    });
    drop({
        let store = open(&path, Vec::new()).expect("open without projections");
        store.append(&events[5..10]).expect("append");
        store
    });
    assert_eq!(checkpoint(&path, "toy.by_type"), Some((1, 5)));

    let store = open(&path, both()).expect("reopen");
    assert_eq!(checkpoint(&path, "toy.by_type"), Some((1, 10)));
    assert_eq!(tables(&store), expected(&events[..10]));

    // Another writer appends while this store is open; the next append here fills the gap.
    let other = open(&path, Vec::new()).expect("second store");
    other.append(&events[10..12]).expect("append");
    store.append(&events[12..]).expect("append");
    assert_eq!(tables(&store), expected(&events));
    assert_eq!(checkpoint(&path, "toy.by_author"), Some((1, 15)));
}

#[test]
fn a_failing_apply_rolls_back_the_append() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("store.db");
    let events = fixture_events();
    let picky = CountBy {
        fail_on: Some("task_moved"),
        ..CountBy::types()
    };
    // A working projection first: it applies the failing batch before `picky` refuses it, and
    // those writes must roll back too.
    let store = open(&path, vec![Box::new(CountBy::authors()), Box::new(picky)]).expect("open");
    let mut rx = store.subscribe();
    store.append(&events[..2]).expect("append");
    let before = tables(&store);
    assert_eq!(before, expected(&events[..2]));

    // events[2] is a task_moved.
    let err = store.append(&events[2..4]).expect_err("apply fails");
    assert!(
        matches!(&err, Error::Projection { name, rev: 3, .. } if name == "toy.by_type"),
        "{err:?}"
    );
    assert!(
        std::error::Error::source(&err)
            .expect("source")
            .to_string()
            .contains("refusing task_moved")
    );
    assert_eq!(store.latest_rev().expect("rev"), 2);
    assert_eq!(tables(&store), before);
    assert_eq!(checkpoint(&path, "toy.by_type"), Some((1, 2)));
    assert_eq!(checkpoint(&path, "toy.by_author"), Some((1, 2)));

    // No revision was used: the next append gets 3.
    let range = store.append(&events[3..4]).expect("append");
    assert_eq!(
        range,
        RevRange {
            from_rev: 3,
            to_rev: 3
        }
    );
    // Only the successful appends were announced.
    assert_eq!(rx.try_recv().expect("first").to_rev, 2);
    assert_eq!(rx.try_recv().expect("second").from_rev, 3);
    assert!(rx.try_recv().is_err());
}

#[test]
fn a_failing_rebuild_fails_the_open() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("store.db");
    drop({
        let store = open(&path, Vec::new()).expect("open");
        store.append(&fixture_events()).expect("append");
        store
    });
    let picky = CountBy {
        fail_on: Some("task_moved"),
        ..CountBy::types()
    };
    let err = open(&path, vec![Box::new(picky)]).expect_err("rebuild fails");
    assert!(matches!(err, Error::Projection { rev: 3, .. }), "{err:?}");
    assert_eq!(checkpoint(&path, "toy.by_type"), None);
    // Revisions 1 and 2 were applied before rev 3 failed; they rolled back with it.
    assert_eq!(raw_table(&path, "toy_by_type"), Table::new());
    assert_eq!(raw_table(&path, "toy_by_author"), Table::new());
}

#[test]
fn a_failing_rebuild_call_changes_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("store.db");
    let events = fixture_events();
    drop({
        let store = open(&path, both()).expect("open");
        store.append(&events).expect("append");
        store
    });
    // Same version and caught up, so the open replays nothing; a rebuild meets the task_moved.
    let picky = CountBy {
        fail_on: Some("task_moved"),
        ..CountBy::types()
    };
    let store = open(&path, vec![Box::new(picky)]).expect("open");
    let before = table(&store, "toy_by_type");
    assert_eq!(before, expected(&events).0);

    let err = store.rebuild("toy.by_type").expect_err("rebuild fails");
    assert!(matches!(err, Error::Projection { rev: 3, .. }), "{err:?}");
    // The reset and revisions 1 and 2 rolled back.
    assert_eq!(table(&store, "toy_by_type"), before);
    assert_eq!(checkpoint(&path, "toy.by_type"), Some((1, 15)));
}

#[test]
fn a_version_mismatch_during_an_append_is_an_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("store.db");
    let events = generated_events(20);
    let old = open(&path, both()).expect("open v1");
    old.append(&events[..5]).expect("append");

    // A newer build opens the same file and rebuilds by_type at version 2.
    let bumped = CountBy {
        version: 2,
        ..CountBy::types()
    };
    let new = open(&path, vec![Box::new(bumped), Box::new(CountBy::authors())]).expect("open v2");
    assert_eq!(checkpoint(&path, "toy.by_type"), Some((2, 5)));

    // The old store refuses to append rather than rebuild it back to version 1 (and the new one
    // then back to 2, on every append).
    let err = old.append(&events[5..7]).expect_err("version mismatch");
    assert!(
        matches!(&err, Error::ProjectionVersion { name, stored: 2, ours: 1 } if name == "toy.by_type"),
        "{err:?}"
    );
    assert_eq!(old.latest_rev().expect("rev"), 5);
    assert_eq!(checkpoint(&path, "toy.by_type"), Some((2, 5)));
    assert_eq!(checkpoint(&path, "toy.by_author"), Some((1, 5)));
    // Nor does it rebuild it on demand: that would be a downgrade.
    let err = old.rebuild("toy.by_type").expect_err("downgrade");
    assert!(
        matches!(
            err,
            Error::ProjectionVersion {
                stored: 2,
                ours: 1,
                ..
            }
        ),
        "{err:?}"
    );
    assert_eq!(checkpoint(&path, "toy.by_type"), Some((2, 5)));

    // The newer store appends as usual.
    new.append(&events[5..15]).expect("append");
    assert_eq!(tables(&new), expected(&events[..15]));
    assert_eq!(checkpoint(&path, "toy.by_type"), Some((2, 15)));

    // A lower stored version is refused on append too (written by hand here; a build from before
    // this check could have rebuilt it).
    sql::Connection::open(&path)
        .expect("raw")
        .execute(
            "UPDATE projection_state SET version = 1 WHERE name = 'toy.by_type'",
            [],
        )
        .expect("tamper");
    let err = new.append(&events[15..]).expect_err("version mismatch");
    assert!(
        matches!(
            err,
            Error::ProjectionVersion {
                stored: 1,
                ours: 2,
                ..
            }
        ),
        "{err:?}"
    );
    assert_eq!(new.latest_rev().expect("rev"), 15);
    assert_eq!(checkpoint(&path, "toy.by_type"), Some((1, 15)));
}

#[test]
fn a_downgrade_is_refused_on_open() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("store.db");
    let events = fixture_events();
    let bumped = CountBy {
        version: 2,
        ..CountBy::types()
    };
    drop({
        let store = open(&path, vec![Box::new(bumped)]).expect("open v2");
        store.append(&events).expect("append");
        store
    });
    let err = open(&path, both()).expect_err("downgrade");
    assert!(
        matches!(&err, Error::ProjectionVersion { name, stored: 2, ours: 1 } if name == "toy.by_type"),
        "{err:?}"
    );
    assert_eq!(checkpoint(&path, "toy.by_type"), Some((2, 15)));
    assert_eq!(raw_table(&path, "toy_by_type"), expected(&events).0);
}

#[test]
fn dropping_the_store_leaves_no_wal() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("store.db");
    let wal = dir.path().join("store.db-wal");
    let shm = dir.path().join("store.db-shm");
    let events = fixture_events();
    let store = open(&path, both()).expect("open");
    store.append(&events).expect("append");
    // Both connections in use.
    assert_eq!(tables(&store), expected(&events));
    assert_eq!(store.since(0, 100).expect("since").len(), events.len());
    assert!(wal.exists(), "the store runs in WAL mode");
    drop(store);
    assert!(!wal.exists(), "-wal left behind");
    assert!(!shm.exists(), "-shm left behind");

    // So the .db alone holds everything.
    let copy = dir.path().join("copy.db");
    std::fs::copy(&path, &copy).expect("copy");
    let store = open(&copy, both()).expect("open the copy");
    assert_eq!(store.latest_rev().expect("rev"), 15);
    assert_eq!(tables(&store), expected(&events));
}

#[test]
fn projection_names_are_checked() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("store.db");
    let err = open(
        &path,
        vec![Box::new(CountBy::types()), Box::new(CountBy::types())],
    )
    .expect_err("duplicate");
    assert!(
        matches!(&err, Error::DuplicateProjection { name } if name == "toy.by_type"),
        "{err:?}"
    );
    let store = open(&path, both()).expect("open");
    let err = store.rebuild("nope").expect_err("unknown");
    assert!(matches!(err, Error::UnknownProjection { .. }), "{err:?}");
}

#[test]
fn log_id_is_stable_and_unique() {
    let dir = tempfile::tempdir().expect("tempdir");
    let a = dir.path().join("a.db");
    let b = dir.path().join("b.db");
    let id_a = Store::open(&a, StoreOptions::default())
        .expect("open a")
        .log_id()
        .to_owned();
    assert_eq!(id_a.len(), 26);
    assert!(id_a.parse::<EventId>().is_ok(), "{id_a} is a ULID");
    let reopened = Store::open(&a, StoreOptions::default()).expect("reopen a");
    assert_eq!(reopened.log_id(), id_a);
    drop(reopened);
    let id_b = Store::open(&b, StoreOptions::default())
        .expect("open b")
        .log_id()
        .to_owned();
    assert_ne!(id_a, id_b);

    // A store written before log ids existed gets one on its next open, and keeps it.
    sql::Connection::open(&a)
        .expect("raw")
        .execute("DELETE FROM meta WHERE key = 'log_id'", [])
        .expect("delete");
    let id_new = Store::open(&a, StoreOptions::default())
        .expect("reopen")
        .log_id()
        .to_owned();
    assert_ne!(id_new, id_a);
    assert_eq!(
        Store::open(&a, StoreOptions::default())
            .expect("reopen")
            .log_id(),
        id_new
    );
}

#[test]
fn reads_are_read_only_and_see_commits() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = open(&dir.path().join("store.db"), both()).expect("open");
    store.append(&fixture_events()[..3]).expect("append");
    let n: i64 = store
        .read(|c| -> pitcrew_store::Result<i64> {
            Ok(c.query_row("SELECT SUM(n) FROM toy_by_type", [], |r| r.get(0))?)
        })
        .expect("read");
    assert_eq!(n, 3);
    let err = store
        .read(|c| -> pitcrew_store::Result<usize> { Ok(c.execute("DELETE FROM toy_by_type", [])?) })
        .expect_err("read-only");
    assert!(matches!(err, Error::Database(_)), "{err:?}");
    assert_eq!(table(&store, "toy_by_type").len(), 3);
}

#[test]
fn reads_do_not_wait_for_a_writer() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("store.db");
    let events = fixture_events();
    // No busy wait: anything that needs a lock another connection holds fails at once.
    let mut options = StoreOptions::default();
    options.busy_timeout = Duration::ZERO;
    let store =
        Store::open_with_migrations(&path, options, &toy_migrations(), both()).expect("open");
    store.append(&events[..3]).expect("append");
    let before = tables(&store);

    let writer = sql::Connection::open(&path).expect("raw");
    writer
        .execute_batch(
            "BEGIN IMMEDIATE;
             INSERT INTO toy_by_type (key, n, last) VALUES ('uncommitted', 1, 1);",
        )
        .expect("write");
    // The write lock is really held: an append cannot take it.
    let err = store.append(&events[3..4]).expect_err("locked");
    let Error::Database(db) = &err else {
        panic!("{err:?}")
    };
    assert_eq!(
        db.as_sql().sqlite_error_code(),
        Some(sql::ErrorCode::DatabaseBusy)
    );
    // Reads go on, without the uncommitted row.
    assert_eq!(tables(&store), before);
    assert_eq!(store.since(0, 10).expect("since").len(), 3);
    let back = store
        .before(u64::MAX, 10, &EventFilter::default())
        .expect("before");
    assert_eq!(back.len(), 3);

    writer.execute_batch("COMMIT").expect("commit");
    assert!(
        table(&store, "toy_by_type")
            .iter()
            .any(|row| row.0 == "uncommitted")
    );
}

#[test]
fn a_read_is_one_snapshot() {
    let dir = tempfile::tempdir().expect("tempdir");
    let events = fixture_events();
    let store = open(&dir.path().join("store.db"), both()).expect("open");
    store.append(&events[..3]).expect("append");
    // Events counted, the projection's checkpoint, the log's last revision.
    let state = |c: &sql::Connection| -> sql::Result<(i64, i64, i64)> {
        c.query_row(
            "SELECT (SELECT SUM(n) FROM toy_by_type),
                    (SELECT rev FROM projection_state WHERE name = 'toy.by_type'),
                    (SELECT MAX(rev) FROM events)",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
    };
    let seen = store
        .read(|c| -> pitcrew_store::Result<_> {
            let first = state(c)?;
            store.append(&events[3..6])?;
            let latest = store.latest_rev()?;
            let second = state(c)?;
            Ok((first, latest, second))
        })
        .expect("read");
    // The append committed during the read, which kept seeing revision 3.
    assert_eq!(seen, ((3, 3, 3), 6, (3, 3, 3)));
    let after = store
        .read(|c| -> pitcrew_store::Result<_> { Ok(state(c)?) })
        .expect("read");
    assert_eq!(after, (6, 6, 6));
}

/// Parks `reset` until the test lets it go, once armed.
struct Gate {
    armed: AtomicBool,
    /// The rebuild and the test: the first wait says the rebuild holds the write lock, the second
    /// lets it finish.
    barrier: Barrier,
}

struct Gated(Arc<Gate>);

impl Projection for Gated {
    fn name(&self) -> &str {
        "gate"
    }

    fn version(&self) -> u32 {
        1
    }

    fn reset(&self, _tx: &Transaction<'_>) -> Result<(), BoxError> {
        if self.0.armed.load(Ordering::SeqCst) {
            self.0.barrier.wait();
            self.0.barrier.wait();
        }
        Ok(())
    }

    fn apply(&self, _tx: &Transaction<'_>, _event: &StoredEvent) -> Result<(), BoxError> {
        Ok(())
    }
}

#[test]
fn log_reads_do_not_wait_for_a_rebuild() {
    let dir = tempfile::tempdir().expect("tempdir");
    let gate = Arc::new(Gate {
        armed: AtomicBool::new(false),
        barrier: Barrier::new(2),
    });
    let store = open(
        &dir.path().join("store.db"),
        vec![
            Box::new(CountBy::types()),
            Box::new(Gated(Arc::clone(&gate))),
        ],
    )
    .expect("open");
    let store = Arc::new(store);
    store.append(&fixture_events()[..3]).expect("append");

    gate.armed.store(true, Ordering::SeqCst);
    let rebuild = {
        let store = Arc::clone(&store);
        std::thread::spawn(move || store.rebuild("gate"))
    };
    gate.barrier.wait();
    // The rebuild now holds the write connection until the gate opens. Read on another thread,
    // so a read that waits for it fails the test instead of hanging it.
    let (tx, rx) = mpsc::channel();
    {
        let store = Arc::clone(&store);
        std::thread::spawn(move || {
            let since = store.since(0, 10).map(|page| page.len());
            let before = store
                .before(u64::MAX, 10, &EventFilter::default())
                .map(|page| page.len());
            let read = store.read(|c| -> pitcrew_store::Result<i64> {
                Ok(c.query_row("SELECT SUM(n) FROM toy_by_type", [], |r| r.get(0))?)
            });
            let _ = tx.send((since, before, read));
        });
    }
    let got = rx.recv_timeout(Duration::from_secs(10));
    gate.barrier.wait();
    rebuild.join().expect("thread").expect("rebuild");
    let (since, before, read) = got.expect("the reads waited for the rebuild");
    assert_eq!(since.expect("since"), 3);
    assert_eq!(before.expect("before"), 3);
    assert_eq!(read.expect("read"), 3);
}
