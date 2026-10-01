//! Snapshots, integrity checks, and export/import.

mod common;

use common::{both, toy_migrations};
use pitcrew_protocol::events::Event;
use pitcrew_protocol::ids::EventId;
use pitcrew_store::{Error, IntegrityReport, Store, StoreOptions};
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

/// The exact fuzz finding in `fuzz/regressions/store_import/r7-bad-line-after-a-batch`: 1,000
/// good lines (a full batch under the old design), then one bad line with no trailing newline.
/// The old `Store::import` committed the first 1,000 before reaching the bad line, leaving them
/// behind and refusing a retry with `NotEmpty` (R7, `docs/security/threat-model.md`).
#[test]
fn r7_fuzz_regression_a_bad_line_right_after_a_full_batch() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open(dir.path().join("store.db"), StoreOptions::default()).expect("open");
    let events = generated_events(1_000);
    let mut text = lines(&events);
    text.push_str("not an event"); // no trailing newline, exactly as the regression file has it
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
