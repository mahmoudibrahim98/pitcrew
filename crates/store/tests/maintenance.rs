//! Snapshots, integrity checks, and export/import.

mod common;

use common::{both, toy_migrations};
use pitcrew_protocol::events::Event;
use pitcrew_store::{IntegrityReport, Store, StoreOptions};
use std::path::Path;

fn fixture_events() -> Vec<Event> {
    pitcrew_fixtures::demo_workspace().expect("fixture").events
}

fn open(path: &Path) -> Store {
    Store::open_with_migrations(path, StoreOptions::default(), &toy_migrations(), both())
        .expect("open")
}

type Table = Vec<(String, i64, i64)>;

fn table(store: &Store, name: &str) -> Table {
    store
        .read(|c| -> pitcrew_store::Result<Table> {
            let mut stmt = c.prepare(&format!("SELECT key, n, last FROM {name} ORDER BY key"))?;
            Ok(stmt
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<rusqlite::Result<Table>>()?)
        })
        .expect("read")
}

fn tables(store: &Store) -> (Table, Table) {
    (table(store, "toy_by_type"), table(store, "toy_by_author"))
}

#[test]
fn a_snapshot_opens_with_identical_events_and_projections() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = open(&dir.path().join("store.db"));
    store.append(&fixture_events()).expect("append");
    let before_events = store.since(0, 1000).expect("since");
    let before_tables = tables(&store);

    let snap = dir.path().join("snapshot.db");
    store.snapshot(&snap).expect("snapshot");

    // The live store is still usable afterwards.
    assert_eq!(store.since(0, 1000).expect("since"), before_events);

    let reopened = open(&snap);
    assert_eq!(reopened.since(0, 1000).expect("since"), before_events);
    assert_eq!(tables(&reopened), before_tables);
}

#[test]
fn snapshot_refuses_an_existing_destination() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = open(&dir.path().join("store.db"));
    store.append(&fixture_events()[..1]).expect("append");
    let snap = dir.path().join("snapshot.db");
    std::fs::write(&snap, b"already here").expect("write");
    assert!(store.snapshot(&snap).is_err());
}

#[test]
fn integrity_check_is_ok_for_a_healthy_store() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = open(&dir.path().join("store.db"));
    store.append(&fixture_events()).expect("append");
    assert_eq!(
        store.integrity_check(false).expect("check"),
        IntegrityReport::Ok
    );
    assert_eq!(
        store.integrity_check(true).expect("full check"),
        IntegrityReport::Ok
    );
}

#[test]
fn a_corrupted_copy_fails_integrity_check() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = open(&dir.path().join("store.db"));
    store.append(&fixture_events()).expect("append");
    let snap = dir.path().join("snapshot.db");
    store.snapshot(&snap).expect("snapshot");
    drop(store);

    // Flip every byte past the 100-byte file header: a handful of bytes mid-page can land in
    // live row content without upsetting the b-tree structure itself (quick_check and
    // integrity_check both check structure, not column values), so corrupt broadly enough to
    // guarantee hitting page headers and cell pointer arrays too.
    let mut bytes = std::fs::read(&snap).expect("read snapshot");
    assert!(
        bytes.len() > 4096,
        "snapshot should have more than one page"
    );
    for b in &mut bytes[100..] {
        *b ^= 0xFF;
    }
    std::fs::write(&snap, &bytes).expect("write corrupted");

    let conn = rusqlite::Connection::open(&snap).expect("open corrupted file");
    let quick = pitcrew_store::integrity_check(&conn, false).expect("quick_check");
    assert!(matches!(quick, IntegrityReport::Failed(_)), "{quick:?}");
    let full = pitcrew_store::integrity_check(&conn, true).expect("integrity_check");
    assert!(matches!(full, IntegrityReport::Failed(_)), "{full:?}");
}

#[test]
fn export_then_import_gives_the_same_events_and_projection_tables() {
    let dir = tempfile::tempdir().expect("tempdir");
    let source = open(&dir.path().join("source.db"));
    source.append(&fixture_events()).expect("append");
    let source_tables = tables(&source);

    let mut buf: Vec<u8> = Vec::new();
    source.export(&mut buf).expect("export");

    let target = open(&dir.path().join("target.db"));
    target.import(buf.as_slice()).expect("import");

    let source_events: Vec<Event> = source
        .since(0, 1000)
        .expect("since")
        .into_iter()
        .map(|e| e.event)
        .collect();
    let target_events: Vec<Event> = target
        .since(0, 1000)
        .expect("since")
        .into_iter()
        .map(|e| e.event)
        .collect();
    assert_eq!(source_events, target_events);
    assert_eq!(tables(&target), source_tables);
    // Import starts a new log, independent of the source's.
    assert_ne!(source.log_id(), target.log_id());
}

#[test]
fn import_refuses_a_non_empty_store() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = open(&dir.path().join("store.db"));
    store.append(&fixture_events()[..1]).expect("append");
    let err = store.import(&b""[..]).expect_err("must refuse");
    assert!(matches!(err, pitcrew_store::Error::NotEmpty), "{err:?}");
}

#[test]
fn import_rejects_a_garbage_line() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = open(&dir.path().join("store.db"));
    let err = store.import(&b"not json\n"[..]).expect_err("must refuse");
    assert!(
        matches!(err, pitcrew_store::Error::Corrupt { .. }),
        "{err:?}"
    );
}
