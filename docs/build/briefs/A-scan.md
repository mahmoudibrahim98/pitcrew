# Brief A · Machine scan

- **Stream:** A · Ingest. **Branch:** `s/A/scan`. **Paths:** `crates/ingest/**`.
- **First read:** [README.md](README.md), then `docs/build/streams/A.md` (work package 5), the
  merged adapters in `crates/ingest` (Claude, Codex, OpenCode), and the onboarding brief's scan
  step (`O-onboarding-components.md`), which is the consumer.

## Goal

A **read-only scan** of a machine's agent history for onboarding and for "scan again": what
exists, grouped usefully, plus **suggested projects and workstreams**. It must be fast on 10,000
sessions, with streamed progress.

## What to build

1. **`scan(homes, options, progress: impl FnMut(ScanProgress)) -> ScanReport`**, using each
   adapter's `discover` plus light metadata reads: the first records and the session facts,
   **never the full transcript**.
   - Report counts per engine, per account home, per folder (`cwd`), and per month.
   - Report first and last activity, and sub-agent sessions separately.
2. **Suggestions:**
   - **projects** from repository roots: walk up from each `cwd` to the nearest `.git` (a
     directory or file); non-git folders group by common research roots;
   - **workstreams** from active sub-folders and non-default branches inside a project root;
   - rank both by recent activity (sessions in the last 30 and 90 days);
   - never propose the home directory itself or system paths.
3. **Types for the contract:** `ScanReport`, `ScanProgress` and `Suggestion` with serde, ready
   to move into `pitcrew-protocol`. Propose the exact move in your report.
4. **Performance:** use a rayon pool (add `rayon` with an exact version if it isn't in the
   workspace; say so) or a bounded thread pool. Progress events at most every 100 ms.
5. **Privacy:** the scan reads metadata only, never copies content, and the report holds paths
   and counts, not prompt text (a title is fine).

## Acceptance

- Synthetic homes with Claude, Codex and OpenCode data in temp dirs: exact counts, suggestions
  and rankings (snapshot tests).
- 10,000 synthetic small sessions scan in under 60 s in release (report the number), and
  progress is monotonic.
- An unreadable folder is skipped with a warning count; symlink loops don't hang.

## Out of scope

Importing sessions (the runner indexes them), the API route (proposed only).
