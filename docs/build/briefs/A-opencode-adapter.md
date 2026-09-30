# Brief A · OpenCode adapter

- **Stream:** A · Ingest. **Branch:** `s/A/opencode-adapter`. **Paths:** `crates/ingest/**`
  only.
- **First read:** [README.md](README.md), then `docs/build/streams/A.md` (work package 3), the
  merged Claude and Codex adapters in `crates/ingest` (their limits, shared engine and tests are
  the standard), and the approximate fixture
  `crates/fixtures/data/transcripts/opencode/{schema,seed}.sql`.

## Goal

A `SourceAdapter` for **OpenCode** sessions, so all three CLIs are covered, with the same
guarantees as the other two:
- read-only;
- incremental;
- tail-first paging;
- bounded payloads;
- no panics on hostile data.

## First, find the real format (read-only)

OpenCode stores sessions differently across versions:
- newer versions use a **SQLite database** (sessions, messages and parts, with JSON payloads);
- older versions use **JSON files** under `storage/session`, `storage/message` and
  `storage/part`.

The data directory is typically `~/.local/share/opencode` (also on Windows, under the user
profile), or `$XDG_DATA_HOME/opencode`.
- **Inspect the real store on this machine read-only** (OpenCode is installed here): table
  names, columns, the JSON shapes of messages and parts (text, tool calls with state and
  output, patches or edits, todos, step boundaries), and how parts are updated while a turn
  streams.
- **Never copy real rows into the repository.** Write your own synthetic test data that
  mirrors the real *shape*, in `crates/ingest/tests/data/opencode/` (your path). In your report,
  list how the stream-0 fixture differs from reality, so the integrator can correct it.
- Support the format(s) you find. If both exist in the wild, support both behind one adapter,
  and say which versions you verified.

## What to build

1. **Discovery.** One `TranscriptRef` per OpenCode session, with `inner_id` = the session id,
   and `size`/`modified` from the session's update time. Sub-sessions (with a parent id) are
   marked `is_subagent`.
2. **Reading SQLite safely.**
   - Open **read-only**, e.g. a `mode=ro` URI (`rusqlite` via `workspace = true`, bundled).
   - Never take a write lock, never create files next to the database, and don't fail while
     OpenCode is writing (WAL). Use a short busy timeout, and retry later instead of blocking.
3. **Incremental cursor.** Parts are updated while a turn streams, so a byte offset doesn't
   apply. Keep resume data in `Cursor.state`, e.g. the last `(time_updated, id)` seen. A part
   that changes after it was emitted must not produce duplicate items. Define and document the
   rule: for example, emit a part only once it is complete, or re-emit with the same offset.
   `offset` must still be a stable, increasing position for receipts, such as a per-session
   sequence derived from creation order.
4. **Mapping to `TranscriptItem`:**
   - user text → `UserPrompt`;
   - assistant text → `AssistantText`;
   - tool parts → `ToolUse`/`ToolResult` (paired by call id; `is_error` from the tool state);
   - edits and patches → `FileEdit` with counts and a capped diff;
   - todos → `PlanUpdated`;
   - step or turn finish → `TurnEnded`.

   `SessionMeta`: id, directory as `cwd`, title, model, start time, all capped like the other
   adapters.
5. **`read_page`:** newest first, bounded work and memory, the same `from`/`to`/`at_start`
   semantics as the other adapters.

## Acceptance

- **Golden test** on your synthetic data: items and `SessionMeta`.
- **Incremental equivalence:** apply your synthetic rows in random batches (simulating streaming
  updates, including parts that change after first insert), reading after each. The union of
  items equals one read of the final state, with no duplicates.
- **Read-only proof:** a test opens a database, reads it, and asserts the file bytes and mtime
  are unchanged and that no `-wal`/`-shm`/journal file was created by the reader.
- **Edge cases:** an empty database, a missing table or column (an older or newer version),
  malformed JSON payloads, a huge part, and a database locked by a writer (retry, no hang).
- **Report:** which OpenCode version(s) and storage format you verified against, and read
  timings for your largest synthetic session.

## Out of scope

The scan (the next brief), watchers (stream D), and changes outside `crates/ingest`.
