# Brief 0 · Idle CPU: the last 0.11 points, without delaying first events

- **Stream:** 0 · Composition root (runner, daemon, benchmarks).
  **Branch:** `integrator/idle-cpu-2`.
  **Paths:** `crates/runner/**`, `crates/daemon/**`, `benches/**`, and the READMEs of the crates
  touched.
- **First read:**
  - [README.md](README.md) and the root `AGENTS.md` (or `CLAUDE.md`);
  - [0-idle-cpu.md](0-idle-cpu.md) and PR #40 (merged), with its comments: the profile, what was
    changed, and why the deadline grid was capped at 10 ms;
  - `benches/README.md` ("After the 10 ms notification cap").
- **Suggested agent:** Codex, on Linux (the CPU harness reads `/proc`).

## Goal

Idle CPU with 50 live sessions and a growing 10k history is **0.61%** of one core; the budget is
**0.5%**. #40 got it to 0.48%, but its 175 ms deadline grid delayed a burst's first event past the
300 ms latency budget on Windows and macOS, and the 10 ms cap gave part of the saving back.

## What to do

1. **Profile again on `main`,** the same way #40 did (`perf record` on an unstripped build with the
   same optimisation during the growing 10k workload). Report the disjoint table first.
2. **Coalesce without delaying the first event.** Deliver the first change of a burst at the
   debounce. Batch only what arrives while a read or delivery is already due or running (for
   example a "busy" window that ends when the in-flight work completes, not a fixed grid). Or
   find the saving elsewhere in the profile.
3. **Both budgets, measured:**
   - idle CPU under 0.5% for every workload in `benches/more.sh cpu` and `--cpu-history`
     (180-second windows, as before);
   - `crates/runner/tests/fake_source.rs` (`new_items_arrive_fast...`) unchanged and passing on all
     three OSes in CI;
   - say what each change saved.

## Acceptance

- fmt, clippy with `-D warnings`, `cargo test -p pitcrew-runner -p pitcrew-daemon`, `npm test` and
  the guards pass. Every CI job passes on the pull request.
- `benches/README.md` records the new measurements next to the old ones.
