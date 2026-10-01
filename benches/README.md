# pitcrew-benches

Benchmarks for the performance budgets in [docs/build/streams/P.md](../docs/build/streams/P.md),
with a baseline and a regression check. **Owned by stream P.**

## Run

```bash
benches/run.sh                    # full: 20 and 200 MiB transcripts, then compare (a few minutes)
benches/run.sh --quick            # CI: 20 MiB only, fewer samples
benches/run.sh --no-tests         # leave out the other crates' timing tests (about a minute less)
benches/run.sh --write-baseline   # record this machine's numbers in benches/baseline.json
benches/run.sh --extend-baseline  # compare, and add the metrics the baseline lacks
cargo bench -p pitcrew-benches    # criterion alone (full mode; PITCREW_BENCH_MODE=quick for quick)
```

`run.sh` runs the criterion benchmarks and three timing tests that other streams own (below),
then `pitcrew-bench-report`, which reads both, prints a table and writes a JSON summary (default
`<target>/pitcrew-bench/summary.json`). It exits non-zero when a metric is **more than 10% worse**
than the baseline, is **over its budget**, or **did not report**. Before failing, it runs what
failed again, up to twice (`--retries N`), keeping each metric's better numbers: noise fails one
attempt, a real regression fails them all. A metric that did not report is retried too: under
load a benchmark can time out or a test can be killed, which is as likely noise as a slow
sample, and a benchmark that is really broken fails every retry. Generated inputs go in the
system temp dir and are deleted as each benchmark finishes; at most one 200 MiB file exists at a
time.

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

From the other streams' timing tests, run in release (the bench profile) with `--ignored
--nocapture` and read from what they print ([`src/external.rs`](src/external.rs)):

| Metric | Test | Unit | Value / best | Budget |
|---|---|---|---|---|
| `runner.idle_cpu.{notify,polling}` | `crates/runner/tests/idle_cpu.rs`: 50 watched transcripts, 20 s idle, Linux only | %cpu | the percent | ≤ 0.5% of one core (P.md); budget only, see below |
| `hub.tasks.route.{status,assignee_status}` | `crates/hub-work/tests/perf.rs`: `GET /v1/tasks` with a filter over 10,000 tasks | ms | median / best of 30 | median < 20 ms |
| `cli.hook.{up,down}.{tcp,unix}` | `crates/cli/tests/hook_timing.rs`: `pitcrew hook` spawn to exit, 200 runs, daemon up or not listening (`unix` on Unix only) | ms | p99 / p50 | p99 ≤ 10 ms up, ≤ 5 ms down |

Those tests assert their own budgets; a test that misses one still prints its numbers, and the
report marks them `over_budget`. If a test's output format changes, its metrics become `missing`
and the run fails, so the parser is updated rather than the numbers silently dropped. Idle CPU is
counted in 0.05% ticks (one tick in the 20 s window), far coarser than a 10% threshold, so it is
checked against its budget only, never the baseline.

Each metric has two numbers. `value` is the typical cost and is checked against the **budget**:
criterion's **median** per iteration, or what the test's budget names (the median, the p99).
`best` is the steadiest number and is checked against the **baseline**: criterion's fastest
sample per iteration, the test's best run or its p50. Other work on the machine (parallel builds,
an fsync stall) only ever adds time, so `best` moves less between runs than the median. A code
change that slows the work down still slows the fastest sample.

Transcripts are synthetic, shaped like the fixtures, with one 24 KiB tool result per turn; they
are in the page cache when measured, so `read_page` is the parse cost, not a cold disk read.

Not measured yet: `pitcrewd` memory with 10k sessions, cold start, first scan of 10k transcripts,
the `pitcrew` verb round trip, and the desktop numbers (stream K).

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
fraction (negative is better). `status` is `ok`, `improved`, `new` (no baseline), `skipped`
(200 MiB inputs in quick mode, a platform that cannot measure it, or `--no-tests`), or one of the
failures: `regressed`, `over_budget`, `missing`.

## The baseline

[`baseline.json`](baseline.json) holds one machine's numbers, and it names the machine class.
Numbers only compare within a class: a CI runner needs its own baseline, recorded on that runner
with `--write-baseline`. The report warns when the machine differs from the baseline's.

The committed baseline comes from the stream's laptop (Intel Core Ultra 5 135U: 2 performance,
8 efficiency and 2 low-power cores; WSL2), recorded while other agents were building. **On that
machine a 10% gate is not reliable**: WSL cannot pin work to a core type, and the same binary
measured this far apart within a few hours (best per iteration):

| Metric | Run A | Slowest run | Run B (the baseline) | Quick runs after B |
|---|---|---|---|---|
| `transcript.claude.read_page.20mib` | 0.92 ms | 3.72 ms | 1.83 ms | 1.44, 1.92, 1.16 ms |
| `transcript.claude.read_from.20mib` | 416 MiB/s | 73 MiB/s | 235 MiB/s | 199, 176, 369 MiB/s |
| `store.before.page_100.one_type` | 0.29 ms | 1.05 ms | 0.51 ms | 0.44, 0.37, 0.35 ms |
| `control.parse` | 370 MiB/s | 139 MiB/s | 255 MiB/s | 263, 241, 360 MiB/s |
| `stream.append_to_frame.memory` | 76.3 ms | 76.2 ms | 76.4 ms | 76.3, 76.3, 76.2 ms |

Only the stream latency, which a timer dominates, holds still. Without retries, one of four quick
runs against this baseline passed. With the default two retries, three of five did (load
average 2-4), each after re-running one to four benchmarks; the other two ran while other builds
held the load average near 10, and the slowdown outlasted both retries. So the laptop gives a local check when it is quiet,
but a real regression under about 2x could hide in this noise: use the laptop baseline to see
orders of magnitude and the budgets' headroom, and gate on a quiet, dedicated runner with its own
baseline.
