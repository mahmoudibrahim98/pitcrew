# Brief A · Codex adapter

- **Stream:** A · Ingest. **Branch:** `s/A/codex-adapter`. **Paths:** `crates/ingest/**` only.
- **First read:** [README.md](README.md), then `docs/build/streams/A.md`,
  [A-claude-adapter.md](A-claude-adapter.md) (merged; reuse its framing and limits), the current
  `crates/ingest` code on `main`, and the fixture
  `crates/fixtures/data/transcripts/codex/rollout-demo.jsonl`.

## Goal

A `SourceAdapter` for **Codex CLI** rollouts, with the same guarantees as the Claude adapter:
- incremental and read-only;
- tail-first paging;
- every payload bounded;
- no panics on hostile input.

This is work package 2 of your card.

## What to build

1. **Discovery.** Find `<home>/sessions/YYYY/MM/DD/rollout-*.jsonl`, where `home` is `~/.codex`
   or `CODEX_HOME`, plus `<home>/archived_sessions/` if it exists. A missing home returns an
   empty list. Don't follow symlinks out of the home (use `DirEntry::file_type`).
2. **Reuse, don't copy.** Use the shared line framing (`lines.rs`), text limits (`text.rs`) and
   skip logging from the Claude adapter. Move shared pieces into common modules where needed,
   and keep the Claude adapter's tests green.
3. **Mapping** (`TranscriptItem`):
   - **`UserPrompt`:** typed prompts only. `event_msg` / `user_message` and `response_item`
     user messages record the same prompt twice; emit it **once**. Exclude injected context
     (`<environment_context>`, `<user_instructions>` and similar wrappers).
   - **`AssistantText`:** `event_msg` / `agent_message` and `response_item` assistant
     `output_text` also duplicate each other; emit once.
   - **`ToolUse` / `ToolResult`:** `function_call` / `function_call_output` paired by `call_id`.
     - For `shell`, the target is the command array joined for display, and the result's
       `is_error` comes from the output's `metadata.exit_code != 0`.
     - `custom_tool_call` / `custom_tool_call_output` pair the same way.
   - **`FileEdit`:** from `apply_patch`, one item per file in the patch, with added and removed
     line counts and the per-file diff (capped).
   - **`PlanUpdated`:** from `update_plan` arguments (`plan[].step`, `status`).
   - **`TurnEnded`:** from `event_msg` / `task_complete`.
   - Ignore `reasoning` items and `token_count`.
4. **`SessionMeta`:**
   - `native_id`, `cwd`, `branch` (`git.branch`) and `started` from `session_meta`;
   - `model` from `turn_context`;
   - `title` from the first prompt, since Codex has no title record.
   - Cap every fact the way the Claude adapter does.
5. **`read_page`** reads backwards in bounded blocks, like the Claude adapter, with the same
   `from`/`to`/`at_start` semantics and a bound of about `limit + 1` on records held.
6. **Leftover from the Claude review.** `read()` keeps an unbounded `skipped: Vec<SkippedLine>`,
   so a 200 MB file of junk lines would allocate about 10 GB. Keep the first N entries (say 100)
   and count the rest, in the shared code so both adapters get it. Add a test.

## Acceptance

- **Golden test** (`insta`) of the items and `SessionMeta` for the Codex fixture, with no
  duplicated prompts or assistant texts.
- **Property tests:** random chunking gives the same items as one read, cutting over the whole
  file range (`0..=len`), and bytes read ≤ size (plus the documented large-partial-line
  exception).
- **Edge cases:**
  - an empty file;
  - a truncated last line completed later;
  - a line over the cap;
  - invalid UTF-8;
  - valid JSON of the wrong shape;
  - `before` inside a line;
  - `limit = 0`;
  - a cursor past EOF;
  - CRLF;
  - an `apply_patch` that touches several files;
  - a malformed patch.
- `read_page` walks from newest to `at_start` and joins up to the full item list.
- The Claude adapter's tests still pass unchanged (or improved).
- **Report the numbers:** `read_page` on a generated 200 MB rollout, and a full `read_from` of it.

## Out of scope

OpenCode, the scan, watchers, and the typed-serde rewrite of the Claude parser (a later brief).
