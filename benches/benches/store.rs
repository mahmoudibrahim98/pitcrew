//! The event log: appending a batch of 100 events, and reading one page of 100 forwards
//! (`since`) and backwards by type (`before`), in a store seeded with 10,000 events shaped like
//! the demo workspace's log. The store lives in a temp dir.

mod common;

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use pitcrew_benches::inputs;
use pitcrew_store::{EventFilter, Store, StoreOptions, event_type};
use std::hint::black_box;

const SEED: usize = 10_000;
const BATCH: usize = 100;

fn store(c: &mut Criterion) {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = Store::open(dir.path().join("bench.db"), StoreOptions::default()).expect("open");
    let seed = inputs::events(SEED).expect("events");
    for batch in seed.chunks(BATCH) {
        store.append(batch).expect("seed the log");
    }
    let end = store.latest_rev().expect("latest rev");

    // The most common type, and a second one.
    let first = event_type(&seed[0].body).expect("event type");
    let second = seed
        .iter()
        .filter_map(|e| event_type(&e.body).ok())
        .find(|t| *t != first)
        .expect("a second event type");
    let one = EventFilter::default().types([first.clone()]);
    let two = EventFilter::default().types([first, second]);

    let mut group = c.benchmark_group("store");
    group.bench_function("since_100", |b| {
        b.iter(|| black_box(store.since(end / 2, BATCH).expect("since")));
    });
    group.bench_function("before_100_one_type", |b| {
        b.iter(|| black_box(store.before(end + 1, BATCH, &one).expect("before")));
    });
    group.bench_function("before_100_two_types", |b| {
        b.iter(|| black_box(store.before(end + 1, BATCH, &two).expect("before")));
    });
    // Last, because it grows the log.
    let template = &seed[..BATCH];
    group.bench_function("append_100", |b| {
        b.iter_batched(
            || inputs::with_fresh_ids(template),
            |batch| black_box(store.append(&batch).expect("append")),
            BatchSize::SmallInput,
        );
    });
    group.finish();
    drop(store);
    dir.close().expect("remove the store");
}

criterion_group! {
    name = benches;
    config = common::criterion();
    targets = store
}
criterion_main!(benches);
