//! The tmux control-mode parser's throughput on 8 MiB of generated `tmux -C` output, fed in
//! 16 KiB chunks as a pipe would deliver it.

mod common;

use criterion::{Criterion, SamplingMode, Throughput, criterion_group, criterion_main};
use pitcrew_benches::inputs::{self, MIB};
use pitcrew_runtime::ControlParser;
use std::hint::black_box;

const CHUNK: usize = 16 * 1024;

fn control(c: &mut Criterion) {
    let data = inputs::control_stream(usize::try_from(8 * MIB).expect("fits"));
    let mut group = c.benchmark_group("control");
    group
        .throughput(Throughput::Bytes(data.len() as u64))
        .sampling_mode(SamplingMode::Flat);
    group.bench_function("parse_8mib", |b| {
        b.iter(|| {
            let mut parser = ControlParser::new();
            let mut notifications = 0usize;
            for chunk in data.chunks(CHUNK) {
                notifications += parser.feed(black_box(chunk)).len();
            }
            notifications
        });
    });
    group.finish();
}

criterion_group! {
    name = benches;
    config = common::criterion();
    targets = control
}
criterion_main!(benches);
