//! Transcript reads on generated files: the newest page (what opening a session in the Agent
//! console costs) and a full parse from the start (what a first scan of a transcript costs), for
//! Claude Code and Codex, at 20 MiB, and at 200 MiB in full mode.
//!
//! Each file is generated in a temp dir and deleted before the next one, so at most one large
//! input exists at a time. The file is in the page cache while it is measured.

mod common;

use criterion::{Criterion, SamplingMode, Throughput, criterion_group, criterion_main};
use pitcrew_benches::Mode;
use pitcrew_benches::inputs::{self, MIB};
use pitcrew_ingest::claude::ClaudeAdapter;
use pitcrew_ingest::codex::CodexAdapter;
use pitcrew_interfaces::source::{Cursor, SourceAdapter};
use pitcrew_protocol::model::Engine;
use std::hint::black_box;
use std::time::Duration;

/// Items per page: the API's default (`docs/build/contracts/api-v1.md`).
const PAGE: usize = 200;

fn transcripts(c: &mut Criterion) {
    let mode = Mode::from_env();
    let claude = ClaudeAdapter::new();
    let codex = CodexAdapter::new();
    let engines: [(&str, Engine, &dyn SourceAdapter); 2] = [
        ("claude", Engine::Claude, &claude),
        ("codex", Engine::Codex, &codex),
    ];
    for &mib in mode.transcript_sizes() {
        for (name, engine, adapter) in engines {
            let dir = tempfile::tempdir().expect("temp dir");
            let path = dir.path().join(format!("{name}.jsonl"));
            let bytes = match engine {
                Engine::Claude => inputs::claude_transcript(&path, mib * MIB),
                _ => inputs::codex_transcript(&path, mib * MIB),
            }
            .expect("generate a transcript");
            let transcript = inputs::transcript_ref(engine, &path).expect("transcript ref");

            let mut group = c.benchmark_group("transcripts");
            group.bench_function(format!("{name}_read_page_{mib}mib"), |b| {
                b.iter(|| {
                    black_box(
                        adapter
                            .read_page(&transcript, None, PAGE)
                            .expect("read a page"),
                    )
                });
            });

            group
                .throughput(Throughput::Bytes(bytes))
                .sampling_mode(SamplingMode::Flat)
                .sample_size(10)
                .warm_up_time(Duration::from_millis(100));
            group.bench_function(format!("{name}_read_from_{mib}mib"), |b| {
                b.iter(|| {
                    black_box(
                        adapter
                            .read_from(&transcript, &Cursor::default())
                            .expect("read the transcript"),
                    )
                });
            });
            group.finish();
            dir.close().expect("remove the generated transcript");
        }
    }
}

criterion_group! {
    name = benches;
    config = common::criterion();
    targets = transcripts
}
criterion_main!(benches);
