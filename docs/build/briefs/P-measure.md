# Brief P · Measure the budgets we haven't measured

- **Stream:** P · Packaging and release. **Branch:** `s/P/measure`. **Paths:** `benches/**` (+ test-only
  helpers it needs inside `benches/`).
- **First read:** [README.md](README.md), the root `CLAUDE.md`, `docs/build/streams/P.md` (the budget
  table), and `benches/README.md` (what is measured, the baseline, "Not measured yet").

## Goal

Real numbers for the budgets nobody has measured: how much memory and time the daemon needs with a
person's realistic history, and how its database grows.

## What to build

1. **Synthetic homes at scale:** a generator for 10,000 transcripts across Claude Code, Codex and
   OpenCode, shaped like the fixtures (sizes, tool results, sub-agents, a spread of folders and
   months). Deterministic from a seed, written to a temp dir. Never read a real home.
2. **Measurements**, each as a benchmark or an ignored timing test that prints its numbers (the
   `benches/run.sh` and `src/external.rs` way):
   - `pitcrewd` RSS with 10,000 sessions indexed: peak and steady (budget ≤ 80 MB);
   - cold start to serving with the index present (budget ≤ 300 ms);
   - the first scan of 10,000 transcripts (budget ≤ 60 s, streamed);
   - the database's size after the scan, and its growth per 1,000 new events and per session;
   - the hook event → stream frame time with 10,000 sessions present (budget ≤ 300 ms).
3. **Record** the numbers in `benches/README.md` with the machine they came from (this cloud VM:
   4 vCPU, 16 GB). Add them to the baseline only if the harness's noise rules allow it. Mark any metric
   over budget and say what dominates it, if you can tell (a profile is welcome but optional).

## Acceptance

- The measurements run with one command each, and `benches/README.md` lists them, with their values.
- They clean up after themselves (no daemon left running), and stay within the VM's disk.
- fmt, clippy, `cargo test --workspace` (in the background), and the guards pass.

## Out of scope

Optimisations (report findings, don't fix them here), and the desktop's start-up and memory (they
need a display; a later local run).
