//! The run log as a store projection: what it stores is what the office did, and it rebuilds
//! identically however the log was appended.

mod common;

use common::{DAY, demo_config, demo_log, tick};
use pitcrew_fixtures::demo_workspace;
use pitcrew_office::{Config, Entry, Office, RUN_LOG, RunLog, read_runs};
use pitcrew_protocol::events::Event;
use pitcrew_protocol::ids::EventId;
use pitcrew_store::sql::{Connection, Transaction};
use pitcrew_store::{BoxError, Projection, Store, StoreOptions, StoredEvent};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

fn demo() -> (Config, Vec<Event>) {
    let ws = demo_workspace().expect("the demo workspace parses");
    let mut log = demo_log(&ws);
    let later = tick(&log, 4 * DAY, 1);
    log.push(later);
    // A week later still, so time-based rules run again after the first ones.
    let later = tick(&log, 7 * DAY, 2);
    log.push(later);
    (demo_config(&ws), log)
}

fn open(path: &Path, config: &Config) -> Store {
    let projections: Vec<Box<dyn Projection>> = vec![Box::new(RunLog::new(config.clone()))];
    Store::open_with(path, StoreOptions::default(), projections).expect("store opens")
}

type Rows = Vec<(String, String)>;

/// Both tables, read back.
fn tables(store: &Store) -> (Vec<Entry>, Rows) {
    store
        .read(|conn: &Connection| -> Result<_, BoxError> {
            let runs = read_runs(conn, 0, usize::MAX)?;
            let mut stmt = conn.prepare("SELECT key, value FROM office_state ORDER BY key")?;
            let state = stmt
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<Result<Rows, _>>()?;
            Ok((runs, state))
        })
        .expect("tables read")
}

fn expected(config: &Config, log: &[Event]) -> Vec<Entry> {
    let mut office = Office::new(config.clone());
    log.iter()
        .zip(1u64..)
        .flat_map(|(e, rev)| office.on_event(rev, e))
        .collect()
}

#[test]
fn the_run_log_is_what_the_office_did_and_rebuilds_identically() {
    let (config, log) = demo();
    let want = expected(&config, &log);
    assert!(want.len() > 5, "the demo makes the office act: {want:#?}");

    let mut seen = None;
    for batch in [1usize, 3, 7, 1000] {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("store.db");
        let store = open(&path, &config);
        for chunk in log.chunks(batch) {
            store.append(chunk).expect("append");
        }
        let incremental = tables(&store);
        assert_eq!(incremental.0, want, "batches of {batch}");

        store.rebuild(RUN_LOG).expect("rebuild");
        assert_eq!(tables(&store), incremental, "rebuilt, batches of {batch}");
        match &seen {
            None => seen = Some(incremental),
            Some(first) => assert_eq!(&incremental, first, "batches of {batch}"),
        }
    }
}

#[test]
fn a_late_projection_and_a_reopened_store_agree() {
    let (config, log) = demo();
    let want = expected(&config, &log);
    let dir = tempfile::tempdir().expect("tempdir");

    // The log first, the run log registered later: the open builds it from the log.
    let late = dir.path().join("late.db");
    {
        let store = Store::open(&late, StoreOptions::default()).expect("store opens");
        store.append(&log).expect("append");
    }
    let store = open(&late, &config);
    assert_eq!(tables(&store).0, want);
    let late_tables = tables(&store);
    drop(store);

    // Half the log, then a new process (an empty cache) appends the rest: the office's state is
    // read back from the store and carries on exactly.
    let reopened = dir.path().join("reopened.db");
    let half = log.len() / 2;
    {
        let store = open(&reopened, &config);
        store.append(&log[..half]).expect("append");
    }
    let store = open(&reopened, &config);
    store.append(&log[half..]).expect("append");
    assert_eq!(tables(&store), late_tables);
}

/// A projection registered after the run log that fails once on one event, so the append rolls
/// back after the office has already seen part of the batch.
struct FailOnce {
    on: EventId,
    armed: Arc<AtomicBool>,
}

impl Projection for FailOnce {
    fn name(&self) -> &str {
        "test.fail_once"
    }

    fn version(&self) -> u32 {
        1
    }

    fn reset(&self, _tx: &Transaction<'_>) -> Result<(), BoxError> {
        Ok(())
    }

    fn apply(&self, _tx: &Transaction<'_>, event: &StoredEvent) -> Result<(), BoxError> {
        if event.event.id == self.on && self.armed.swap(false, Ordering::SeqCst) {
            return Err("failing once".into());
        }
        Ok(())
    }
}

#[test]
fn a_rolled_back_append_leaves_the_office_as_it_was() {
    let (config, log) = demo();
    let want = expected(&config, &log);
    let dir = tempfile::tempdir().expect("tempdir");
    let half = log.len() / 2;
    let armed = Arc::new(AtomicBool::new(true));
    let projections: Vec<Box<dyn Projection>> = vec![
        Box::new(RunLog::new(config.clone())),
        Box::new(FailOnce {
            on: log[half + 3].id,
            armed: Arc::clone(&armed),
        }),
    ];
    let store = Store::open_with(
        dir.path().join("store.db"),
        StoreOptions::default(),
        projections,
    )
    .expect("store opens");
    store.append(&log[..half]).expect("append");
    assert!(store.append(&log[half..]).is_err());
    assert!(!armed.load(Ordering::SeqCst), "the failure happened");
    store.append(&log[half..]).expect("append");
    assert_eq!(tables(&store).0, want);
}
