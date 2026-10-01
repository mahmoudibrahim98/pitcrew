//! Timing: the recap index rebuilt from a log of 100,000 events, and its queries.
//!
//! `cargo test -p pitcrew-hub-work --release --test recap_perf -- --ignored --nocapture`
//!
//! Two workloads, as in the recap engine's own timing: *sparse*, where 200 sessions share the log
//! and events are up to two minutes apart, so most events start a block of their own (the most
//! blocks); and *bursty*, where 24 sessions work in bursts a few seconds apart.

mod recap_common;

use pitcrew_hub_work::{BlockFilter, DaysScope, RecapIndex, WorkService};
use pitcrew_recap::{Config, Directory};
use recap_common::{Ids, SplitMix, T0, World, gen_events, log_events, open_store};
use std::sync::Arc;
use std::time::{Duration, Instant};

const EVENTS: usize = 100_000;
const REBUILDS: usize = 5;
const QUERIES: usize = 21;

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1e3
}

/// Times `f` `runs` times: (best, median, worst).
fn timed(runs: usize, mut f: impl FnMut()) -> (Duration, Duration, Duration) {
    let mut times: Vec<Duration> = (0..runs)
        .map(|_| {
            let start = Instant::now();
            f();
            start.elapsed()
        })
        .collect();
    times.sort();
    (times[0], times[runs / 2], times[runs - 1])
}

fn run(name: &str, world: &World, max_gap_ms: u64) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = open_store(dir.path());
    let mut ids = Ids::default();
    let setup = world.setup(&mut ids, T0 - 86_400_000);
    store.append(&setup).expect("append");
    let specs = SplitMix(0x5EED).specs(EVENTS - setup.len(), max_gap_ms);
    let events = gen_events(&specs, world, &mut ids, T0);
    for chunk in events.chunks(1_000) {
        store.append(chunk).expect("append");
    }
    let total = store.latest_rev().expect("rev");
    assert_eq!(total, EVENTS as u64);

    // Reading the log alone, and the engine alone, to see where the time goes.
    let reading = timed(3, || {
        let mut rev = 0;
        let mut read = 0;
        loop {
            let page = store.since(rev, 1_000).expect("read");
            let Some(last) = page.last() else { break };
            rev = last.rev;
            read += page.len();
        }
        assert_eq!(read, EVENTS);
    });
    // What the index is fed: the log without the task creations the hub refused.
    let all = log_events(&WorkService::new(
        Arc::clone(&store),
        world.workspace.clone(),
    ));
    let mut engine = Vec::new();
    let engine_time = timed(3, || {
        engine = pitcrew_recap::blocks(&all, &Directory::new(), &Config::default());
    });

    // The rebuild: a fresh service's first sync reads the whole log into its index.
    let mut work = None;
    let rebuild = timed(REBUILDS, || {
        let fresh = WorkService::new(Arc::clone(&store), world.workspace.clone());
        assert_eq!(fresh.sync_recaps().expect("sync"), total);
        work = Some(fresh);
    });
    let work = work.expect("a run");

    let all_filter = BlockFilter::default();
    let project = DaysScope::Project(world.projects[0].id);
    let cold_days = timed(1, || {
        work.recap_days(project, 0, None, Some(30)).expect("days");
    });
    let warm_days = timed(QUERIES, || {
        work.recap_days(project, 0, None, Some(30)).expect("days");
    });
    let first_page = timed(QUERIES, || {
        work.recap_blocks(&all_filter, None, None).expect("blocks");
    });
    let session = BlockFilter {
        session: Some(world.sessions[0].id),
        ..BlockFilter::default()
    };
    let by_session = timed(QUERIES, || {
        work.recap_blocks(&session, None, Some(200))
            .expect("blocks");
    });
    // Paging through every block, 200 at a time, with their size as JSON.
    let mut blocks = 0;
    let mut json = 0;
    let every_page = timed(1, || {
        let mut before = None;
        loop {
            let page = work
                .recap_blocks(&all_filter, before, Some(200))
                .expect("blocks");
            blocks += page.blocks.len();
            json += serde_json::to_vec(&page.blocks).map_or(0, |v| v.len());
            before = page.blocks.last().map(|b| b.block.id);
            if page.at_start {
                break;
            }
        }
    });
    assert_eq!(blocks, engine.len());
    // A new event: the next query reads it and nothing else.
    let more = gen_events(&specs[..1], world, &mut ids, T0 + 400 * 86_400_000);
    store.append(&more).expect("append");
    let one_more = timed(1, || {
        work.recap_blocks(&all_filter, None, Some(1))
            .expect("blocks");
    });

    eprintln!(
        "{name}: {EVENTS} events -> {} blocks ({:.1} MB as JSON with their lines)\n  \
         rebuild (sync_recaps on a fresh service, {REBUILDS} runs): best {:.0} ms, median {:.0} \
         ms, worst {:.0} ms\n  \
         reading the log alone: best {:.0} ms; the engine alone: best {:.0} ms\n  \
         median of {QUERIES}: first page of 50 blocks {:.2} ms; a session's 200 blocks {:.2} ms; \
         30 days of a project from the cache {:.2} ms\n  \
         30 days of a project, cold {:.2} ms; every page of 200 {:.0} ms; one appended event, \
         then a page {:.2} ms",
        engine.len(),
        json as f64 / 1e6,
        ms(rebuild.0),
        ms(rebuild.1),
        ms(rebuild.2),
        ms(reading.0),
        ms(engine_time.0),
        ms(first_page.1),
        ms(by_session.1),
        ms(warm_days.1),
        ms(cold_days.0),
        ms(every_page.0),
        ms(one_more.0),
    );
}

#[test]
#[ignore = "timing; run in release with --ignored --nocapture"]
fn rebuilding_from_100k_events() {
    run("sparse", &World::new(3, 12, 400, 200), 120_000);
    run("bursty", &World::new(2, 6, 60, 24), 4_000);
}
