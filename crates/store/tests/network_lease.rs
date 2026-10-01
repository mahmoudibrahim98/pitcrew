//! The single-host lease (network mode). The lease file sits next to the database as
//! `<db>.lease` (here `store.db.lease`) and holds JSON `{host, pid, owner, until_ms}`; these
//! tests read and write that file directly to play the part of another host or a crashed run.
//! The pure takeover/expiry decision logic has its own exhaustive unit tests in `src/lease.rs`.

mod common;

use common::FakeClock;
use pitcrew_store::{Error, FsMode, Store, StoreOptions};
use std::path::PathBuf;
use std::time::Duration;

fn paths(dir: &tempfile::TempDir) -> (PathBuf, PathBuf) {
    (
        dir.path().join("store.db"),
        dir.path().join("store.db.lease"),
    )
}

// `StoreOptions` is `#[non_exhaustive]`, so outside its own crate it can only be built by
// mutating a `default()`, not with `..` struct-update syntax.
fn network_options(ttl: Duration) -> StoreOptions {
    let mut options = StoreOptions::default();
    options.fs = FsMode::Network;
    options.lease_ttl = ttl;
    options
}

#[test]
fn a_second_open_in_network_mode_fails_with_leased() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (path, _lease) = paths(&dir);
    let first = Store::open(&path, network_options(Duration::from_secs(30))).expect("first open");
    let err =
        Store::open(&path, network_options(Duration::from_secs(30))).expect_err("must refuse");
    assert!(matches!(err, Error::Leased { .. }), "{err:?}");
    drop(first);
}

#[test]
fn dropping_the_store_releases_the_lease() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (path, lease) = paths(&dir);
    let store = Store::open(&path, network_options(Duration::from_secs(30))).expect("open");
    assert!(lease.exists(), "open must write the lease file");
    drop(store);
    assert!(!lease.exists(), "drop must release the lease file");
}

#[test]
fn an_expired_lease_is_taken_over_using_the_injected_clock() {
    // A lease a crashed process left behind: a real crash would also close its SQLite connection
    // (releasing the OS-level lock network mode's `locking_mode=EXCLUSIVE` takes), which a
    // `mem::forget` of a live `Store` in this same process would not simulate correctly — so the
    // file is written directly, as the real file would look once that connection is gone.
    let dir = tempfile::tempdir().expect("tempdir");
    let (path, lease) = paths(&dir);
    std::fs::write(
        &lease,
        serde_json::to_vec(&serde_json::json!({
            "host": "another-host",
            "pid": 123_456,
            "owner": "01STALEOWNERULIDXXXXXXXXXX",
            "until_ms": 500,
        }))
        .expect("encode"),
    )
    .expect("write stale lease");

    // The clock `Store::open` is given already reads past `until_ms`, with no real time passed.
    let mut options = StoreOptions::default();
    options.fs = FsMode::Network;
    options.lease_ttl = Duration::from_secs(60);
    options.clock = FakeClock::new(1_000);
    let store = Store::open(&path, options).expect("takes over the expired lease");
    drop(store);
}

#[test]
fn renewal_keeps_extending_the_lease() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (path, lease) = paths(&dir);
    let ttl = Duration::from_millis(300);
    let store = Store::open(&path, network_options(ttl)).expect("open");
    let initial_until = read_until_ms(&lease);

    // The renewal thread wakes roughly every ttl/3 (~100ms here); give it generous room on a
    // possibly busy machine.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        if read_until_ms(&lease) > initial_until {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "lease was never renewed"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    drop(store);
}

#[test]
fn a_taken_over_lease_fails_the_next_append() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (path, lease) = paths(&dir);
    let ttl = Duration::from_millis(300);
    let store = Store::open(&path, network_options(ttl)).expect("open");

    // Someone else takes the lease: same shape, a different owner. Contents otherwise do not
    // matter, since the renewal thread only checks the owner before it writes.
    let mut value = read_json(&lease);
    value["owner"] = serde_json::json!("someone-else");
    std::fs::write(&lease, serde_json::to_vec(&value).expect("encode")).expect("overwrite");

    let fixture = pitcrew_fixtures::demo_workspace().expect("fixture").events;
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let mut event = fixture[0].clone();
        event.id = pitcrew_protocol::ids::EventId::new();
        match store.append(std::slice::from_ref(&event)) {
            Err(Error::LeaseLost) => break,
            Ok(_) => {
                assert!(
                    std::time::Instant::now() < deadline,
                    "LeaseLost was never observed"
                );
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => panic!("unexpected error: {e:?}"),
        }
    }
}

#[test]
fn a_fresh_garbage_lease_file_refuses_but_a_stale_one_is_taken_over() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (path, lease) = paths(&dir);
    std::fs::write(&lease, b"not json at all").expect("write garbage");

    // Generous ttl: the point is that a *fresh* garbage file refuses regardless of how long the
    // open itself takes.
    let ttl = Duration::from_secs(2);
    let err =
        Store::open(&path, network_options(ttl)).expect_err("a fresh garbage file must refuse");
    assert!(matches!(err, Error::Leased { .. }), "{err:?}");

    // Age the file past the lease length, independent of real elapsed time.
    let old = std::time::SystemTime::now() - ttl - Duration::from_secs(1);
    let file = std::fs::File::options()
        .write(true)
        .open(&lease)
        .expect("open for mtime");
    file.set_modified(old).expect("set_modified");
    drop(file);

    let store =
        Store::open(&path, network_options(ttl)).expect("a stale garbage file is taken over");
    drop(store);
}

fn read_json(path: &std::path::Path) -> serde_json::Value {
    let bytes = std::fs::read(path).expect("read lease");
    serde_json::from_slice(&bytes).expect("parse lease")
}

fn read_until_ms(path: &std::path::Path) -> i64 {
    read_json(path)["until_ms"].as_i64().expect("until_ms")
}
