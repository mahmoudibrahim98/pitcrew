# Brief D · Watch and index

- **Stream:** D · Runner service. **Branch:** `s/D/watch-and-index`. **Paths:**
  `crates/runner/**` only.
- **First read:** [README.md](README.md), then `docs/build/streams/D.md`, ADR-0009, ADR-0010,
  `crates/interfaces/src/source.rs` (`SourceAdapter`, `Cursor`) and its fakes,
  `crates/protocol/src/{runner,events,model}.rs`, and `crates/ingest` on `main` (the Claude
  adapter, `ClaudeAdapter`).

## Goal

The runner's first job: **notice every agent session on this machine and keep an up-to-date,
restart-safe index of them**. Emit the events the hub needs, without ever re-reading a
transcript from the start.

## What to build

1. **Runner configuration**, e.g. `RunnerConfig`, holding:
   - the workspace, machine and owner ids (the owner authors events for unnamed sessions; the hub
     may re-stamp);
   - engine homes;
   - a state directory.
2. **Runner-side store:** a small SQLite in the state directory, with its **own** migrations
   inside `crates/runner` (not the hub store's `migrations/` folder). It holds each transcript's
   path, engine, `Cursor`, size, mtime, its `SessionId` (a ULID assigned on first discovery and
   stable after), and the last known session facts.
3. **Discovery:** call each configured `SourceAdapter`'s `discover` on start and when an engine
   home changes. New transcripts get a `SessionId`.
4. **Watching:**
   - `notify` watches the directories of **hot** transcripts (modified in the last 24 h);
   - **cold** ones are re-checked on a slow schedule (size or mtime changed);
   - use **polling** where the filesystem is a network one (detect it, and allow forcing it with
     config).
   - Changes are debounced (e.g. 100 ms), and each change calls `read_from` with the stored
     cursor.
5. **Events:**
   - `session_discovered` on first sight, with the `Session` built from `SessionMeta`;
   - `session_state_changed` as items arrive: working while a tool call is open or text is
     streaming; waiting after a `Question`; idle after `TurnEnded`;
   - `tool_ran`, `file_edited` and `turn_ended`, each with transcript receipts.

   Events go to an `EventSink` trait through a **bounded** channel; the hub link comes in a
   later brief. Persist the cursor **after** the events are accepted by the sink, so a crash
   repeats rather than loses events. Dedupe on restart by `(session, offset)`.

## Acceptance

- With `FakeSource`: a new item becomes an event in under 300 ms. A restart resumes from stored
  cursors, and the fake's read log proves nothing was re-read.
- With the real Claude adapter, using a copy of the fixture transcript in a temp home: appending
  lines produces the right events. Truncating or replacing the file (a new inode, a smaller size)
  is detected and re-indexed from the start, with a note in the log.
- The sink channel full: the watcher applies backpressure, it doesn't drop events or grow
  memory.
- Idle CPU with 50 watched transcripts and no changes: report the number (budget ≤ 0.5% of one
  core).

## Out of scope

Hub commands and the terminal runtime, linking sessions to workstreams, the files API, the hub
link protocol (later briefs). Don't edit other crates; if the source trait needs a change,
describe it in your report.
