# Stream A · Ingest

**Goal:** read every agent CLI's transcripts on a machine **fast, incrementally and read-only**,
turning them into `TranscriptItem`s and session facts. Also the machine **scan** used by
onboarding.

**Owns:** `crates/ingest/**`.  **Depends on:** stream 0.  **Model:** Opus-class.
**Read first:** ADR-0010, ADR-0002; `crates/interfaces/src/source.rs`;
`crates/protocol/src/transcript.rs`; `crates/fixtures/data/transcripts/`.

## Work packages

1. **Claude Code adapter.** Discover `~/.claude/projects/*/*.jsonl` (and a `CLAUDE_CONFIG_DIR`
   home); sub-agent transcripts are marked `is_subagent`. Map records to items:
   - user prompts (not tool results, not injected context or command wrappers);
   - assistant text; `tool_use` / `tool_result` paired by `call_id`;
   - edits (`Edit`, `MultiEdit`, `Write`) with added/removed counts and the diff when recorded;
   - `TodoWrite` → `PlanUpdated`; `AskUserQuestion` → `Question`;
   - turn ends (`stop_reason: end_turn`, turn-duration records).
   - Session facts: `cwd` and `gitBranch` **from the records** (never from folder names), title
     (custom title > summary > first prompt), model, start time.
2. **Codex adapter.** `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl` (and `CODEX_HOME`):
   `session_meta`, `turn_context`, messages, `function_call` / `function_call_output`
   (`shell`, `update_plan`), `custom_tool_call` (`apply_patch` → `FileEdit`), `task_complete`.
3. **OpenCode adapter.** Its SQLite store (sessions, messages, parts). The fixture schema is an
   approximation: check a real store and correct the fixture through a contract change.
4. **Incremental and tail-first.** `read_from` resumes at a byte offset and never re-reads;
   an incomplete last line is not consumed. `read_page` reads **backwards** from `before`.
   Cap line length (e.g. 16 MiB): skip and report oversized or invalid lines, never fail the file.
5. **Scan.** Walk engine homes on a machine, streaming progress: counts per engine, account,
   folder and month, plus suggested projects (repo roots) and workstreams (active sub-folders,
   feature branches). Propose the scan API types with O (see `contracts.md`).
6. **Golden tests and benchmarks.** Golden JSON outputs for the fixtures (`insta`). Maintainers
   with access to the predecessor's parsers can compare against private corpora locally in
   `crates/ingest/tests/private/` (git-ignored). Benchmark a large synthetic transcript.

## Acceptance

- Each adapter passes golden tests on the committed fixtures.
- **Property:** reading a file in N random chunks yields exactly the items of one full read
  (proptest), and total bytes read ≤ file size (no re-reads).
- Truncated last line, invalid UTF-8, a 50 MB line, and an empty file: no panic, correct items.
- `read_page` on a 200 MB synthetic transcript returns the newest page in < 50 ms.
- First scan of 10,000 small transcripts on SSD in < 60 s (benchmark, reported not gated yet).

## Security

Transcripts are attacker-controllable text. No `unsafe`; bounded allocations; expose
`parse_line`-level functions so stream Q can fuzz each parser.

## Do not

Write to any CLI file; copy transcripts; read OAuth tokens or credential files; touch the
runner's watcher (stream D) or the API (stream H).
