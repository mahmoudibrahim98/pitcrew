//! Properties of the recap index over generated logs: kept current as the log grows, in any
//! batches and with queries in between, it serves exactly what a rebuild from the whole log
//! serves, and what the recap engine makes of the whole log at once; every receipt points into
//! the log.

mod recap_common;

use pitcrew_hub_work::{
    BlockFilter, DAY_CACHE_ENTRIES, DaysScope, RecapIndex, Recaps, WorkService,
};
use pitcrew_protocol::events::Event;
use pitcrew_recap::{Config, Directory};
use proptest::collection::vec;
use proptest::prelude::*;
use recap_common::{
    Core, Ids, Oracle, Spec, T0, World, allowed_receipts, check_receipts, dump, gen_events,
    log_events, open_store, oracle_dump,
};
use std::sync::{Arc, Mutex};

fn spec() -> impl Strategy<Value = Spec> {
    let dt = prop_oneof![
        6 => 0i64..300_000,
        3 => 300_000i64..2_400_000,
        1 => 3_600_000i64..86_400_000,
        1 => -600_000i64..0,
    ];
    (any::<u8>(), any::<u8>(), any::<u8>(), any::<bool>(), dt)
}

fn config() -> impl Strategy<Value = Config> {
    (1i64..40, any::<bool>()).prop_map(|(minutes, small)| {
        let gap_ms = minutes * 60_000;
        if small {
            Config {
                gap_ms,
                max_open: 3,
                max_files: 2,
                max_facts: 3,
                max_tasks: 2,
                max_actors: 2,
                max_receipts: 2,
            }
        } else {
            Config {
                gap_ms,
                ..Config::default()
            }
        }
    })
}

/// Something a client might ask between two appends.
fn ask(index: &dyn RecapIndex, world: &World, what: u8, limit: usize) {
    let blocks = |filter: BlockFilter, limit: usize| {
        index
            .recap_blocks(&filter, None, Some(limit))
            .expect("blocks");
    };
    let days = |scope: DaysScope, tz: i32, limit: Option<usize>| {
        index.recap_days(scope, tz, None, limit).expect("days");
    };
    match what % 6 {
        0 => blocks(BlockFilter::default(), limit),
        1 => days(DaysScope::Project(world.projects[0].id), 0, Some(3)),
        2 => days(DaysScope::Workstream(world.workstreams[1].id), 120, None),
        3 => blocks(
            BlockFilter {
                session: Some(world.sessions[0].id),
                ..BlockFilter::default()
            },
            2,
        ),
        // Every recent day the dump will ask for, so the cache holds what may go stale.
        4 => {
            for scope in recap_common::scopes(world) {
                for tz in recap_common::OFFSETS {
                    days(scope, tz, Some(limit.min(30)));
                }
            }
        }
        // Nothing: the next query reads several appends at once.
        _ => {}
    }
}

/// Specs without the events that can change a name (asks, members, tasks and workstreams
/// re-stated): those rewrite every cached paragraph, so without them only what changed is.
fn quiet(specs: &[Spec], quiet: bool) -> Vec<Spec> {
    specs
        .iter()
        .map(|&(kind, a, b, flag, dt)| match kind % 24 {
            9 | 16 | 17 | 19 if quiet => (4, a, b, flag, dt),
            _ => (kind, a, b, flag, dt),
        })
        .collect()
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

proptest! {
    #![proptest_config(ProptestConfig { cases: 24, ..ProptestConfig::default() })]

    /// Through the hub: appends in random batches, queries in between (the index catches up
    /// part-way, and caches days that later change), then every page equals a fresh index's
    /// rebuild from the log and the engine's blocks of the whole log.
    #[test]
    fn incremental_updates_equal_a_rebuild(
        specs in vec(spec(), 0..300),
        sizes in vec(1usize..40, 1..20),
        queries in vec(any::<u8>(), 1..20),
        limit in 1usize..60,
        names_change in any::<bool>(),
    ) {
        let world = World::new(2, 4, 8, 6);
        let mut ids = Ids::default();
        let setup = world.setup(&mut ids, T0 - 86_400_000);
        let events = gen_events(&quiet(&specs, !names_change), &world, &mut ids, T0);
        let dir = tempfile::tempdir().expect("tempdir");
        let work = WorkService::new(open_store(dir.path()), world.workspace.clone());
        work.store().append(&setup).expect("append");
        let mut question = queries.iter().cycle();
        for batch in batches(&events, &sizes) {
            work.store().append(batch).expect("append");
            ask(&work, &world, question.next().copied().unwrap_or(0), limit);
        }
        let incremental = dump(&work, &world, limit);
        let rebuilt = WorkService::new(Arc::clone(work.store()), world.workspace.clone());
        prop_assert_eq!(&dump(&rebuilt, &world, limit), &incremental);
        let log = log_events(&work);
        prop_assert_eq!(&oracle_dump(&Oracle::new(&log), &world), &incremental);
        check_receipts(&incremental, &allowed_receipts(&log));
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 128, ..ProptestConfig::default() })]

    /// In memory, with other engine settings (small caps evict open blocks) and caches as small
    /// as nothing: any batching, with queries in between, gives what one push of everything
    /// gives, and what the engine makes of the whole log.
    #[test]
    fn any_batching_gives_the_same_recaps(
        specs in vec(spec(), 0..250),
        sizes in vec(1usize..25, 1..40),
        queries in vec(any::<u8>(), 1..40),
        cfg in config(),
        cache in prop_oneof![1 => Just(0usize), 2 => 1usize..6, 3 => Just(DAY_CACHE_ENTRIES)],
        limit in 1usize..40,
        names_change in any::<bool>(),
    ) {
        let world = World::new(2, 3, 6, 5);
        let mut ids = Ids::default();
        let mut log = world.setup(&mut ids, T0 - 86_400_000);
        log.extend(gen_events(&quiet(&specs, !names_change), &world, &mut ids, T0));

        let mut whole = Recaps::with_config(cfg.clone(), Directory::new());
        whole.push(&log);
        let whole = Core(Mutex::new(whole));

        let batched = Core(Mutex::new(
            Recaps::with_config(cfg.clone(), Directory::new()).with_day_cache(cache),
        ));
        let mut question = queries.iter().cycle();
        for batch in batches(&log, &sizes) {
            batched.0.lock().expect("lock").push(batch);
            ask(&batched, &world, question.next().copied().unwrap_or(0), limit);
        }
        let got = dump(&batched, &world, limit);
        prop_assert!(batched.0.lock().expect("lock").cached_days() <= cache);
        prop_assert_eq!(&dump(&whole, &world, limit), &got);
        let oracle = Oracle::with_config(&log, Directory::new(), &cfg);
        prop_assert_eq!(&oracle_dump(&oracle, &world), &got);
        check_receipts(&got, &allowed_receipts(&log));
    }
}
