//! `StoreOptions::fs`: `Auto` (detection), and the forced modes. Detection's own type-mapping
//! unit tests live in `src/fs_kind.rs` (white-box: they call private helpers directly).

mod common;

use common::{both, toy_migrations};
use pitcrew_store::{FsMode, Store, StoreOptions};
use std::time::Duration;

fn pragma(store: &Store, name: &str) -> String {
    store
        .read(|c| -> pitcrew_store::Result<String> {
            Ok(c.query_row(&format!("PRAGMA {name}"), [], |r| r.get(0))?)
        })
        .expect("read pragma")
}

#[test]
fn auto_mode_picks_wal_on_this_machines_temp_dir() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open(dir.path().join("store.db"), StoreOptions::default()).expect("open");
    assert_eq!(pragma(&store, "journal_mode"), "wal");
}

#[test]
fn forced_local_mode_uses_wal_even_if_detection_would_disagree() {
    let dir = tempfile::tempdir().expect("tempdir");
    // `StoreOptions` is `#[non_exhaustive]`: outside its crate, build it by mutating `default()`.
    let mut options = StoreOptions::default();
    options.fs = FsMode::Local;
    let store = Store::open(dir.path().join("store.db"), options).expect("open");
    assert_eq!(pragma(&store, "journal_mode"), "wal");
}

#[test]
fn forced_network_mode_uses_delete_and_exclusive_locking() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut options = StoreOptions::default();
    options.fs = FsMode::Network;
    options.lease_ttl = Duration::from_secs(30);
    let store = Store::open(dir.path().join("store.db"), options).expect("open");
    assert_eq!(pragma(&store, "journal_mode"), "delete");
    assert_eq!(pragma(&store, "locking_mode"), "exclusive");
}

#[test]
fn forced_network_mode_appends_reads_and_runs_projections() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("store.db");
    let mut options = StoreOptions::default();
    options.fs = FsMode::Network;
    options.lease_ttl = Duration::from_secs(30);
    let store =
        Store::open_with_migrations(&path, options, &toy_migrations(), both()).expect("open");
    let events = pitcrew_fixtures::demo_workspace().expect("fixture").events;
    store.append(&events).expect("append");

    assert_eq!(store.since(0, 1000).expect("since").len(), events.len());
    let n: i64 = store
        .read(|c| -> pitcrew_store::Result<i64> {
            Ok(c.query_row("SELECT SUM(n) FROM toy_by_type", [], |r| r.get(0))?)
        })
        .expect("read");
    assert_eq!(n, i64::try_from(events.len()).expect("fits"));
}

#[test]
fn local_mode_takes_no_lease_file() {
    // Sanity check for the other tests' assumption about the lease file's name and location.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("store.db");
    let lease_path = dir.path().join("store.db.lease");
    let store = Store::open(&path, StoreOptions::default()).expect("open (local, auto)");
    assert!(
        !lease_path.exists(),
        "local mode must not create a lease file"
    );
    drop(store);
}
