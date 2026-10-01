//! The delta stream's latency: from appending one event until the `events` frame carrying it
//! leaves the pump, with the default configuration (so the 75 ms batch window is included).
//! Over `MemorySource`, and over a real `Store` in a temp dir.

mod common;

use criterion::{Criterion, SamplingMode, criterion_group, criterion_main};
use pitcrew_api::stream::StreamConfig;
use pitcrew_api::{MemorySource, StoreSource};
use pitcrew_benches::inputs;
use pitcrew_benches::probe::StreamProbe;
use pitcrew_store::{Store, StoreOptions};
use std::sync::Arc;
use std::time::Duration;

fn stream(c: &mut Criterion) {
    let mut group = c.benchmark_group("stream");
    group
        .sampling_mode(SamplingMode::Flat)
        .sample_size(10)
        .warm_up_time(Duration::from_millis(200));

    let memory = Arc::new(MemorySource::new("bench", 1024));
    let mut probe = StreamProbe::connect(memory.clone(), StreamConfig::default()).expect("connect");
    group.bench_function("append_to_frame_memory", |b| {
        b.iter_custom(|iters| {
            (0..iters)
                .map(|_| {
                    probe
                        .round_trip(|| {
                            memory.append(vec![inputs::liveness_event()]);
                        })
                        .expect("a frame")
                })
                .sum()
        });
    });
    drop(probe);

    let dir = tempfile::tempdir().expect("temp dir");
    let store =
        Arc::new(Store::open(dir.path().join("stream.db"), StoreOptions::default()).expect("open"));
    let source = Arc::new(StoreSource::new(store.clone(), "bench"));
    let mut probe = StreamProbe::connect(source, StreamConfig::default()).expect("connect");
    group.bench_function("append_to_frame_store", |b| {
        b.iter_custom(|iters| {
            (0..iters)
                .map(|_| {
                    probe
                        .round_trip(|| {
                            store.append(&[inputs::liveness_event()]).expect("append");
                        })
                        .expect("a frame")
                })
                .sum()
        });
    });
    group.finish();
    drop(probe);
    drop(store);
    dir.close().expect("remove the store");
}

criterion_group! {
    name = benches;
    config = common::criterion();
    targets = stream
}
criterion_main!(benches);
