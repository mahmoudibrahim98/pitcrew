# Brief 0 · Bring the daemon under its memory budget

- **Stream:** 0 · Composition root (integration work: hub-work, runner, daemon).
  **Branch:** `integrator/memory-budget`.
  **Paths:**
  - `crates/hub-work/src/recap.rs` and what it needs in `crates/hub-work/**`;
  - `crates/runner/src/**`;
  - `crates/daemon/src/{serve,recaps}.rs`;
  - `crates/store/**`, only if a new projection table is needed (a migration; say so in the report);
  - `benches/README.md` (new numbers);
  - the READMEs of the crates touched, and `Cargo.lock`.
- **First read:**
  - [README.md](README.md) and the root `CLAUDE.md`;
  - `benches/README.md` "At scale" (the measurements, and what dominates them);
  - the READMEs of `crates/hub-work` (the recap index), `crates/runner` (its index and rows) and
    `crates/daemon` (start-up order);
  - `docs/build/streams/P.md` (the budget table).

## Goal

P-measure (PR #12) measured `pitcrewd` with 10,000 transcripts (612 k events) on the 4 vCPU cloud VM.
The budget is ≤ 80 MiB RSS with 10k sessions indexed:

| Measurement | RSS |
|---|---|
| First scan, peak | 88 MiB |
| Restart with the index present | **127.5 MiB** |

Two things dominate:

- **The recap index, about 51 MiB.**
  - It is rebuilt at every start from the whole log (`built the recap index … ms=2441`).
  - It costs about 88 bytes an event, so it grows with events, not sessions.
- **The runner, about 60 MiB.**
  - At start it loads all 10,000 rows of its index (cursor, session facts and metadata, as JSON)
    and tracks every one: about 6 KiB a transcript.

Bring both restart and first scan under 80 MiB at 10k sessions, with headroom, without slowing the
first scan past its budget (≤ 60 s; 27-31 s today) or the cold start (≤ 300 ms; 100 ms today).

## What to build

1. **The recap index needn't live whole in memory, or be rebuilt at every start.** Measure first,
   then pick, and say why:
   - (a) persist it (a projection in `hub.db`, kept up to date as events append, rebuilt only when
     its revision is behind);
   - (b) keep only what recaps query in memory, in a compact form (interned strings, ids instead of
     copies, no per-event `String`s);
   - (c) both.

   Recaps must answer exactly as today: the recap tests and the daemon's `tests/recaps.rs` pass
   unchanged. If it's persisted, a crash mid-update must leave it either rebuilt or correct, never
   stale; test it.
2. **The runner needn't hold every row.**
   - Keep in memory only what the watcher needs per transcript (path, cursor, size and mtime, or
     whatever decides "changed"), compactly.
   - Load session facts and metadata on demand from its database.
   - Keep the per-transcript in-memory cost under 1 KiB, and report it.
3. **Measure again** with P-measure's harness at 2,500, 5,000 and 10,000 transcripts.
   - Update `benches/README.md` "At scale", and add the `scale.rss.*` lines to the baseline if they
     are now within budget and the noise rules allow it.
   - Report heap as well as RSS (a heap profile is welcome: `dhat` or `heaptrack`, whichever runs in
     the VM).
4. **The allocator:** if what's left is fragmentation, not live data, say so with numbers. Don't
   switch allocators in this brief.

## Acceptance

- `scale.rss.restart_steady` and `scale.rss.scan_peak` are ≤ 80 MiB at 10,000 transcripts, on the
  same VM shape as #12 (4 vCPU, 16 GB).
- First scan, cold start and hook→frame stay within budget; report all three.
- fmt, clippy, `cargo test --workspace --no-fail-fast` (in the background, mind the disk), and the
  guards pass. Every CI job passes on the pull request.

## Out of scope

The desktop's memory, the UI, and switching the allocator.
