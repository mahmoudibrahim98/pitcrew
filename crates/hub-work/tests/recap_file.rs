//! The recap index with its blocks in a file (`WorkService::with_recap_file`), as the daemon runs
//! it: kept current as the log grows, in any batches and with queries in between, it serves
//! exactly what an index in memory rebuilt from the whole log serves, and what the recap engine
//! makes of the whole log at once.

mod recap_common;

use pitcrew_hub_work::{BlockFilter, DaysScope, RecapIndex, WorkService};
use pitcrew_protocol::events::Event;
use proptest::collection::vec;
use proptest::prelude::*;
use recap_common::{
    Ids, Oracle, Spec, T0, World, allowed_receipts, check_receipts, dump, gen_events, log_events,
    open_store, oracle_dump,
};
use std::sync::Arc;

fn spec() -> impl Strategy<Value = Spec> {
    let dt = prop_oneof![
        6 => 0i64..300_000,
        3 => 300_000i64..2_400_000,
        1 => 3_600_000i64..86_400_000,
        1 => -600_000i64..0,
    ];
    (any::<u8>(), any::<u8>(), any::<u8>(), any::<bool>(), dt)
}

fn batches<'a>(events: &'a [Event], sizes: &[usize]) -> Vec<&'a [Event]> {
    let mut out = Vec::new();
    let mut rest = events;
    let mut size = sizes.iter().cycle();
    while !rest.is_empty() {
        let n = size.next().copied().unwrap_or(1).clamp(1, rest.len());
        let (batch, tail) = rest.split_at(n);
        out.push(batch);
        rest = tail;
    }
    out
}

/// Something a client might ask between two appends.
fn ask(index: &dyn RecapIndex, world: &World, what: u8, limit: usize) {
    match what % 4 {
        0 => {
            index
                .recap_blocks(&BlockFilter::default(), None, Some(limit))
                .expect("blocks");
        }
        1 => {
            index
                .recap_days(DaysScope::Project(world.projects[0].id), 0, None, Some(3))
                .expect("days");
        }
        2 => {
            index
                .recap_days(DaysScope::Workstream(world.workstreams[1].id), 120, None, None)
                .expect("days");
        }
        _ => {
            let filter = BlockFilter {
                session: Some(world.sessions[0].id),
                ..BlockFilter::default()
            };
            index
                .recap_blocks(&filter, None, Some(2))
                .expect("blocks");
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 24, ..ProptestConfig::default() })]

    #[test]
    fn a_file_index_equals_a_rebuild_in_memory(
        specs in vec(spec(), 0..300),
        sizes in vec(1usize..40, 1..20),
        queries in vec(any::<u8>(), 1..20),
        limit in 1usize..60,
    ) {
        let world = World::new(2, 4, 8, 6);
        let mut ids = Ids::default();
        let setup = world.setup(&mut ids, T0 - 86_400_000);
        let events = gen_events(&specs, &world, &mut ids, T0);
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("recaps.sqlite3");
        let work = WorkService::new(open_store(dir.path()), world.workspace.clone())
            .with_recap_file(&file);
        work.store().append(&setup).expect("append");
        let mut question = queries.iter().cycle();
        for batch in batches(&events, &sizes) {
            work.store().append(batch).expect("append");
            ask(&work, &world, question.next().copied().unwrap_or(0), limit);
        }
        prop_assert!(file.exists());
        let incremental = dump(&work, &world, limit);
        let rebuilt = WorkService::new(Arc::clone(work.store()), world.workspace.clone());
        prop_assert_eq!(&dump(&rebuilt, &world, limit), &incremental);
        let log = log_events(&work);
        prop_assert_eq!(&oracle_dump(&Oracle::new(&log), &world), &incremental);
        check_receipts(&incremental, &allowed_receipts(&log));
        drop(work);
        prop_assert!(!file.exists());
    }
}

/// A file a crashed daemon left behind, from an index built over more of the log, or another
/// log, is replaced when the index is built: a new service answers from the log alone.
#[test]
fn a_file_left_by_a_crash_is_never_served() {
    let world = World::new(2, 4, 8, 6);
    let mut ids = Ids::default();
    let setup = world.setup(&mut ids, T0 - 86_400_000);
    let specs = recap_common::SplitMix(7).specs(400, 1_800_000);
    let events = gen_events(&specs, &world, &mut ids, T0);
    let (first, second) = events.split_at(events.len() / 2);

    // Another log's index, in full, at the path the next service uses.
    let other = tempfile::tempdir().expect("tempdir");
    let left = other.path().join("left.sqlite3");
    let full = WorkService::new(open_store(other.path()), world.workspace.clone())
        .with_recap_file(&left);
    full.store().append(&setup).expect("append");
    full.store().append(&events).expect("append");
    full.sync_recaps().expect("sync");

    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("recaps.sqlite3");
    std::fs::copy(&left, &file).expect("copy");
    drop(full);

    let store = open_store(dir.path());
    store.append(&setup).expect("append");
    store.append(first).expect("append");
    let work = WorkService::new(Arc::clone(&store), world.workspace.clone()).with_recap_file(&file);
    let memory = WorkService::new(Arc::clone(&store), world.workspace.clone());
    assert_eq!(dump(&work, &world, 50), dump(&memory, &world, 50));
    store.append(second).expect("append");
    assert_eq!(dump(&work, &world, 50), dump(&memory, &world, 50));
    let log = log_events(&work);
    assert_eq!(oracle_dump(&Oracle::new(&log), &world), dump(&work, &world, 50));
}
