//! Snapshots, integrity checks, and export/import.

mod common;

use common::{both, toy_migrations};
use pitcrew_protocol::events::Event;
use pitcrew_protocol::ids::EventId;
use pitcrew_store::{Error, FsMode, IntegrityReport, Store, StoreOptions};
use rusqlite::OptionalExtension;
use std::path::Path;

fn fixture_events() -> Vec<Event> {
    pitcrew_fixtures::demo_workspace().expect("fixture").events
}

/// `n` events built from the fixture, cycled and given fresh ids so a batch larger than the
/// fixture's own 15 events still has unique ids throughout (as `event_log.rs`'s own
/// `numbered_events` does).
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

/// The export-format text for `events`: one JSON line each, oldest first, as `Store::export`
/// writes it.
fn lines(events: &[Event]) -> String {
    let mut out = String::new();
    for e in events {
        out.push_str(&serde_json::to_string(e).expect("line"));
        out.push('\n');
    }
    out
}

/// `lines(events)` with one extra, undecodable line spliced in at position `at` (0-indexed; `at
/// == events.len()` appends it after every good line). Used to put a bad line at an exact
/// position relative to `Store::import`'s internal batch size (1,000).
fn lines_with_bad_at(events: &[Event], at: usize, bad: &str) -> String {
    let mut out = String::new();
    for (i, e) in events.iter().enumerate() {
        if i == at {
            out.push_str(bad);
            out.push('\n');
        }
        out.push_str(&serde_json::to_string(e).expect("line"));
        out.push('\n');
    }
    if at == events.len() {
        out.push_str(bad);
        out.push('\n');
    }
    out
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

// --- R7: import is whole or nothing (docs/build/briefs/C-import-and-reopen.md) ---

/// A bad line at 1, at 1,000 (the last line of the first internal batch), at 1,001 (the first
/// line of the second) and at the end: every one of these left a prefix behind under the old
/// batch-committing design. Now the whole import is one transaction, so all four leave nothing.
#[test]
fn import_is_whole_or_nothing_for_a_bad_line_at_1_1000_1001_and_the_end() {
    let events = generated_events(1_500);
    for at in [0usize, 999, 1_000, events.len()] {
        let dir = tempfile::tempdir().expect("tempdir");
        let store =
            Store::open(dir.path().join("store.db"), StoreOptions::default()).expect("open");
        let text = lines_with_bad_at(&events, at, "not an event");
        let err = store
            .import(text.as_bytes())
            .expect_err("a bad line must fail the import");
        assert!(matches!(err, Error::Corrupt { .. }), "{err:?} (at {at})");
        assert_eq!(
            store.latest_rev().expect("rev"),
            0,
            "a bad line at position {at} left events behind"
        );
    }
}

/// The exact fuzz finding in `fuzz/regressions/store_import/r7-bad-line-after-a-batch`, read from
/// that file (not retyped, so this test can never quietly drift from what the fuzzer actually
/// found): 1,000 good lines (a full batch under the old design), then one bad line — with a
/// trailing newline, same as every other line (the fixture's own bytes are `[n: u8][file bytes]`,
/// where the fuzz target prepends `n * 8` of its own filler lines before `file`; this test keeps
/// only `file`, the fixture's own trailing bytes, which are what found R7, and supplies its own
/// 1,000 valid lines ahead of them instead of reproducing the fuzz target's exact filler). The old
/// `Store::import` committed the first 1,000 before reaching the bad line, leaving them behind and
/// refusing a retry with `NotEmpty` (R7, `docs/security/threat-model.md`).
#[test]
fn r7_fuzz_regression_a_bad_line_right_after_a_full_batch() {
    let fixture_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fuzz/regressions/store_import/r7-bad-line-after-a-batch");
    let raw = std::fs::read(&fixture_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", fixture_path.display()));
    let (_n, bad_tail) = raw.split_first().expect("the fixture file is not empty");
    let bad_tail = std::str::from_utf8(bad_tail).expect("the fixture's tail is UTF-8");

    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open(dir.path().join("store.db"), StoreOptions::default()).expect("open");
    let events = generated_events(1_000);
    let mut text = lines(&events);
    text.push_str(bad_tail);
    let err = store.import(text.as_bytes()).expect_err("must fail");
    assert!(matches!(err, Error::Corrupt { .. }), "{err:?}");
    assert_eq!(
        store.latest_rev().expect("rev"),
        0,
        "the old design left the first batch of 1,000 behind"
    );
    // A retry with the bad line fixed is accepted — the old design refused it with `NotEmpty`
    // because the first batch's 1,000 events were still there.
    store
        .import(lines(&events).as_bytes())
        .expect("clean import now succeeds");
    assert_eq!(store.latest_rev().expect("rev"), 1_000);
}

/// Proves the rollback is real on disk, not just this process's in-memory view of it: a fresh
/// `Store` opened at the same path after a failed import also reads an empty log, and accepts a
/// clean import afterwards.
#[test]
fn a_failed_import_leaves_nothing_even_after_reopening() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("store.db");
    let events = generated_events(1_200);
    let text = lines_with_bad_at(&events, 1_000, "not an event");
    {
        let store = Store::open(&path, StoreOptions::default()).expect("open");
        let err = store.import(text.as_bytes()).expect_err("must fail");
        assert!(matches!(err, Error::Corrupt { .. }), "{err:?}");
        assert_eq!(store.latest_rev().expect("rev"), 0);
    }
    let reopened = Store::open(&path, StoreOptions::default()).expect("reopen");
    assert_eq!(reopened.latest_rev().expect("rev"), 0);
    reopened
        .import(lines(&events).as_bytes())
        .expect("a clean import now succeeds");
    assert_eq!(reopened.latest_rev().expect("rev"), events.len() as u64);
}

/// A reader that yields its bytes normally up to `fail_after`, then fails with an I/O error —
/// standing in for whatever interrupts an import partway (a cut network transfer, a disk error,
/// a real process crash). The point of the test below is not how the interruption happens, only
/// that nothing it already inserted into the (uncommitted) transaction survives: for a real
/// process crash this is SQLite's own rollback-journal recovery on the next open, which an I/O
/// error reaching the caller cleanly (this struct) cannot itself exercise, but the `Store` side
/// of the contract (never commit on an error) is the same either way.
struct FlakyReader {
    data: Vec<u8>,
    pos: usize,
    fail_after: usize,
}

impl std::io::Read for FlakyReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.pos >= self.fail_after {
            return Err(std::io::Error::other("simulated crash mid-import"));
        }
        let end = (self.pos + buf.len())
            .min(self.fail_after)
            .min(self.data.len());
        let n = end - self.pos;
        buf[..n].copy_from_slice(&self.data[self.pos..end]);
        self.pos = end;
        Ok(n)
    }
}

#[test]
fn an_interrupted_read_rolls_back_the_whole_import() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("store.db");
    let events = generated_events(1_500);
    let text = lines(&events);
    // Cut off partway through the second internal batch: by the time the read fails, the first
    // batch has already been inserted into the (uncommitted) transaction.
    let cut = text.len() * 3 / 4;
    let store = Store::open(&path, StoreOptions::default()).expect("open");
    let reader = std::io::BufReader::new(FlakyReader {
        data: text.into_bytes(),
        pos: 0,
        fail_after: cut,
    });
    let err = store.import(reader).expect_err("must fail");
    assert!(matches!(err, Error::Io(_)), "{err:?}");
    assert_eq!(store.latest_rev().expect("rev"), 0);
    drop(store);

    let reopened = Store::open(&path, StoreOptions::default()).expect("reopen");
    assert_eq!(reopened.latest_rev().expect("rev"), 0);
}

/// A successful import spanning several internal batches (not just the single-batch case the
/// other export/import test already covers) commits once, as one log.
#[test]
fn a_successful_multi_batch_import_commits_as_one_log() {
    let dir = tempfile::tempdir().expect("tempdir");
    let source_events = generated_events(2_500);
    let source = Store::open(dir.path().join("source.db"), StoreOptions::default()).expect("open");
    source.append(&source_events).expect("append");

    let mut buf = Vec::new();
    source.export(&mut buf).expect("export");

    let target = Store::open(dir.path().join("target.db"), StoreOptions::default()).expect("open");
    let mut rx = target.subscribe();
    target.import(buf.as_slice()).expect("import");
    assert_eq!(target.latest_rev().expect("rev"), 2_500);
    let got: Vec<Event> = target
        .since(0, 3_000)
        .expect("since")
        .into_iter()
        .map(|e| e.event)
        .collect();
    assert_eq!(got, source_events);
    // One announcement for the whole import, not one per internal batch (a partial, not-yet-
    // durable range must never reach a subscriber).
    assert_eq!(
        rx.try_recv().expect("one range"),
        pitcrew_store::RevRange {
            from_rev: 1,
            to_rev: 2_500
        }
    );
    assert!(rx.try_recv().is_err());
}

fn projection_checkpoint(store: &Store, name: &str) -> Option<(u32, i64)> {
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

/// A failed import's rollback covers registered projections too, not just the log: a bad line at
/// 1,001 leaves their tables empty and their checkpoints exactly as they were at open (rev 0),
/// not advanced to 1,000 by the first batch that got rolled back with everything else. A clean
/// re-import afterwards gives the same tables as appending the same events directly.
#[test]
fn a_failed_import_rolls_projections_back_with_the_log() {
    let events = generated_events(1_500);
    let dir = tempfile::tempdir().expect("tempdir");
    let target = open(&dir.path().join("target.db"));
    // A fresh store's own open already ran every projection's `reset` once; record that as the
    // "untouched" baseline rather than assuming it is `None`.
    let baseline_type = projection_checkpoint(&target, "toy.by_type");
    let baseline_author = projection_checkpoint(&target, "toy.by_author");

    let text = lines_with_bad_at(&events, 1_000, "not an event");
    let err = target
        .import(text.as_bytes())
        .expect_err("a bad line at 1,001 must fail");
    assert!(matches!(err, Error::Corrupt { .. }), "{err:?}");
    assert_eq!(target.latest_rev().expect("rev"), 0);
    assert_eq!(
        tables(&target),
        (Table::new(), Table::new()),
        "the projections must roll back with the import, not keep the first batch's rows"
    );
    assert_eq!(
        projection_checkpoint(&target, "toy.by_type"),
        baseline_type,
        "the checkpoint must be unchanged, not advanced to 1,000 by the rolled-back first batch"
    );
    assert_eq!(
        projection_checkpoint(&target, "toy.by_author"),
        baseline_author
    );

    // A clean import now matches a source store the same events were appended to directly.
    target
        .import(lines(&events).as_bytes())
        .expect("clean import now succeeds");
    let source = open(&dir.path().join("source.db"));
    source.append(&events).expect("append");
    assert_eq!(tables(&target), tables(&source));
}

/// A reader whose `read` panics partway through — an unwind reached while reading, as opposed to
/// `FlakyReader`'s clean `Err` above. After `catch_unwind` sees the panic, the same `Store` handle
/// must still read 0 and still accept ordinary writes: the `Mutex`es this crate uses recover from
/// being poisoned (`PoisonError::into_inner`, this crate's own convention throughout), and the
/// uncommitted transaction's own `Drop` rolls it back while unwinding.
struct PanicReader {
    data: Vec<u8>,
    pos: usize,
    panic_after: usize,
}

impl std::io::Read for PanicReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.pos >= self.panic_after {
            panic!("simulated panic mid-import");
        }
        let end = (self.pos + buf.len())
            .min(self.panic_after)
            .min(self.data.len());
        let n = end - self.pos;
        buf[..n].copy_from_slice(&self.data[self.pos..end]);
        self.pos = end;
        Ok(n)
    }
}

#[test]
fn a_panic_partway_leaves_the_store_empty_and_still_usable() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("store.db");
    let events = generated_events(1_500);
    let text = lines(&events).into_bytes();
    let cut = text.len() * 3 / 4;
    let store = Store::open(&path, StoreOptions::default()).expect("open");

    let wrapped = std::panic::AssertUnwindSafe(&store);
    let data = text.clone();
    let result = std::panic::catch_unwind(move || {
        // Rebind the whole wrapper first: Rust 2021's precise closure capture would otherwise
        // capture only `wrapped.0` (a bare `&Store`, not `UnwindSafe`) since that is all the body
        // below actually touches, silently stepping around `AssertUnwindSafe`.
        let wrapped = wrapped;
        let reader = std::io::BufReader::new(PanicReader {
            data,
            pos: 0,
            panic_after: cut,
        });
        wrapped.0.import(reader)
    });
    assert!(
        result.is_err(),
        "the panic must propagate out of catch_unwind"
    );

    assert_eq!(store.latest_rev().expect("rev"), 0);
    store
        .append(&events[..3])
        .expect("the store still accepts writes after the panic");

    drop(store);
    let reopened = Store::open(&path, StoreOptions::default()).expect("reopen");
    assert_eq!(
        reopened.latest_rev().expect("rev"),
        3,
        "disk matches the one successful append after the panic"
    );
}

/// `Store::import`'s own emptiness check (`latest_rev(&tx) != 0`, inside the transaction that then
/// does the inserts) sees a row a concurrent writer commits while `import`'s `BEGIN IMMEDIATE` is
/// still waiting for the write lock — it cannot miss it, because nothing else can write once that
/// transaction holds the lock, and the check runs from inside it. A `latest_rev()` read taken
/// before even trying to acquire the lock (the old design) could instead see "empty" and then
/// still go on to write once the lock became free, even though the store was no longer empty by
/// then.
#[test]
fn import_checks_emptiness_under_the_same_lock_it_inserts_with() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("store.db");
    let store = Store::open(&path, StoreOptions::default()).expect("open");

    // Hold the write lock on a second, raw connection, exactly as `reads_do_not_wait_for_a_writer`
    // in tests/projections.rs does, so `import`'s own `BEGIN IMMEDIATE` must wait for it.
    let writer = rusqlite::Connection::open(&path).expect("raw");
    writer.execute_batch("BEGIN IMMEDIATE").expect("begin");

    let text = lines(&generated_events(3));
    let import_thread = std::thread::spawn(move || store.import(text.as_bytes()));
    // Give the blocked `import` call time to actually reach, and wait on, its own
    // `BEGIN IMMEDIATE` before this thread commits a row behind its back.
    std::thread::sleep(std::time::Duration::from_millis(200));

    // Stands in for a concurrent append finishing while `import` is still waiting for the write
    // lock: a plain row is enough here, its content does not matter to this test.
    writer
        .execute(
            "INSERT INTO events (id, at, workspace, author, on_behalf_of, type, data)
             VALUES ('concurrent-1', 0, 'w', 'a', NULL, 'x', '{}')",
            [],
        )
        .expect("insert");
    writer.execute_batch("COMMIT").expect("commit");

    let err = import_thread
        .join()
        .expect("thread")
        .expect_err("import must see the row committed while it waited, not a stale empty check");
    assert!(matches!(err, Error::NotEmpty), "{err:?}");
}

/// A reader that, once it has handed back `trigger_after` bytes, runs `action` once before
/// continuing — used to simulate another host taking over the network-mode lease at a precise
/// point in the stream, right after the first internal batch of 1,000 lines.
struct TriggerReader<F: FnMut()> {
    data: Vec<u8>,
    pos: usize,
    trigger_after: usize,
    action: Option<F>,
}

impl<F: FnMut()> std::io::Read for TriggerReader<F> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.pos >= self.trigger_after
            && let Some(mut action) = self.action.take()
        {
            action();
        }
        let end = (self.pos + buf.len()).min(self.data.len());
        let n = end - self.pos;
        buf[..n].copy_from_slice(&self.data[self.pos..end]);
        self.pos = end;
        Ok(n)
    }
}

/// A takeover that lands after the first batch of 1,000 lines (another host's own exclusive
/// create of the next lease generation) fails the import with `LeaseLost` and leaves the store
/// empty — `check_lease` is called again before every batch's insert and once more right before
/// the final `COMMIT`, not just once up front.
#[test]
fn a_lease_taken_over_mid_import_fails_and_leaves_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("store.db");
    let mut options = StoreOptions::default();
    options.fs = FsMode::Network;
    let store = Store::open(&path, options).expect("open");

    let events = generated_events(1_500);
    let text = lines(&events).into_bytes();
    // Right after the first batch of 1,000 lines, so the takeover lands between batches.
    let trigger_after = lines(&events[..1_000]).len();
    let lease_path = dir.path().join("store.db.lease.2");
    let reader = std::io::BufReader::new(TriggerReader {
        data: text,
        pos: 0,
        trigger_after,
        action: Some(move || {
            let competitor = serde_json::json!({
                "host": "someone-else", "pid": 999_999, "until_ms": 999_999_999_999_i64
            });
            std::fs::write(
                &lease_path,
                serde_json::to_vec(&competitor).expect("encode"),
            )
            .expect("write a competing lease");
        }),
    });

    let err = store
        .import(reader)
        .expect_err("a takeover mid-import must fail");
    assert!(matches!(err, Error::LeaseLost), "{err:?}");
    assert_eq!(store.latest_rev().expect("rev"), 0);
}

/// A reader that calls `std::process::abort()` once it has handed back `abort_after` bytes —
/// simulating a real process crash partway through an import. Used only by
/// `crash_child_import_then_abort`, below, never under a normal `cargo test` run.
#[cfg(unix)]
struct AbortReader {
    data: Vec<u8>,
    pos: usize,
    abort_after: usize,
}

#[cfg(unix)]
impl std::io::Read for AbortReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.pos >= self.abort_after {
            std::process::abort();
        }
        let end = (self.pos + buf.len())
            .min(self.abort_after)
            .min(self.data.len());
        let n = end - self.pos;
        buf[..n].copy_from_slice(&self.data[self.pos..end]);
        self.pos = end;
        Ok(n)
    }
}

/// Not a test of its own: a no-op under a normal `cargo test` run (its env var is unset).
/// `a_real_crash_mid_import_recovers_to_an_empty_store_in_both_modes`, below, spawns this same
/// test binary with it set, via `std::env::current_exe()` and `--exact`, so this runs with the
/// env var present and does the actual crashing. It opens the pre-created, already-migrated store
/// at the given path (in the given mode) and aborts partway through an import: a real,
/// uncatchable process death, unlike `FlakyReader`'s clean `Err` or `PanicReader`'s catchable
/// panic above.
#[cfg(unix)]
#[test]
fn crash_child_import_then_abort() {
    let Ok(spec) = std::env::var("PITCREW_IMPORT_CRASH") else {
        return;
    };
    let (path, mode) = spec
        .split_once(':')
        .expect("PITCREW_IMPORT_CRASH=<path>:<mode>");
    let mut options = StoreOptions::default();
    options.fs = if mode == "network" {
        FsMode::Network
    } else {
        FsMode::Local
    };
    let store = Store::open(path, options).expect("open");
    let events = generated_events(2_000);
    let text = lines(&events).into_bytes();
    // Comfortably past the first batch, so there is real, uncommitted on-disk content (WAL frames
    // or rollback-journal pages) by the time this aborts.
    let abort_after = lines(&events[..1_500]).len();
    let reader = std::io::BufReader::new(AbortReader {
        data: text,
        pos: 0,
        abort_after,
    });
    let _ = store.import(reader);
    panic!("AbortReader must abort the process before import ever returns");
}

#[cfg(unix)]
#[test]
fn a_real_crash_mid_import_recovers_to_an_empty_store_in_both_modes() {
    use std::os::unix::process::ExitStatusExt;

    for mode in ["local", "network"] {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("store.db");
        let mut options = StoreOptions::default();
        options.fs = if mode == "network" {
            FsMode::Network
        } else {
            FsMode::Local
        };
        // Pre-create the (empty, migrated) store, and close it cleanly, so the child process only
        // exercises `import` itself, and so this process's own clean close (which releases the
        // network-mode lease) happens before the child tries to open the same path.
        drop(Store::open(&path, options.clone()).expect("precreate"));

        let exe = std::env::current_exe().expect("current_exe");
        let status = std::process::Command::new(&exe)
            .arg("--exact")
            .arg("crash_child_import_then_abort")
            .arg("--nocapture")
            .env("PITCREW_IMPORT_CRASH", format!("{}:{mode}", path.display()))
            .status()
            .expect("spawn the crashing child");
        assert!(
            status.signal().is_some(),
            "mode {mode}: expected the child to die by signal (process::abort), got {status:?}"
        );

        let reopened = Store::open(&path, options).expect("reopen after the crash");
        assert_eq!(
            reopened.latest_rev().expect("rev"),
            0,
            "mode {mode}: a real crash mid-import must leave the store empty on reopen"
        );
    }
}
