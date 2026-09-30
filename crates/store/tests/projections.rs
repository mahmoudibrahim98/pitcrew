mod common;

use common::{CountBy, both, by_author, by_type, toy_migrations};
use pitcrew_protocol::events::Event;
use pitcrew_protocol::ids::EventId;
use pitcrew_store::sql;
use pitcrew_store::{Error, Projection, RevRange, Store, StoreOptions, StoredEvent};
use std::collections::BTreeMap;
use std::path::Path;
fn open(path: &Path, projections: Vec<Box<dyn Projection>>) -> Result<Store, Error> {
    Store::open_with_migrations(
        path,
        StoreOptions::default(),
        &toy_migrations(),
        projections,
    )
}

type Table = Vec<(String, i64, i64)>;

fn table(store: &Store, name: &str) -> Table {
    store
        .read(|conn| -> pitcrew_store::Result<Table> {
            let mut stmt =
                conn.prepare(&format!("SELECT key, n, last FROM {name} ORDER BY key"))?;
            let rows = stmt
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<sql::Result<_>>()?;
            Ok(rows)
        })
        .expect("read")
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
/// direct count, a rebuild, and a store that only met the projections after the fact.
fn incremental_equals_rebuilt(events: &[Event], batch: usize) {
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
    incremental_equals_rebuilt(&fixture_events(), 4);
}

#[test]
fn generated_50k_incremental_equals_rebuilt() {
    // More than one replay batch, and batches that do not divide it evenly.
    incremental_equals_rebuilt(&generated_events(50_000), 333);
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
    let store = open(&path, vec![Box::new(picky), Box::new(CountBy::authors())]).expect("open");
    let mut rx = store.subscribe();
    store.append(&events[..2]).expect("append");
    let before = tables(&store);

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
