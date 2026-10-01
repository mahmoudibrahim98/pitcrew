# pitcrew-ingest

Transcript parsers for Claude Code, Codex and OpenCode (incremental, by offset), and the machine scan.

**Owned by stream A** — see [docs/build/streams/A.md](../../docs/build/streams/A.md).

## Reading only regular files

Discovery lists regular files only, and does not follow links below a home. A read comes later,
so every read opens the transcript again without following a link in its **last** component
and checks, on the opened handle, that it is a regular file (`src/open.rs`):

- **Unix:** `O_NOFOLLOW | O_NONBLOCK`, then `fstat`. A named pipe cannot block the open.
- **Windows:** `FILE_FLAG_OPEN_REPARSE_POINT`, then the handle's attributes: a symbolic link, a
  junction, any other reparse point or a folder is refused.
- **OpenCode** stores, which SQLite opens by path: the check above, then on Unix SQLite gets the
  store's folder resolved and `SQLITE_OPEN_NOFOLLOW` (it opens every file with `O_NOFOLLOW`
  too), so a link put in place of the store after the check is refused as well; on Windows the
  checked handle is held without delete sharing until SQLite has opened the store, so the file
  cannot be renamed or replaced in between.

Folders above the transcript may be links (a home on another drive). This applies to
`read_from`, `read_page`, OpenCode's discovery (which opens each store) and the scan's 64 KiB
prefix reads.

A refused file is `SourceError::Io` carrying a `NotRegularFile` (its path and what is there:
`FileKind::Link`, `Directory`, `Fifo`, …); `pitcrew_ingest::refusal(&err)` finds it. Its message
names the path and the kind, never the contents. Callers treat it like any other read error: the
runner warns once per file and does not read it again until it changes; discovery of an OpenCode
home skips such a store; the scan counts it unreadable.

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

## The scan, for consumers

`scan::scan(homes, options, progress)` is a read-only, bounded walk of a machine's agent history,
built for onboarding's scan step and "scan again". It is **not** an import: it never produces
`TranscriptItem`s, never reads a full transcript, and never copies prompt text.

- **Bounded reads.** Each adapter's `discover` finds the transcripts; each one's session facts
  (`cwd`, `branch`, start time, sub-agent flag) come from a 64 KiB prefix of a JSONL file, or one
  indexed row of an OpenCode store — never the rest of the file. Because of this, `cwd`/`branch`
  reflect a session's **start**, not a later change (unlike the adapters' own "latest" `branch`).
- **Parallel, not racy.** The light reads run on a bounded pool of threads (`ScanOptions.threads`,
  default: the machine's parallelism) that claim work from a shared queue, so one huge OpenCode
  store does not stall the others. `progress` is only ever called on the caller's own thread, at
  most every 100 ms.
- **Suggestions.** A project is the nearest `.git` ancestor of a `cwd`; cwds with no `.git` above
  them are grouped under a shared parent once at least two of them share one, else each is its own
  project. A project's workstreams come from its sessions' first-level sub-folders and non-default
  branches. Both are ranked by sessions in the last 30 and 90 days. The user's home directory, a
  scanned engine home, and well-known system folders are never suggested.
- **Errors are warnings.** An unreadable home, folder or transcript is skipped and counted in
  `ScanReport::unreadable`; the rest of the scan still runs. Adapters already do not follow
  directory symlinks, so a symlink cycle cannot make the walk hang.

### Proposed move into `pitcrew-protocol`

`ScanReport`, `ScanProgress`, `EngineCount`, `HomeCount`, `FolderCount`, `MonthCount`,
`Suggestion` and `WorkstreamSuggestion` (all in `scan.rs`, all `Serialize`/`Deserialize`) are
written to become the wire types for the planned `POST /v1/machines/{id}/scan` contract
(`docs/build/contracts.md`'s "Machine scan API", proposed by A + O). They would move to a new
`pitcrew_protocol::scan` module unchanged; `ScanHome` and `ScanOptions` stay here; they describe a
local filesystem walk, not something the wire protocol needs to name (the hub would build a
`Vec<ScanHome>` from `MachineInfo`/account state and call `scan::scan` itself, or a future runner
command would). See this crate's final report for the exact diff against the onboarding UI's
proposed `ScanResult`/`ScanProgressEvent` shapes (`apps/ui/src/onboarding/api.ts`).
