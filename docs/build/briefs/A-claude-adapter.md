# Brief A · Claude Code adapter

- **Stream:** A · Ingest. **Branch:** `s/A/claude-adapter`. **Paths:** `crates/ingest/**` only.
- **First read:** [README.md](README.md) (setup, environment, rules, done, report), then
  `docs/build/streams/A.md`, ADR-0010, `crates/interfaces/src/source.rs`,
  `crates/protocol/src/transcript.rs`, `crates/fixtures/data/transcripts/claude/demo-session.jsonl`.

## Goal

A `SourceAdapter` for **Claude Code** transcripts in `crates/ingest`. It reads fast,
incrementally and read-only, and pages tail-first. This is work package 1 of your card, plus the
Claude half of work package 4.

## What to build

1. **Discovery.** `discover(home)` finds `<home>/projects/*/*.jsonl`, where `home` is `~/.claude`
   or a `CLAUDE_CONFIG_DIR`. It also finds sub-agent transcripts: files under a session's
   `subagents/` folder, or records marked `isSidechain: true`. Those get `is_subagent`. A missing
   home returns an empty list, not an error.
2. **`read_from`** (incremental; `Cursor.offset` is a byte offset):
   - Resume exactly at the offset; never re-read earlier bytes.
   - Don't consume an **incomplete last line**; the cursor stops before it.
   - Cap line length at 16 MiB. Skip longer lines and report them without failing the read.
   - Tolerate invalid UTF-8 and malformed JSON the same way: skip the line, keep going.
3. **Mapping records to `TranscriptItem`s:**
   - `UserPrompt`: typed prompts only. Not tool results, not injected context or command wrappers
     (for example `<command-name>`, `<system-reminder>` or local-command output).
   - `AssistantText` from text blocks.
   - `ToolUse`: `call_id` from the tool-use id; a short `target` (the command, path or pattern);
     `input` truncated to a sane size.
   - `ToolResult`: paired by `call_id`, with `is_error` and a short summary.
   - `FileEdit` from `Edit`, `MultiEdit` and `Write`: added and removed counts, plus the diff from
     `toolUseResult.structuredPatch` when present.
   - `PlanUpdated` from `TodoWrite`, mapping `pending`, `in_progress` and `completed`.
   - `Question` from `AskUserQuestion`: the question and its option labels.
   - `TurnEnded` from `stop_reason: "end_turn"` or a turn-duration system record.
   - Every item's `offset` is the byte offset of its record's line.
4. **`SessionMeta`:**
   - `native_id` is `sessionId`;
   - `cwd` and `branch` (`gitBranch`) come **from the records, never from the folder name**;
   - `title` is the custom title, else the summary, else the first prompt (trimmed);
   - `model` and `started` (the first record's timestamp) where recorded.
5. **`read_page(before, limit)`** reads **backwards** from `before` (or the end of the file) in
   blocks until it has `limit` items, keeping whole records. It returns a `TranscriptPage` with
   `from`, `to` and `at_start` as documented in the protocol and `api-v1.md`.

## Acceptance

- **Golden test** (`insta`, JSON snapshots) of the full item list and `SessionMeta` for the
  fixture transcript.
- **Property test** (`proptest`): reading the fixture, or generated transcripts, in random chunk
  sizes gives exactly the items of one full read, and total bytes read never exceed the file size.
- **Edge cases:** an empty file; a truncated last line that is completed later; a 20 MB line;
  invalid UTF-8; a record that is valid JSON of the wrong shape. None may panic, and all must give
  the correct items.
- `read_page`: the newest page, then older pages by `from`, reaches `at_start` and joins up to
  the full item list.
- Benchmark or ignored test: `read_page` on a generated 200 MB transcript returns in under 50 ms
  (report the number).

## Out of scope

Codex, OpenCode, the scan, watchers, and anything outside `crates/ingest`. If the real Claude
format differs from the synthetic fixture, say how in your report. The fixture belongs to
stream 0.
