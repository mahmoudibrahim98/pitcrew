# pitcrew-ingest

Transcript parsers for Claude Code, Codex and OpenCode (incremental, by offset), and the machine scan.

**Owned by stream A** — see [docs/build/streams/A.md](../../docs/build/streams/A.md).

## Offsets and refs, for consumers

Every `TranscriptItem` has an `offset`, and every read returns a `Cursor`. What they mean depends
on the adapter.

**Claude Code and Codex (JSONL).** `offset` is the byte offset of the line the item came from.
Items of one line share it. `Cursor::offset` is a byte position in the file; `TranscriptRef::size`
is the file's length, so a smaller size means the file was truncated or replaced.

**OpenCode (SQLite).** One store holds many sessions; `TranscriptRef::inner_id` is the session.
- `offset` is an **ordering position, not a byte offset**: the part's creation time on OpenCode's
  id scale (milliseconds × 4096 plus a counter), at most 2^53 − 1.
- A tool call and its result share one offset (so do its plan, question and file-edit items).
- A result can arrive in a later read than its call, after items with higher offsets. Sort by
  offset to show a transcript in order; do not assume each read only appends higher offsets.
- Two items can share an offset: the items of one part, a failed turn's `TurnEnded` after its
  last part, or two parts created at the same position. **Never dedupe on `(session, offset)`
  alone**; use the offset with the item itself (its kind and content, or `call_id`).
- `Cursor::offset` is the frontier: everything at or below it has been emitted. The cursor
  resumes from `(offset, part id)` in its state, so pass the whole cursor back.
- `TranscriptRef::size` is a **change counter, not a size**: the highest rowid among the
  session's parts. It grows when parts are added, stays the same when a part is updated in place,
  and can go down when a revert deletes parts. A smaller value is **not** a truncation: keep the
  cursor. `modified` is the session's `time_updated`. Neither moves on every streaming update, so
  watch the store's directory (the database and its `-wal`) and call `read_from` on change.
- A read can fail with `io::ErrorKind::WouldBlock` while OpenCode holds or rewrites the store:
  retry later with the same cursor.
