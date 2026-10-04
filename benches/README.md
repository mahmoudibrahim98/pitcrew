# pitcrew-benches

Benchmarks for the performance budgets in [docs/build/streams/P.md](../docs/build/streams/P.md),
with a baseline and a regression check. **Owned by stream P.**

## Run

```bash
benches/run.sh                    # full: 20 and 200 MiB transcripts, then compare (a few minutes)
benches/run.sh --quick            # CI: 20 MiB only, fewer samples
benches/run.sh --no-tests         # leave out the other crates' timing tests (about a minute less)
benches/run.sh --write-baseline   # record this machine's numbers in benches/baseline.json
benches/run.sh --extend-baseline  # compare, and add the passing metrics the baseline lacks
benches/run.sh --scale            # also the scale measurements: pitcrewd with 10,000 transcripts (below)
benches/run.sh --scale-only       # only those (about 5 minutes, 2 GiB of disk, Linux)
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

From `pitcrew-bench-scale`, the real `pitcrewd` over 10,000 synthetic transcripts (next section):

| Metric | What | Unit | Budget |
|---|---|---|---|
| `scale.first_scan` | Setup request until the runner has read every transcript to its end, page cache dropped | ms | First scan, 10k transcripts on SSD ≤ 60 s, streamed (budget only) |
| `scale.first_scan.first_session` | Until the first session is in the log (the scan streams) | ms | none (not compared: 100 ms polls) |
| `scale.rss.{scan_peak,scan_steady}` | `VmHWM` over the first scan and the idle after it; `VmRSS` after 20 s idle | MiB | `pitcrewd` memory, 10k sessions indexed ≤ 80 MB RSS (read as 80 MiB) |
| `scale.rss.{restart_peak,restart_steady}` | The same for a daemon started with the index present | MiB | the same |
| `scale.cold_start`, `.no_tmux` | Process spawn to the ready line, index present: median of 5 (tmux detection as shipped; tmux refused, median of 3) | ms | Cold start to serving ≤ 300 ms (`.no_tmux`: none) |
| `scale.hook_to_frame` | `POST /v1/hooks/claude/…` until the `events` frame for its state change arrives on `/v1/stream`: p50 of 40 | ms | Hook event → UI change ≤ 300 ms local |
| `scale.db.after_scan` | `hub.db` after a clean stop | MiB | none |
| `scale.db.per_session` | `hub.db` growth over an empty store, per session | KiB | none |
| `scale.db.per_1000_events` | `hub.db` growth for events from live transcripts, per 1,000 | KiB | none |
| `scale.index.after_scan` | The runner's own index (`runner/<log id>/`) | MiB | none |

The remaining CPU, CLI and loaded-hook measurements are opt-in; see
[Remaining budgets](#remaining-budgets). Desktop interactive startup and three-workspace RAM
still need a graphical run (stream K).

## Scale: `pitcrewd` with 10,000 transcripts

The budgets that depend on a person's whole history (memory with 10k sessions indexed, cold
start, the first scan, how the database grows) need the real daemon over a real-sized history.
`pitcrew-bench-scale` (`src/bin/scale.rs`, `src/scale.rs`) writes one into a temp folder, runs the
release `pitcrewd` on it, and prints each number as a line `external.rs` reads, so the report
checks them like the rest. Nothing real is read: the homes are generated, the daemon is started
with `--homes` on them, and its `HOME` and every variable that points elsewhere is the run's own
(`pitcrew_fixtures::homes` refuses to start it otherwise).

```bash
benches/run.sh --scale-only        # all of it, then the report
cargo build --profile bench -p pitcrew-daemon      # pitcrewd, built beside the tool (run.sh does this)
cargo run --profile bench -p pitcrew-benches --bin pitcrew-bench-scale -- scan     # one command per measurement:
#   scan    generate, first scan, memory during and after it, database size
#   start   scan, then cold start and memory with the index present
#   hook    scan, then hook to stream frame
#   growth  scan, then the database's growth per 1,000 events
#   all     everything (the default)
cargo run --profile bench -p pitcrew-benches --bin pitcrew-bench-scale -- gen --out DIR   # only write the homes
```

Options: `--sessions N` (default 10,000), `--seed N`, `--pitcrewd PATH` (or `$PITCREWD`; default
the `pitcrewd` next to the tool), `--idle SECONDS` (20), `--starts N` (5), `--probes N` (40),
`--growth-turns N` (40), `--keep-cache` (do not drop the page cache), `--work DIR`, `--keep`
(keep the folder). It needs Linux (`/proc`), about 2 GiB in the temp dir (it refuses to start
unless that and 6 GiB to spare are free; `--min-free-gib`), and root to drop the page cache (without it
the scan reads what the generator just wrote, from memory; the output says which). It cleans up
after itself: every daemon is stopped with SIGTERM and waited for (and killed if the run fails
half way), the run's tmux servers are killed, the folder is removed, and it warns if anything of
the run is still running. Set `TMPDIR` to put the folder elsewhere.

### What a run does

1. **Generates the homes** (`src/homes.rs`), about 10 s, deterministic from `--seed`.
2. **First scan.** Starts a daemon on an empty state (to size an empty database), starts it again,
   sets the workspace up with `POST /v1/setup` (which starts the runner), and watches until the
   runner's index holds every transcript read to its end. Timed from the setup request. Memory
   is read every 5 s and again after the daemon has used almost no CPU for 2 s (at least 20 s).
   Then it lists the sessions through the API (`GET /v1/sessions` must return all 10,000), stops
   the daemon cleanly and sizes `hub.db` and the runner's index.
3. **Starts** with the index present, five times (spawn to the ready line), then three with tmux
   refused, once after dropping the page cache, and once more to stay up: its memory after it
   has caught up and gone quiet.
4. **Hooks.** Sends `UserPromptSubmit` and `Stop` alternately for one session (forty times after
   five that are not counted) with the stream open, and times each from just before the request
   until its `events` frame arrives.
5. **Growth.** Appends 40 turns to each of the 24 live sessions (6,240 events), waits until the
   log stops growing, stops the daemon and sizes `hub.db` again.

### The history

Generated as the fixtures are shaped (prompt, plan, tool calls with results, edits with a
structured patch, a closing message, a turn duration) with what a real history adds:

| | |
|---|---|
| Mix | 6,000 Claude sessions, 1,000 Claude sub-agents (next to a longer session), 2,000 Codex rollouts, 1,000 OpenCode sessions in one store: 10,000 transcripts, 1.50 GiB, 1.35 M records |
| Length | 45% of sessions are 1-2 turns, 30% 3-10, 20% 11-40, 5% 41-250. Claude files: median 46 KB, mean 174 KB, p99 2.6 MB, largest 3.6 MB; Codex: median 42 KB, mean 154 KB; the OpenCode store is 87 MB |
| Tool results | Log-normal: file reads median 4 KiB (200 B to 48 KiB), shell output median 700 B |
| Folders, months | 156 working directories (125 projects, some with git worktrees), a few holding most sessions; starts spread over 270 days, more of them recent; file times are the end of each session |
| Live | 24 Claude sessions written in the last 4 hours (the watcher keeps their folders watched), which the growth stage writes to |
| Events | The runner derives 611,924 events from it (61 a session): 399 k `tool_ran`, 133 k `turn_ended`, 70 k `file_edited`, 10 k `session_discovered` |

How much memory and database that takes depends on this shape, above all on events a session
(see "How it scales"). It is a heavy user's history, not a typical one.

### Numbers

This VM: 4 vCPU Intel Xeon under KVM, 16 GiB RAM, Ubuntu 24.04 (Linux 6.18), ext4 on a virtio
disk (the guest cannot tell if it is an SSD), Rust 1.97, tmux 3.4. The CPU reported 2.10 GHz for
P-measure's runs (PR #12) and 2.80 GHz for the memory brief's, on the same shape. Built in the
`bench` profile (the shipped `release`: thin LTO, stripped). Nothing else ran: the machine was
idle, after the builds had finished (load average at the start below).

Measured on 2026-10-03 for the memory brief (`docs/build/briefs/0-memory-budget.md`). Run A is
the recorded run (`benches/run.sh --scale-only --baseline benches/baseline-cloud-vm.json`, load
average 0.51 1.36 1.92 at the start, page cache dropped); the range is over three full runs of the
same code (A-C, load 0.51, 0.44 and 0.41). **Before** is `origin/main`'s daemon (the code P-measure
measured), run twice on the same VM the same day with the same tool (load 0.29 and 0.37).

| Metric | Budget | Run A | Range, 3 runs | Before, 2 runs | |
|---|---|---|---|---|---|
| `scale.first_scan` | 60 s | 28.4 s | 27.8-28.4 s | 26.6-28.5 s | within |
| `scale.first_scan.first_session` | streamed | 404 ms | 404-505 ms | 404 ms | 1,000 sessions after 3.2-3.4 s |
| `scale.rss.scan_peak` | 80 MiB | **43.9 MiB** | 43.9-44.3 | 83.4 | within (was over) |
| `scale.rss.scan_steady` | 80 MiB | 42.7 MiB | 42.7-43.3 | 82.1-82.3 | within (was over) |
| `scale.rss.restart_peak` | 80 MiB | **52.8 MiB** | 52.8-52.9 | 122.8-122.9 | within (was over) |
| `scale.rss.restart_steady` | 80 MiB | 52.8 MiB | 52.8-52.9 | 122.8-122.9 | within (was over) |
| `scale.cold_start` (tmux detection as shipped) | 300 ms | 92.1 ms (best 88.7) | median 90.5-92.1, best 87.3-88.7 | median 103.5-110.7, best 100.2-102.8 | within |
| `scale.cold_start.no_tmux` | | 61.2 ms | 58.1-61.2 ms | 69.3-70.3 ms | |
| first start after dropping the page cache (printed, not a metric) | 300 ms | 186 ms | 186-195 ms | 212-217 ms | within |
| `scale.hook_to_frame` | 300 ms | 78.5 ms (p95 79, max 80) | p50 78.4-78.5 ms, max 80-84 | p50 78.5-78.6 ms | within |
| `scale.db.after_scan` | | 349.6 MiB | 349.6 | 349.6 | 598 bytes an event |
| `scale.db.per_session` | | 35.75 KiB | 35.75-35.76 | 35.76 | 61 events a session |
| `scale.db.per_1000_events` | | 481 KiB | 476-481 | 476-478 | |
| `scale.index.after_scan` | | 11.75 MiB | 11.75 | 11.75 | 1.2 KiB a transcript |

P-measure's six runs on the 2.10 GHz VM gave, for the same code as "before": the first scan in
26.9-31.0 s, `scan_peak` 87.6-89.1 MiB, `restart_peak` 127.4-127.7 MiB, the cold start 99.9-114 ms
(median), the hook 78.8-79.5 ms. `GET /v1/sessions` with all 10,000 sessions answers in 40-45 ms (39-49 before)
(3.1 MiB). The daemon logged no warning or error that a fresh start does not always log. The
"first start after dropping the page cache" row is a start that reads the binary, the database and
the index from disk, as the first start after boot does.

**Memory, now within budget.** The 80 MB in P.md is read as 80 MiB (80 MB would be 76.3 MiB). Two
things held most of it, and both changed with the memory brief:

- **The recap index** kept every block in memory and is rebuilt from the whole log at every
  start (`built the recap index rev=611924 ms=2668` before, `ms=2926` now). Its blocks now go to a
  SQLite file of the index's own, `recaps.sqlite3` in the state directory on a local filesystem
  (on a network or unknown filesystem, a private local folder in temp before `$XDG_RUNTIME_DIR`,
  or memory if neither works): a cache replaced at
  every start and removed at a clean stop, never read from one run to the next (hub-work's README,
  "Recaps"). Here that is 10,000 blocks (one a session) in 33 MiB of disk. What stays in memory is
  the recap engine's directory and the blocks it still holds open.
- **The runner** loaded every row of its index (cursor, session facts and metadata) and kept it:
  about 4 KiB a transcript. Its watcher now keeps only what tells a change and what routes hooks,
  and reads the rest of a row when the transcript changes or a hook reports (the runner's README,
  "Memory"): **0.87 KiB a transcript**.

Seen from the outside on one kept state (the 10,000 transcripts above, `pitcrewd serve` run by
hand on a copy; RSS 25 s after the recap index is built; the live heap from `heaptrack` on a build
with line tables, at the same point):

| Daemon, restart with the index present | RSS before | RSS now | Live heap before | Live heap now |
|---|---|---|---|---|
| default | 121.6 MiB | 53.3 MiB | 96.7 MiB | 32.6 MiB |
| `--no-runner` | 71.0 MiB | 40.2 MiB | | |

The heap at its peak (`heaptrack`: the recap index being built while the runner starts) went from
103.4 MiB to 36.3 MiB:

| Part, at the peak | Before | Now |
|---|---|---|
| The runner (the watcher's maps and rows, the rows decoded at start) | 42.5 MiB | 8.5 MiB |
| The recap index (blocks, directory, open blocks, lines) | 49.4 MiB | 17.1 MiB, and 33 MiB on disk |
| SQLite (page caches: `hub.db`, the runner's index, the recap file) | 6.2 MiB | 9.3 MiB |

**What is left is live data, not fragmentation.** glibc's `malloc_stats()` (through `gdb`, on the
same restart, `bench` build) puts 42.4 MiB in its 18 arenas, 34.4 MiB of it in use: 8.0 MiB is
free memory the allocator holds (15% of the 53.3 MiB RSS; before, 4.9 MiB of 111.2). Another
12.3 MiB of RSS is mapped files (the binary and libraries). So a different allocator could save at
most about 8 MiB here; the allocator was left alone.

**Not over budget, but what dominates.**

- *First scan, 28 s of 60.* 352-360 transcripts a second, 21-22 k events a second; 38.6-38.9 s of
  CPU over 27.8-28.4 s wall on 4 vCPUs. Two runner threads do nearly all of it: the sink thread
  (appending 612 k events to `hub.db` with the work model's projections, and saving each cursor
  in the runner's index) 18.3-18.6 s, and the watcher (reading, parsing and deriving the events)
  17.6-17.7 s, joined by a 64-batch channel. The watcher uses about 10% more CPU than before
  (15.6-16.1 s on the same VM), which the wall time absorbs: the scan is within the spread of
  the runs before. It is CPU-bound, and streams: the first session is in the log after 0.4-0.5 s
  and 1,000 after 3.2-3.4 s.
- *Cold start, 92 ms of 300.* Tmux detection is on the path to the ready line (`block_on`): it
  costs about 30 ms (61 ms with tmux refused). Opening the store and starting the runner with 10k
  rows is the rest (see "How it scales"). The recap index's 2.9 s build does not delay it.
- *Hook to frame, 78 ms of 300.* 75 ms of that is the stream's batch window (the default
  config); the request, the runner and the append are under 4 ms, with 10,000 sessions present
  or not (the `stream.append_to_frame` benchmarks give 76.4 ms with none).
- *Database.* 598 bytes an event (the event's JSON, four indexes, and the work model's
  projections); live events cost 476-481 KiB a thousand, a little less than the average because
  they only append. A heavy history of 612 k events is 350 MiB. The runner's index adds
  11.75 MiB, and the recap file 33 MiB while the daemon runs.
- *Recap queries.* With the blocks in the file, the newest 50 blocks take 2.2 ms (median of 21,
  1.95 ms before) and every block, in pages of 200, 373 ms (275 ms before); a session's block
  0.7 ms, as before. The pages are byte for byte the same as before (all 50 pages of 200 hashed).

### How it scales

`start --sessions N` with the same mix (page cache warm); the 10,000 row is from the three `all`
runs above (page cache dropped for the scan). Before is `origin/main`'s daemon, the same day:

| Transcripts | Events | First scan | RSS, first scan (peak / steady) | RSS, restart | Cold start | `hub.db` |
|---|---|---|---|---|---|---|
| 2,500 | 148 k | 6.6 s (6.6 before) | 28.8 / 28.0 MiB (38.0 / 37.5) | 33.5 MiB (48.2) | 53 ms (56) | 85.5 MiB |
| 5,000 | 309 k | 13.6 s (13.6) | 34.5 / 33.1 MiB (53.3 / 52.0) | 44.4 MiB (77.9) | 69 ms (72) | 177.4 MiB |
| 10,000 | 612 k | 27.8-28.4 s (26.6-28.5) | 44.1 / 42.8 MiB (83.4 / 82.2) | 52.8 MiB (122.9) | 91 ms (104-111) | 349.6 MiB |

Everything is linear in the history. The restart's RSS now grows by 2.6 MiB a thousand
transcripts (10.0 before) and the first scan's by 2.0 (6.1 before), so at this density the restart
would cross 80 MiB at about 20,000 transcripts and the first scan at about 28,000; the first scan
would take 60 s at about 21,000.

### Limits of these numbers

- One synthetic shape on one VM class. Memory follows transcripts (the runner, and the recap
  engine's directory and the blocks it holds open) more than events, which now cost disk (the
  recap file) rather than memory; the table above gives the slope. Every session here is one
  block; a history whose sessions make many blocks has a larger recap file, not more memory.
- The cold-start and restart numbers have a warm page cache, except the one printed after
  dropping it. The first scan is run with the cache dropped when the run may (root), which
  needs a fresh disk read of 1.5 GiB; the guest's disk may not be an SSD.
- The hook is timed over loopback TCP from this process; the desktop reaches the same routes
  over the private socket. The stream's WebSocket write is included (the client reads it).
- The first scan is one sample per run, and the memory numbers are read from `/proc` once, so the
  scale metrics are not retried. Between six runs the first scan moved by 15% (cache warm and
  dropped together) and 9% with the cache the same, the first session by 25%; memory by 1-4%.
- Windows and macOS are not measured (the tool reads `/proc`); it compiles there and refuses to
  run.

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

### The cloud VM baseline

[`baseline-cloud-vm.json`](baseline-cloud-vm.json) is the baseline of the scale metrics for the
cloud VM class above (a 4 vCPU Xeon, 16 GiB; the machine string differs from the laptop's, so
the laptop's file is left alone and the numbers are not mixed). It was written with
`benches/run.sh --scale-only --baseline benches/baseline-cloud-vm.json --extend-baseline` on the
idle VM, which adds only the metrics within their budgets, and then trimmed by the noise rule:
what is in it agreed within 10% across the runs (cold start best 88.6-98.8 ms, the stream 78.0
ms, the database sizes to 0.1%). Left out: `scale.first_scan` and
`scale.first_scan.first_session` (one sample that moved by 9% and 25% between runs; checked
against the budget only, `compare = false` in `metrics.rs`). Compare with
`benches/run.sh --scale-only --baseline benches/baseline-cloud-vm.json`; runs E and F did, and
every metric in the file was `ok`, within 6% (the cold start best 5.4% better in E, 4.7% worse
in F).

The four `scale.rss.*` were over their budgets then, and are in it since the memory brief: from its
run A (see "Numbers"), after three runs of the same code agreed within 1.6% on each. Those runs
were on a VM of the same shape whose CPU reported 2.80 GHz, so the report warned that the machine
string differs from the file's; memory does not depend on the clock, and every other metric in
the file was `ok` against them (the cold start best 5.4% better, the stream 0.9% better, the
database sizes within 0.1%; the runner's index 8.6% smaller, 11.75 MiB, as the code before the
brief also measured on that VM).

## Remaining budgets

Run each independently, or run the combined budget report:

```bash
benches/more.sh cpu         # 50 transcripts; static and growing, 180 seconds each
benches/more.sh cpu --cpu-history # 10,000 transcripts, including the same 50 growing files
benches/more.sh verbs       # 10,000 transcripts; whoami, task list, task show
benches/more.sh hook-cli    # CLI hook: loaded daemon, then daemon stopped
benches/more.sh hook-live   # matching hook state frame amid 50 growing transcripts
benches/more.sh more        # all four; JSON budget report and a failing exit for a missed budget
```

The wrapper builds the daemon, CLI and harness with the bench profile and the existing lockfile.
Output goes to `target/pitcrew-more/<stage>.log`; the combined run also writes `summary.json`.
These stages are separate from historical `all`/`benches/run.sh` runs, so an ordinary benchmark
run does not require the new measurements. The default is 200 samples after five warmups for
CLI/stream timings. Tool flags follow the stage (`--probes`, `--cpu-seconds`, `--pitcrew`,
`--pitcrewd`, `--sessions` and the historical flags); shorter windows are for smoke checks,
not evidence for the three-minute CPU budget.

Each stage uses fresh synthetic homes and state, loopback TCP on an allocated port, and a fresh
seeded demo providing a real registered agent token for ordinary CLI verbs. Device tokens are
used only for device routes/hooks on the unowned transcript probe. No token is printed or put
in process arguments. The existing harness stops its daemon, joins the transcript writer and
stream reader, removes temporary homes even after an error, and checks for surviving processes.
It retains logs only; `--keep` explicitly retains the synthetic state for investigation.
The default free-space guard is 6 GiB; one 10k stage used about 1.5 GiB of transcript data and
completed within the cloud environment's disk.

### Method

- **CPU:** the whole daemon's user + system ticks from Linux `/proc`, divided by `getconf CLK_TCK`
  and actual elapsed wall time, as a percentage of **one** core. No division by the vCPU count.
  First indexing and a quiet settling period finish before either 180-second window. Fifty
  writers append one parseable assistant JSONL record every five seconds each, staggered by
  100 ms. The writer runs in the harness, outside the measured daemon. Afterward the runner's
  persisted cursors must reach all fifty new file ends, so ignored writes cannot pass. At
  100 Hz, a 180-second window has a 0.0056 percentage-point tick quantum.
- **Verbs:** real `pitcrew --json whoami`, `task list`, and `task show BENCH-1`, process spawn
  through successful exit, including handshake, API calls and JSON output. Each output must
  parse; task show must return the created task. Median and p95 are printed; p95 is compared
  conservatively against the 50 ms budget.
- **CLI hooks:** real stdin payload, alternating prompt/stop, process spawn to silent zero exit.
  A successful loaded sample must also deliver the expected session state via the API; silent
  failure cannot satisfy it. p99 is checked against 10 ms up and 5 ms down. Down is measured
  after the owned daemon has stopped, on the same now-unserved TCP port.
- **Live stream:** fifty transcripts grow while a separate stable probe alternates prompt/stop.
  The probe is reset idle first, so an earlier odd-length CLI run cannot turn the first prompt
  into a no-op. Each POST must return 202 and a typed event frame must match that session,
  target state and a revision newer than the pre-request log revision. Receipt time is captured
  before parsing, not after JSON processing. Probes are spaced by 400 ms. The metric is POST
  to matching **stream frame**, including the default 75 ms batch window; it does not measure
  browser rendering or claim a desktop interactive result.

### Cloud measurements, 2026-10-03

AMD EPYC 9V74 class, four available CPU threads, 33 GiB RAM; Linux x86_64 Debian 13 container,
Rust 1.99, original root release/bench profile. Page cache retained. No build ran alongside the
measurements. The 10k history held Claude 6,000 + 1,000 sub-agents, Codex 2,000 and OpenCode
1,000: 1.50 GiB, about 1.345 million source records. CPU-only generated fifty Claude transcripts.
Demo seed sessions are additional to the generated history. Live-frame runs reserve one probe
beside the fifty growing files. Values are observations on shared hardware, not universal costs.

| Measurement | Median | p95 | Budget statistic | Budget | Result |
|---|---:|---:|---:|---:|---|
| Static CPU, 50 transcripts | — | — | 0.01% of one core | ≤ 0.5% | within |
| Growing CPU, 50 transcripts | — | — | 0.49% of one core | ≤ 0.5% | within |
| Static CPU, 10k history | — | — | 0.34% of one core | ≤ 0.5% | within |
| Growing CPU, 50 live + 10k history | — | — | 1.58% of one core | ≤ 0.5% | **over** |
| CLI whoami, 10k | 1.665 ms | 2.074 ms | p95 2.074 ms | ≤ 50 ms | within |
| CLI task list, 10k | 1.915 ms | 2.314 ms | p95 2.314 ms | ≤ 50 ms | within |
| CLI task show, 10k | 1.843 ms | 2.849 ms | p95 2.849 ms | ≤ 50 ms | within |
| CLI hook, loaded | 1.284 ms | 1.845 ms | p99 3.417 ms | ≤ 10 ms | within |
| CLI hook, daemon down | 0.997 ms | 1.312 ms | p99 2.217 ms | ≤ 5 ms | within |
| Hook → frame, 50 growing + 10k | 77.827 ms | 78.422 ms | p50 77.827 ms | ≤ 300 ms | within |

The CLI's existing test was also run separately, on this machine without a concurrent build:

```bash
cargo test --locked -p pitcrew-cli --profile bench --test hook_timing -- --ignored --nocapture
```

| Existing hook timing | p50 | p99 | Budget | Result |
|---|---:|---:|---:|---|
| Unconfigured process floor | 0.89 ms | 1.17 ms | informational | — |
| Unix socket up | 1.05 ms | 1.31 ms | ≤ 10 ms p99 | within |
| Stale Unix socket down | 0.98 ms | 1.31 ms | ≤ 5 ms p99 | within |
| Loopback TCP up | 1.16 ms | 1.52 ms | ≤ 10 ms p99 | within |
| Loopback TCP down | 1.01 ms | 1.35 ms | ≤ 5 ms p99 | within |

**Over-budget CPU:** the growing 10k run added 1.24 percentage points over its static control,
about 78% of its total CPU. Handling live source changes (watcher wakeups, incremental reads,
runner/index work and any resulting hub work) dominates that increment; this measures the whole
pipeline and does not attribute it to an individual function. The fifty-only control separates
that activity from the extra history's periodic work: growing costs 0.49% without the 10k
history, leaving only about two 100 Hz ticks of margin in a three-minute window. This control
suggests the large history is material, but an exact hot-function attribution needs profiling. No optimisation or runtime behavior was
changed in this brief. The stream timing is close to its deliberate 75 ms batching window.

**Noise and baseline:** load averages at measurement starts ranged from 0.11 to 0.85. Loaded
hook p99 was 1.726 ms in the combined run and 3.417 ms in the independent run, already beyond
10% agreement. A hook timing run during compilation also varied substantially. There is no
stable repeated same-machine baseline evidence: neither baseline file was extended. New metrics
have `compare = false` and are budget-only; missing values or a budget miss fail the opt-in
`--only-more` report. An over-budget observation remains a failure, not an ignored assertion.

**Desktop not measured:** native Linux desktop builds succeeded during installer work, but this
container has no graphical display or Xvfb, and its unrelated-UID private-path ancestors also
prevent a usable terminal runtime. Cold start to interactive (1.5 s) and idle RAM with three
workspaces (300 MB) remain for a local graphical run. Daemon RSS/startup and stream delivery are
not substituted for those desktop measurements.


### Idle CPU follow-up, 2026-10-03 (`0-idle-cpu`)

Started from `main` at `6b18a6a`. The following before/after runs use the same synthetic harness,
Rust 1.99, AMD EPYC 9V74 cloud environment class (four available threads, 33 GiB RAM), and the
shipped release/bench profile with `opt-level = "z"`. PR #27's older absolute CPU numbers above
used the earlier profile; the fresh controls below are the comparison for this change. The
cloud environment resumed between the earlier profiling/controls and final runs. Page cache was
retained; no builds, tests or profiler ran beside these CPU windows.

```bash
benches/more.sh cpu --keep-cache
benches/more.sh cpu --cpu-history --keep-cache
```

`--cpu-history` keeps the default 10,000 generated transcripts **including** the 50 live files,
rather than forcing the CPU-only stage down to 50 Claude files. The history mix is unchanged:
Claude 6,000 + 1,000 sub-agents, Codex 2,000, OpenCode 1,000; 1.50 GiB and 1,345,483 records.
Demo seed sessions are additional. Every static/growing window is 180 seconds; growing follows
10 seconds of writer warmup. Each full growing run wrote 1,897 lines and verified all 50
persisted cursors at their new file ends.

| Whole daemon CPU, one core | Before | After | Before ticks | After ticks |
|---|---:|---:|---:|---:|
| Static, 50 files | 0.01% | 0.00% | 1 / 180.06 s | 0 / 180.06 s |
| Growing, 50 files | 0.55% | 0.33% | 99 / 180.06 s | 59 / 180.06 s |
| Static, 10k history | 0.38% | 0.08% | 69 / 180.07 s | 15 / 180.04 s |
| Growing, 10k history | 1.17% | **0.46%** | 210 / 180.05 s | 82 / 180.06 s |

#### Profile and the work removed

Before changing runtime code, an unstripped build with the same optimisation was sampled during
the growing 10k stage with `perf record -e cpu-clock:u -F 999 --call-graph dwarf,16384 -p PID
-- sleep 90`. It captured 730 user-space CPU samples, with none lost. The profiler was outside
the official before/after windows. These are sample shares, not wall-time measurements; kernel
work is included in the `/proc` CPU numbers but excluded from this user-space profile.

The text flame-graph summary groups each stack once (nested inclusive costs are not added):

```text
watcher / handle / full-history polling filter       105  14.4%
watcher / rediscover / adapter walks, sorts, maps     137  18.8%
watcher / check / load / SQLite + cursor JSON        113  15.5%
watcher / check / remaining stat, read, derivation    127  17.4%
sink / accept + cursor commits / SQLite              152  20.8%
notification/library paths                           26   3.6%
other, including inlined scheduling/library work      70   9.6%
```

The hot paths were `Watcher::handle` scanning all tracked rows for polling even when no home
was polled, `Watcher::rediscover → SourceAdapter::discover` walking/sorting the file history,
`Watcher::check → load → Store::load` decoding saved rows for every live change, and the sink's
SQLite commits. The polling-filter closure alone had 13.15% self samples. Hub-work appeared in
4/730 inclusive samples (0.55%); raw notification paths in 29/730 (4.0%). They did not justify
changing the hub or dropping watches. The slow sweep remains on its original schedule.

The runner now bypasses polling-list scans when there are no polled homes, retains at most 64
saved hot rows separately from unsaved rows, shares immutable cursor snapshots, caches update
statements, and avoids redundant EOF reads for the concrete byte-cursor adapters. Periodic
file discovery checks bounded directory snapshots; Unix quiet OpenCode databases also check
identity, size, mtime and ctime, with SQLite side files forcing full discovery. Explicit rescans,
new-file events, overflow, watcher errors, failed watches and network polling retain full work.
All transcripts still get their scheduled size/mtime/identity checks, including deletion checks.

Before `36e1aa3`, the daemon's 100 ms notification debounce was rounded to a 175 ms grid, adding less than 175 ms;
source reads are therefore due within 275 ms. Polling and hook reports are not rounded. Adjacent
completed reads of different sessions can share one sink acceptance and one atomic cursor
transaction, bounded to four batches and the existing event limit. Partial/backfill batches
keep their boundaries and backpressure. Cursor advancement still follows durable acceptance;
failed acceptance retries identical events and failed cursor transactions pin all affected rows.

#### Other guarantees and limits

The runner's existing five-write latency/restart test passed with latencies
174.11, 174.97, 174.92, 175.03 and 174.82 ms (each strictly below 300 ms), and no reads/events were
repeated on restart. Real-Claude tests cover byte EOF, partial/appended lines, truncation,
replacement, symlink refusal, deletion/reappearance and stable session IDs. Cache, polling,
forced discovery, grouping/retry, accepted-item replay and atomic rollback tests also pass.
Two existing row-unload tests now explicitly make their rows cold, preserving their assertions;
new tests separately verify bounded saved-hot retention, unsaved pinning and cooling.

The other budgets were checked after the CPU windows, without concurrent builds or tests:

```bash
target/release/pitcrew-bench-scale scan --keep-cache
benches/more.sh hook-live --keep-cache
```

| Measurement | After | Budget / condition |
|---|---:|---|
| 10k first scan | 13.86 s | ≤ 60 s; page cache retained, cold SSD timing not verified |
| First scan peak / steady RSS | 39.45 / 38.40 MiB | ≤ 80 MiB |
| Hook → matching frame, 50 growing + 10k | p50 78.94 ms; p95 79.63; p99 80.76; max 83.20 | p50 ≤ 300 ms; 200 probes |

The scan streamed its first session after 205 ms and produced 611,924 events for all 10,000
sessions. The loaded hook run wrote another 1,099 lines and verified all 50 persisted cursors.
The container's unrelated-UID ancestors prevent a terminal runtime; the measured daemon still
served the real API, hooks and stream. No graphical or terminal result is inferred from it.

No baseline was extended: these are budget observations on shared cloud hardware, without the
stable repeated same-machine evidence required for a 10% regression baseline. Temporary raw
profile artifacts from the earlier environment did not survive resumption; their text summary
is retained above. Desktop CPU and graphical budgets were outside this brief.


#### After merging dispatch and its follow-ups

Merged `origin/main` at `5453375` into `integrator/idle-cpu` with normal merge commit
`c9b56f3`, preserving named-session dispatch/adoption, folder claims and the candidate-scoped
exited-terminal final scan. The daemon keeps main's `FollowingSink` and the idle optimizations.
The ten named-session tests also run with the daemon's optimization settings; all pass.

The two full CPU commands above were repeated on this merged tree, on the same cloud environment
class. No builds, tests or profiler ran beside their 180-second static/growing windows. Each
writer again produced 1,897 lines and all fifty persisted cursors reached the new file ends.

| Whole daemon CPU, one core | Original control `6b18a6a` | Before merge | Merged tree | Merged ticks / wall |
|---|---:|---:|---:|---:|
| Static, 50 files | 0.01% | 0.00% | 0.01% | 1 / 180.07 s |
| Growing, 50 files | 0.55% | 0.33% | 0.36% | 65 / 180.06 s |
| Static, 10k history | 0.38% | 0.08% | 0.10% | 18 / 180.06 s |
| Growing, 10k history | 1.17% | 0.46% | **0.48%** | 87 / 180.04 s |

The merged growing 10k result remains below 0.5%; its margin is about three 100 Hz ticks in this
window. The small movements from the pre-merge results are observations on shared hardware,
without repeated same-machine evidence establishing a regression. No baseline was extended.

The merged 10k scan with retained page cache completed in **15.09 s**, streamed the first session
after **102.53 ms**, and kept peak/steady RSS at **39.96 / 39.12 MiB**. It produced the same
611,924 events. The 60 s scan and 80 MiB memory budgets remain met.
The merged loaded hook-to-frame run measured **79.17 ms p50**, 79.72 ms p95, 80.84 ms p99 and
81.47 ms maximum across 200 probes. It wrote 1,099 source lines and again verified every live
cursor at EOF; the 300 ms budget remains met.


#### After the 10 ms notification cap, 2026-10-04 (`36e1aa3`)

Fast-forwarded to `36e1aa3` and rebuilt the benchmark binaries. Repeated both full commands:

```bash
benches/more.sh cpu --keep-cache
benches/more.sh cpu --cpu-history --keep-cache
```

Notification rounding is now capped at 10 ms, even though the daemon requests 175 ms. The
following values supersede the uncapped merged CPU results above for the current implementation.

| Workload | Original control `6b18a6a` | Uncapped merged `e0cf493` | 10 ms cap `36e1aa3` | Latest ticks / wall |
|---|---:|---:|---:|---:|
| Static, 50 files | 0.01% | 0.01% | 0.01% | 1 / 180.06 s |
| Growing, 50 files | 0.55% | 0.36% | 0.46% | 82 / 180.05 s |
| Static, 10k history | 0.38% | 0.10% | 0.11% | 20 / 180.07 s |
| Growing, 10k history | 1.17% | 0.48% | **0.61% (OVER)** | 109 / 180.06 s |

Growing 10k CPU is **0.61% of one core**, **0.11 percentage points above the 0.5% budget**. The budget is not met. The original 0.48% merged result used the
uncapped notification grid. As requested, measurement work stopped here without changing
scheduling or making another runtime optimization.

Each static/growing window lasted 180 seconds. Both commands exited 0 (the CPU stage records
measurements; it does not fail its process on a budget miss). Each writer produced 1,897 lines
and all fifty persisted cursors reached their new file ends. The 10k history retained the same
1.50 GiB / 1,345,483-record mix, including the 50 live files. AMD EPYC 9V74 cloud environment
class, four available threads, 33 GiB RAM, Rust 1.99 and the shipped `opt-level = "z"` profile.
Page cache was retained, with no concurrent builds, tests or profiler during the windows.
No baseline was extended. The earlier scan/RSS/hook and test results above predate `36e1aa3`
and were not repeated in this CPU-only follow-up.
