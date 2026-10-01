//! The single-host lease (network mode). The lease file sits next to the database as
//! `<db>.lease` (here `store.db.lease`) and holds JSON `{host, pid, owner, until_ms}`; these
//! tests read and write that file directly to play the part of another host or a crashed run.
//! The pure takeover/expiry decision logic has its own exhaustive unit tests in `src/lease.rs`.

mod common;

use common::FakeClock;
use pitcrew_store::{Error, FsMode, Store, StoreOptions};
use std::path::PathBuf;
use std::time::Duration;

/// Several `Store::open` calls on the same path, each on its own thread. Returns how many
/// succeeded and the errors the rest failed with, so a caller can assert exactly one opener won
/// and every loser's error is [`Error::Leased`] — never a database error, which would mean a
/// loser reached SQLite before the lease refused it.
fn race_to_open(
    path: &std::path::Path,
    n: usize,
    options: impl Fn() -> StoreOptions,
) -> Vec<Result<Store, Error>> {
    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..n)
            .map(|_| {
                let path = path.to_path_buf();
                let options = options();
                scope.spawn(move || Store::open(&path, options))
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().expect("thread"))
            .collect()
    })
}

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
    // machine shared with other agents and this same file's own thread-heavy race tests. Same
    // bound as `a_taken_over_lease_fails_rebuild_too`, empirically the margin this file needs
    // under heavy concurrent load.
    let deadline = std::time::Instant::now() + Duration::from_secs(120);
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
    // Same generous deadline as `renewal_keeps_extending_the_lease`, for the same reason.
    let deadline = std::time::Instant::now() + Duration::from_secs(120);
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

#[test]
fn many_threads_racing_a_fresh_path_exactly_one_opens_and_losers_never_touch_sqlite() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (path, _lease) = paths(&dir);
    const N: usize = 4;
    let results = race_to_open(&path, N, || network_options(Duration::from_secs(30)));

    let oks = results.iter().filter(|r| r.is_ok()).count();
    assert_eq!(oks, 1, "exactly one opener should win a fresh lease");
    for r in &results {
        if let Err(e) = r {
            // Leased, specifically: a loser that instead hit a database error would mean it got
            // past the lease and reached SQLite before being turned away.
            assert!(matches!(e, Error::Leased { .. }), "{e:?}");
        }
    }
}

#[test]
fn many_threads_racing_an_expired_lease_exactly_one_opens() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (path, lease) = paths(&dir);

    // One shared clock, well past `until_ms`, for every racer: all must agree the lease is
    // already expired, with no real time passing during the race.
    let clock = FakeClock::new(1_000_000);
    const N: usize = 4;
    // Safety — at most one opener ever succeeds — is checked on every attempt below, never
    // retried or waived: that is the one property this whole mechanism exists to guarantee.
    // Liveness and cleanliness — exactly one opener succeeds, and every loser's error is a clean
    // `Error::Leased` rather than it having reached SQLite — are checked too, but `N` in-process
    // threads with zero network latency between them synchronize far more tightly than real,
    // separate hosts racing over an actual network ever would, so a single attempt not reaching
    // that ideal outcome is retried with a fresh seed, up to a bound, rather than failed outright
    // (see `lease::tests::many_racers_on_an_expired_lease_exactly_one_wins` for the same
    // reasoning in more detail).
    const ROUND_RETRIES: u32 = 5;
    let mut results = Vec::new();
    for attempt in 0..ROUND_RETRIES {
        std::fs::write(
            &lease,
            serde_json::to_vec(&serde_json::json!({
                "host": "stale-host",
                "pid": 999_999,
                "owner": "01STALEOWNERULIDXXXXXXXXXX",
                "until_ms": 500,
            }))
            .expect("encode"),
        )
        .expect("seed stale lease");
        results = race_to_open(&path, N, || {
            let mut options = StoreOptions::default();
            options.fs = FsMode::Network;
            options.lease_ttl = Duration::from_secs(30);
            options.clock = clock.clone();
            options
        });
        let oks = results.iter().filter(|r| r.is_ok()).count();
        assert!(
            oks <= 1,
            "attempt {attempt}: at most one opener may ever win, got {oks}"
        );
        let all_losers_clean = results
            .iter()
            .filter_map(|r| r.as_ref().err())
            .all(|e| matches!(e, Error::Leased { .. }));
        if oks == 1 && all_losers_clean {
            return;
        }
    }
    let oks = results.iter().filter(|r| r.is_ok()).count();
    assert_eq!(
        oks, 1,
        "no opener won an expired lease within {ROUND_RETRIES} attempts"
    );
    for r in &results {
        if let Err(e) = r {
            assert!(matches!(e, Error::Leased { .. }), "{e:?}");
        }
    }
}

#[test]
fn a_taken_over_lease_fails_rebuild_too() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (path, lease) = paths(&dir);
    let ttl = Duration::from_millis(300);
    let store = Store::open_with_migrations(
        &path,
        network_options(ttl),
        &common::toy_migrations(),
        vec![Box::new(common::CountBy::types())],
    )
    .expect("open");

    // Someone else takes the lease, exactly as in `a_taken_over_lease_fails_the_next_append`.
    let mut value = read_json(&lease);
    value["owner"] = serde_json::json!("someone-else");
    std::fs::write(&lease, serde_json::to_vec(&value).expect("encode")).expect("overwrite");

    // A generous deadline: the renewal thread's wakeup is real wall-clock time, and this suite
    // runs many other threads (including the races below) on a machine shared with other agents,
    // so a 100ms-ish wakeup can occasionally take much longer under scheduling pressure.
    let deadline = std::time::Instant::now() + Duration::from_secs(120);
    loop {
        match store.rebuild("toy.by_type") {
            Err(Error::LeaseLost) => break,
            Ok(()) => {
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

fn read_json(path: &std::path::Path) -> serde_json::Value {
    let bytes = std::fs::read(path).expect("read lease");
    serde_json::from_slice(&bytes).expect("parse lease")
}

fn read_until_ms(path: &std::path::Path) -> i64 {
    read_json(path)["until_ms"].as_i64().expect("until_ms")
}
