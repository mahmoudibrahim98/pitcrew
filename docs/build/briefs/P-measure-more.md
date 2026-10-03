# Brief P · Measure the budgets still unmeasured

- **Stream:** P · Packaging and release. **Branch:** `s/P/measure-more`. **Paths:** `benches/**`.
- **First read:** [README.md](README.md), the root `AGENTS.md` (or `CLAUDE.md`),
  `docs/build/streams/P.md` (the budget table), and `benches/README.md` (how PR #12 measured the
  daemon at scale: its harness, the noise rules, "Not measured yet").
- **Suggested agent:** Codex, or any coding agent.
- **Overlaps:** `0-flaky-tests` fixes a test in `benches/src/client.rs`; leave that test alone.

## Goal

Real numbers for the budgets nobody has measured yet. Reuse PR #12's synthetic homes and harness.

## What to measure

Each one runs with one command and prints its numbers, in the way `benches/run.sh` and
`src/external.rs` do:

1. **Idle CPU with 50 live sessions** (budget ≤ 0.5% of one core):
   - 50 synthetic transcripts that keep growing slowly (a line every few seconds each), with the
     daemon watching them;
   - measure the daemon's CPU over a few minutes once the first scan is done;
   - also report idle CPU with nothing changing.
2. **A `pitcrew` verb's round trip** (budget ≤ 50 ms): the CLI's `whoami`, `task list` and
   `task show` against a daemon with the 10,000-session history; process start to exit, median and
   p95.
3. **`pitcrew hook` wall time** (budget ≤ 10 ms):
   - the CLI's own `hook_timing` test exists (`crates/cli`); report its numbers on this machine;
   - add the same measurement with the daemon under the 10,000-session load and with the daemon
     down.
4. **Hook event → UI change over the stream** with 50 live sessions, the same way the hook → frame
   time is measured today, to see whether live load changes it.
5. **Optional, only if the environment can build and run the desktop app under Xvfb:** its cold start
   to interactive (budget ≤ 1.5 s) and idle RAM with 3 workspaces (budget ≤ 300 MB). If it can't,
   say why, and leave it for a local run.

## Record

- Put the numbers in `benches/README.md` with the machine they came from.
- Mark each one within or over budget, and say what dominates any that are over.
- Add them to the baseline only if the noise rules allow it.

## Acceptance

- Each measurement cleans up after itself (no daemon left running, temporary homes removed) and
  stays within the environment's disk.
- fmt, clippy, the guards, and every CI job pass.

## Out of scope

Optimisations (report findings only), and changes outside `benches/`.
