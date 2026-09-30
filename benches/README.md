# pitcrew-benches

Benchmarks for the performance budgets in [docs/build/streams/P.md](../docs/build/streams/P.md),
with a baseline and a regression check. **Owned by stream P.**

## Run

```bash
benches/run.sh                   # full: 20 and 200 MiB transcripts, then compare (a few minutes)
benches/run.sh --quick           # CI: 20 MiB only, fewer samples (about a minute)
benches/run.sh --write-baseline  # record this machine's numbers in benches/baseline.json
cargo bench -p pitcrew-benches   # criterion alone (full mode; PITCREW_BENCH_MODE=quick for quick)
```

`run.sh` runs the criterion benchmarks, then `pitcrew-bench-report`, which reads criterion's
results, prints a table and writes a JSON summary (default
`<target>/pitcrew-bench/summary.json`). It exits non-zero when a metric is **more than 10% worse**
than the baseline, is **over its budget**, or **did not run**. Generated inputs go in the system
temp dir and are deleted as each benchmark finishes; at most one 200 MiB file exists at a time.

## What is measured

| Metric | Benchmark | Unit | Budget |
|---|---|---|---|
| `transcript.{claude,codex}.read_page.{20,200}mib` | Newest page (200 items, the API default) of a generated transcript | ms | Transcript open to first paint ≤ 300 ms at any size |
| `transcript.{claude,codex}.read_from.{20,200}mib` | Full parse from offset 0 | MiB/s | none (first-scan speed) |
| `store.append.batch_100` | `Store::append` of 100 events into a 10k-event log | ms | 10 ms (10k events in batches of 100 < 1 s, store README) |
| `store.since.page_100` | `Store::since(5000, 100)` | ms | 5 ms per page (store README) |
| `store.before.page_100.{one,two}_type(s)` | `Store::before` with a type filter | ms | 5 ms per page |
| `control.parse` | `ControlParser::feed` over 8 MiB of `tmux -C` output in 16 KiB chunks | MiB/s | none |
| `stream.append_to_frame.{memory,store}` | Append one event → the `events` frame leaves the delta-stream pump (default config, so the 75 ms batch window is included) | ms | Hook event → UI change ≤ 300 ms local |

Each metric has two numbers. `value` is criterion's **median** per iteration, the typical cost,
and is checked against the **budget**. `best` is the fastest sample's time per iteration, and is
checked against the **baseline**: other work on the machine (parallel builds, landing on an
efficiency core, an fsync stall) only ever adds time, so `best` moves far less between runs than
the median, and a 10% threshold does not flake on a shared machine. A code change that slows the
work down still slows the fastest sample.

Transcripts are synthetic, shaped like the fixtures, with one 24 KiB tool result per turn; they
are in the page cache when measured, so `read_page` is the parse cost, not a cold disk read.

Not measurable yet, because the code does not exist: `pitcrewd` idle CPU and memory, cold start,
first scan of 10k transcripts, `pitcrew` round trip and hook wall time (streams 0, D and I), and
the desktop numbers (stream K).

## Summary format

```json
{
  "schema": 1, "mode": "quick", "machine": "…", "baseline_machine": "…",
  "threshold": 0.1, "passed": true,
  "metrics": [
    { "name": "store.since.page_100", "value": 0.36, "best": 0.31, "unit": "ms",
      "budget": 5.0, "budget_name": "Each event-log page under 5 ms (store README)",
      "baseline": 0.30, "change": 0.033, "status": "ok" }
  ]
}
```

`baseline` is the baseline's `best`, and `change` is how much worse `best` is than it, as a
fraction (negative is better). `status` is
`ok`, `improved`, `new` (no baseline), `skipped` (200 MiB inputs in quick mode), or one of the
failures: `regressed`, `over_budget`, `missing`.

## The baseline

[`baseline.json`](baseline.json) holds one machine's numbers, and it names the machine class.
Numbers only compare within a class: a CI runner needs its own baseline, recorded on that runner
with `--write-baseline`. The report warns when the machine differs from the baseline's.

<!-- baseline-notes -->
