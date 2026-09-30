# Stream D · Runner service

**Goal:** the runner on each machine: watch transcripts, derive session state and events, link
sessions to workstreams, run the hub's commands in terminals, and serve files from project
locations safely.

**Owns:** `crates/runner/**`.  **Depends on:** stream 0; A and B **through traits only**.
**Model:** Opus-class.
**Read first:** ADR-0009, ADR-0005, ADR-0010; `crates/protocol/src/runner.rs`;
`crates/interfaces/`.

## Work packages

1. **Watchers.** `notify` on the directories of hot transcripts (active in the last 24 h); a slow
   schedule for cold ones; **adaptive stat polling** on network filesystems. Each change calls
   the adapter's `read_from` with the stored cursor.
2. **Runner-side state.** A small SQLite (its own migrations inside this crate, since it is not
   the hub store): transcripts (path, engine, cursor, size, mtime), session index, files touched.
   Restart resumes from cursors and never rescans.
3. **Session state and events.** From items, hooks and the runtime, derive `SessionState`
   (starting, working, waiting, idle, ended, unreachable) and emit `RunnerToHub::Events`:
   `session_discovered`, `session_state_changed`, `tool_ran`, `file_edited`, `turn_ended`,
   `session_ended`, with transcript receipts.
4. **Linking.** Match a session's `cwd` / branch against workstream locations sent by the hub
   (`link_basis: folder | branch`). Never override `dispatch`, `claimed` or `manual` links.
5. **Commands.** Handle `HubToRunner::Command` (`StartSession`, `ResumeSession`, `SendText`,
   `SendKeys`, `Interrupt`, `EndSession`, `ResizeTerminal`, `ReadTerminal`, `Scan`) via the
   `Runtime`, idempotent by `CommandId`, answering `CommandResult`.
6. **Files API** (propose the contract): list, read and write inside project locations only;
   canonicalise paths, reject symlink escapes, cap sizes, back up before writes.
7. **Hub link.** The runner side of the JSON-lines protocol with resume by cursor, heartbeat,
   and backpressure (bounded channels).

## Acceptance

- With `FakeSource` + `FakeRuntime`: a new transcript item becomes an event in < 300 ms; a
  restart resumes without re-reading (asserted by the fake's read log).
- With real files in a temp dir and the stream A adapters (once available): appending lines to a
  fixture transcript emits the right events.
- Linking rules table-tested, including "manual beats folder".
- Files API rejects `..`, absolute paths outside roots, and symlinks pointing out.
- Idle CPU with 50 watched sessions ≤ 0.5% of a core (benchmark).

## Do not

Parse transcript formats (A), implement terminals (B), store hub tables (C, E), or open TCP ports.
