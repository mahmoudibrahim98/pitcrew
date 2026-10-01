//! The single-host lease (network mode). Lease files sit next to the database as
//! `<db>.lease.<gen>` (here `store.db.lease.<gen>`), a `u64` generation counter; the current
//! lease is whichever generation is highest. These tests write generation files directly to play
//! the part of another host, a crashed run, or a takeover in progress. The pure takeover/expiry
//! decision logic, and the generation bookkeeping (listing, parsing, GC) has its own exhaustive
//! unit tests in `src/lease.rs`.

mod common;

use common::FakeClock;
use pitcrew_store::{Error, FsMode, Store, StoreOptions};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Several `Store::open` calls on the same path, each on its own thread. Returns every result, so
/// a caller can assert exactly one opener won and every loser's error is [`Error::Leased`] —
/// never a database error, which would mean a loser reached SQLite before the lease refused it.
fn race_to_open(
    path: &Path,
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

fn db_path(dir: &tempfile::TempDir) -> PathBuf {
    dir.path().join("store.db")
}

/// Generation `generation`'s lease file path, mirroring `pitcrew_store`'s own (private) naming so
/// these tests can seed, inspect and simulate takeovers directly.
fn gen_file(dir: &tempfile::TempDir, generation: u64) -> PathBuf {
    dir.path().join(format!("store.db.lease.{generation}"))
}

fn write_gen(dir: &tempfile::TempDir, generation: u64, host: &str, pid: u32, until_ms: i64) {
    let value = serde_json::json!({ "host": host, "pid": pid, "until_ms": until_ms });
    std::fs::write(
        gen_file(dir, generation),
        serde_json::to_vec(&value).expect("encode"),
    )
    .expect("write gen file");
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
    let path = db_path(&dir);
    let first = Store::open(&path, network_options(Duration::from_secs(30))).expect("first open");
    let err =
        Store::open(&path, network_options(Duration::from_secs(30))).expect_err("must refuse");
    assert!(matches!(err, Error::Leased { .. }), "{err:?}");
    drop(first);
}

#[test]
fn dropping_the_store_releases_the_lease() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = db_path(&dir);
    let store = Store::open(&path, network_options(Duration::from_secs(30))).expect("open");
    let gen1 = gen_file(&dir, 1);
    assert!(gen1.exists(), "open must write generation 1's lease file");
    drop(store);
    assert!(!gen1.exists(), "drop must release the lease file");
}

#[test]
fn an_expired_lease_is_taken_over_at_the_next_generation() {
    // A lease a crashed process left behind: a real crash would also close its SQLite connection
    // (releasing the OS-level lock network mode's `locking_mode=EXCLUSIVE` takes), which a
    // `mem::forget` of a live `Store` in this same process would not simulate correctly — so the
    // file is written directly, as the real file would look once that connection is gone.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = db_path(&dir);
    write_gen(&dir, 1, "another-host", 123_456, 500);

    // The clock `Store::open` is given already reads past `until_ms`, with no real time passed.
    let mut options = StoreOptions::default();
    options.fs = FsMode::Network;
    options.lease_ttl = Duration::from_secs(60);
    options.clock = FakeClock::new(1_000);
    let store = Store::open(&path, options).expect("takes over the expired lease");
    assert!(
        gen_file(&dir, 1).exists(),
        "the old generation's file is left untouched by a takeover"
    );
    assert!(
        gen_file(&dir, 2).exists(),
        "the takeover creates the next generation, not a fresh generation 1"
    );
    drop(store);
}

#[test]
fn renewal_keeps_extending_the_lease() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = db_path(&dir);
    let gen1 = gen_file(&dir, 1);
    let ttl = Duration::from_millis(300);
    let store = Store::open(&path, network_options(ttl)).expect("open");
    let initial_until = read_until_ms(&gen1);

    // The renewal thread wakes roughly every ttl/3 (~100ms here); give it generous room on a
    // machine shared with other agents and this same file's own thread-heavy race tests.
    let deadline = std::time::Instant::now() + Duration::from_secs(120);
    loop {
        if read_until_ms(&gen1) > initial_until {
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
fn a_taken_over_lease_fails_the_next_append_immediately() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = db_path(&dir);
    let store = Store::open(&path, network_options(Duration::from_secs(60))).expect("open");
    assert!(gen_file(&dir, 1).exists());

    // Someone else takes over: a higher generation appears. `check_lease` re-lists on every
    // write, so this is caught on the very next call — no sleep loop, no deadline, unlike the
    // old single-fixed-file design where only the renewal thread's own periodic wakeup (up to
    // `ttl / RENEW_FRACTION` later) could notice.
    write_gen(&dir, 2, "someone-else", 999_999, 999_999_999);

    let fixture = pitcrew_fixtures::demo_workspace().expect("fixture").events;
    let event = fixture[0].clone();
    let err = store
        .append(std::slice::from_ref(&event))
        .expect_err("a displaced owner's next write must fail at once");
    assert!(matches!(err, Error::LeaseLost), "{err:?}");
}

#[test]
fn a_taken_over_lease_fails_rebuild_immediately() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = db_path(&dir);
    let store = Store::open_with_migrations(
        &path,
        network_options(Duration::from_secs(60)),
        &common::toy_migrations(),
        vec![Box::new(common::CountBy::types())],
    )
    .expect("open");

    write_gen(&dir, 2, "someone-else", 999_999, 999_999_999);

    let err = store
        .rebuild("toy.by_type")
        .expect_err("a displaced owner's rebuild must fail at once");
    assert!(matches!(err, Error::LeaseLost), "{err:?}");
}

#[test]
fn a_fresh_garbage_lease_file_refuses_but_a_stale_one_is_taken_over() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = db_path(&dir);
    let gen1 = gen_file(&dir, 1);
    std::fs::write(&gen1, b"not json at all").expect("write garbage");

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
        .open(&gen1)
        .expect("open for mtime");
    file.set_modified(old).expect("set_modified");
    drop(file);

    let store =
        Store::open(&path, network_options(ttl)).expect("a stale garbage file is taken over");
    assert!(gen_file(&dir, 2).exists(), "takeover creates generation 2");
    drop(store);
}

#[test]
fn many_threads_racing_a_fresh_path_exactly_one_opens_and_losers_never_touch_sqlite() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = db_path(&dir);
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
    let path = db_path(&dir);

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
    // that ideal outcome is retried with a fresh seed, up to a bound, rather than failed outright.
    const ROUND_RETRIES: u32 = 5;
    let mut results = Vec::new();
    for attempt in 0..ROUND_RETRIES {
        write_gen(&dir, 1, "stale-host", 999_999, 500);
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
        drop(std::mem::take(&mut results)); // release the winner, if any, before reseeding
        clean_lease_dir(&dir);
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

/// A direct regression test for review round 2's finding, through the full `Store::open` path
/// (not just `LeaseGuard::acquire` directly, which `lease::tests::three_racers_on_an_expired_lease_never_give_two_holders`
/// already covers): with three racers contending for one expired lease, at most one `Store` may
/// ever successfully open. Safety, checked on every attempt, no retries or exceptions — this is
/// exactly the three-way interleaving (a straggler's stale decision displacing an already-won
/// lease, a third racer filling the gap) that broke the old move-aside design.
#[test]
fn three_threads_racing_an_expired_lease_never_give_two_holders() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = db_path(&dir);
    let clock = FakeClock::new(1_000_000);
    const N: usize = 3;
    const ROUNDS: u32 = 10;
    for round in 0..ROUNDS {
        write_gen(&dir, 1, "stale-host", 999_999, 500);
        let results = race_to_open(&path, N, || {
            let mut options = StoreOptions::default();
            options.fs = FsMode::Network;
            options.lease_ttl = Duration::from_secs(30);
            options.clock = clock.clone();
            options
        });
        let oks = results.iter().filter(|r| r.is_ok()).count();
        assert!(
            oks <= 1,
            "round {round}: at most one opener may ever hold the lease, got {oks}"
        );
        drop(results);
        clean_lease_dir(&dir);
    }
}

#[test]
fn opening_over_several_stale_generations_gcs_down_to_the_current_and_previous() {
    // `pitcrew_store::lease`'s own unit tests already exercise `gc_old_generations` directly
    // (seeding generations and asserting exactly which survive); this is the same guarantee seen
    // through `Store::open`, simulating generations 1-3 as prior, now-expired holders (crashed or
    // long gone, never released — a real release would delete its own file, leaving nothing for
    // GC to find, which is why this seeds them directly rather than cycling real `Store::open`
    // and `drop` calls) so that one open's takeover has real history to collect.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = db_path(&dir);
    for generation in 1..=3 {
        write_gen(&dir, generation, "stale-host", 999_999, 500);
    }

    let mut options = StoreOptions::default();
    options.fs = FsMode::Network;
    options.lease_ttl = Duration::from_secs(60);
    options.clock = FakeClock::new(1_000_000);
    let store = Store::open(&path, options).expect("takes over at generation 4");

    assert!(gen_file(&dir, 4).exists(), "the new current generation");
    assert!(gen_file(&dir, 3).exists(), "kept: current - 1");
    assert!(!gen_file(&dir, 2).exists(), "gc'd: older than current - 1");
    assert!(!gen_file(&dir, 1).exists(), "gc'd: older than current - 1");
    drop(store);
}

/// Removes every generation file in `dir`'s lease family, for tests that run several independent
/// rounds of the same race and need each round to start from a clean slate.
fn clean_lease_dir(dir: &tempfile::TempDir) {
    for entry in std::fs::read_dir(dir.path()).expect("read_dir").flatten() {
        let name = entry.file_name();
        if name.to_string_lossy().starts_with("store.db.lease.") {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

fn read_until_ms(path: &Path) -> i64 {
    let bytes = std::fs::read(path).expect("read lease");
    let value: serde_json::Value = serde_json::from_slice(&bytes).expect("parse lease");
    value["until_ms"].as_i64().expect("until_ms")
}
