# Brief 0 · Bring idle CPU under budget with a large history

- **Stream:** 0 · Composition root (runner and daemon; measurement in `benches`).
  **Branch:** `integrator/idle-cpu`.
  **Paths:** `crates/runner/**`, `crates/daemon/**`, `crates/hub-work/**` only if the profile points
  there, `benches/**` (the measurement), and the READMEs.
- **First read:**
  - [README.md](README.md) and the root `CLAUDE.md`;
  - `benches/README.md` "Remaining budgets" (PR #27: method, numbers, "Over-budget CPU");
  - `crates/runner/README.md` (watchers, the slow schedule, polling on network filesystems);
  - `docs/build/streams/P.md` (budget: `pitcrewd` idle CPU with 50 live sessions ≤ 0.5% of one core).
- **Needs Linux** (`/proc`, `perf`): run it in the cloud VM.

## Goal

PR #27 measured, on the cloud VM, with 50 transcripts growing slowly (a line every 5 s each):

| History | CPU (one core) |
|---|---|
| None | 0.49% |
| Plus the 10,000-session history | **1.58%** (budget 0.5%) |

The growing run with the history costs 1.24 points more than its static control. Handling live
changes over the large history dominates, but nobody has profiled it. Find where the time goes, and
bring the growing 10k case under 0.5% without losing what the runner guarantees.

## What to do

1. **Profile first:**
   - run `benches/more.sh cpu` with the 10k history under `perf record` (or a sampling profiler that
     runs in the VM), and report the top costs with their call paths;
   - include a flame graph's text summary in the report.
   - Likely suspects to confirm or rule out:
     - a scan or relink touching all 10,000 rows per change;
     - the slow schedule re-statting cold transcripts too often;
     - notify watching too much;
     - hub-side work per event (projections, the recap index).
2. **Fix what dominates, keeping the guarantees:**
   - every new line still becomes an event, within the 300 ms budget the runner's own test checks;
   - a restart still resumes from cursors, without rescanning;
   - network filesystems still get polled;
   - deleted transcripts are still noticed.

   Prefer doing less over doing it faster: skip unchanged cold files, batch per wakeup, and don't
   touch all rows per event.
3. **Measure again** with the same harness: static and growing, with and without the 10k history.
   Update `benches/README.md`, and add the CPU lines to the baseline only if the noise rules allow.

## Acceptance

- The growing 10k case is ≤ 0.5% of one core on the cloud VM, and the 50-only and static cases
  don't get worse.
- The runner's latency test (new items arrive fast) and the restart tests pass. The first scan stays
  within 60 s, memory within 80 MiB, and hook → frame within 300 ms; report all three.
- fmt, clippy with `-D warnings`, the workspace tests (in the background), the guards, and every CI
  job pass.

## Out of scope

The desktop's CPU, and features.
