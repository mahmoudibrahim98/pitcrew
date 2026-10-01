//! Timing: 100,000 generated events must build blocks in under 500 ms in a release build.
//!
//! Run it with `cargo test --release -p pitcrew-recap --test perf -- --nocapture`. In debug builds
//! it is ignored, because the number means nothing there.
//!
//! Two workloads: *sparse*, where 200 sessions share the log and events are up to two minutes
//! apart, so almost every event starts its own block (the worst case: most allocation, most
//! closing); and *bursty*, where 24 sessions work in bursts a few seconds apart.

mod common;

use common::{SplitMix, T0, World, gen_events};
use pitcrew_recap::{BlockBuilder, Config, RuleSummarizer, blocks, day_recaps};
use std::time::{Duration, Instant};

const EVENTS: usize = 100_000;
const BUDGET: Duration = Duration::from_millis(500);

fn best_of<T>(runs: usize, mut f: impl FnMut() -> T) -> (Duration, T) {
    let mut best = Duration::MAX;
    let mut out = None;
    for _ in 0..runs {
        let start = Instant::now();
        let v = f();
        best = best.min(start.elapsed());
        out = Some(v);
    }
    (best, out.expect("at least one run"))
}

fn run(name: &str, world: &World, max_gap_ms: u64) {
    let specs = SplitMix(0x5EED).specs(EVENTS, max_gap_ms);
    let events = gen_events(&specs, world, T0, 1);
    let cfg = Config::default();

    let (pure, all) = best_of(5, || blocks(&events, &world.dir, &cfg));
    let (batched, count) = best_of(5, || {
        let mut b = BlockBuilder::new(cfg.clone(), world.dir.clone());
        let mut n = 0;
        for chunk in events.chunks(500) {
            n += b.push_batch(chunk).closed.len();
        }
        n + b.open_blocks().len()
    });
    let (recap, days) = best_of(3, || {
        day_recaps(&all, &world.dir, 0, &RuleSummarizer).map(|d| d.len())
    });

    eprintln!(
        "{name}: {EVENTS} events -> {} blocks; blocks() {:.1} ms; builder in batches of 500 \
         {:.1} ms; {} day recaps {:.1} ms",
        all.len(),
        pure.as_secs_f64() * 1e3,
        batched.as_secs_f64() * 1e3,
        days.unwrap_or(0),
        recap.as_secs_f64() * 1e3,
    );
    assert_eq!(count, all.len());
    assert!(pure < BUDGET, "{name}: blocks() took {pure:?}");
    assert!(batched < BUDGET, "{name}: the builder took {batched:?}");
}

#[test]
#[cfg_attr(
    debug_assertions,
    ignore = "timing is only meaningful in release builds"
)]
fn hundred_thousand_events_build_in_budget() {
    run("sparse", &World::new(200, 400, 20), 120_000);
    run("bursty", &World::new(24, 60, 6), 4_000);
}
