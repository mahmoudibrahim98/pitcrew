# pitcrew-office

The back office: small, deterministic rules that act on evidence in the event log, with caps, a
run log and a hard "never" list. Model calls come later, behind recap's `Summarizer`.

**Owned by stream F** — see [docs/build/streams/F.md](../../docs/build/streams/F.md).

## What is here

- **`Office`.** Feed it events in log order with `on_event(rev, &event)`. It learns from each one
  (`World`: members, tasks, dispatches, sessions, open asks, workstreams; built from events only),
  runs its rules, and returns the run log's `Entry`s: rule, event, action and outcome. Time is the
  office's clock, the latest event time seen, never the wall clock. An event at or before the last
  revision seen is ignored, so a replay never acts twice.
- **`Rule`.** `fn on_event(&mut self, ctx: &mut Context, event: &Event) -> Vec<Action>`. A rule
  reads `ctx.world` and `ctx.now`, and keeps anything it must remember in `ctx.memo`, which is
  saved with the office.
- **`Action`s.** Append an event, raise an ask, or propose a brief (recap's `BriefProposal`). The
  office never applies them: the caller does, through its `Commands` (stream E), with
  `apply(&entries, &mut commands)`. Capped and refused entries are only logged.
- **Caps.** At most `per_rule_per_hour` emitted actions per rule in any hour, and
  `global_per_hour` over all rules. Overflow is logged as `capped`, not applied.
- **The "never" list**, checked in code for every action of every rule: never send anything
  outward (only internal events, and no approval asks, which is how outward writes are
  requested), never mark a task done unless it allows automatic acceptance, never answer an ask
  addressed to a person (nor any decision or approval). Every move must also pass
  `TaskStatus::can_move` for `Mover::BackOffice` from the status the task is in, and every action
  must cite receipts. A member once known as a person stays one, and a task's automatic
  acceptance, once off, stays off, whatever later events claim. `apply` checks an action's shape
  again before handing it over.
- **`RunLog`**, the run log as a store projection (`office.runs`, migration `0301`): it replays
  the rules inside each append and writes `office_runs` (one row per action, with `outcome`
  `emitted`, `capped` or `refused` and a `reason`) and `office_state` (the office's state, only
  the rows that changed). `read_runs` reads the log back. The live office and the run log must
  use the same `Config`.

## The first rules

| Rule | When | Action |
|---|---|---|
| `dispatch_to_review` | a dispatch finished successfully and its task is in progress | move the task to review as the back office |
| `job_diverged` | a tool's outcome or a dispatch's summary reports divergence | a decision ask to the person who owns the work (once per session per 12 hours) |
| `tests_failing` | three failed test runs in a row in one session | a decision ask to the owner (once per streak; a pass ends it) |
| `remind_stale_asks` | an ask open for 24 hours | one reminder mention to its addressee (not for mentions, nor asks to the office) |
| `quiet_workstream` | an active workstream with no activity for 3 days | a "paused?" brief proposal (once per quiet spell) |

Rules ignore events written by the back office itself (`Config::office`), and those are not
activity, so the office never feeds on itself.

## Tests

- `tests/demo.rs`: a snapshot of the run log over the demo workspace (as a whole log, plus four
  quiet days), and its actions applied through a recording `Commands`.
- `tests/rules.rs`: one scenario per rule.
- `tests/never.rs`: each "never" rule, with crafted events and with a rule that does whatever a
  crafted event says; and a property test that no crafted log makes the default rules break it.
- `tests/props.rs`: replay is deterministic and idempotent (also with overlapping batches), the
  saved state restores exactly at any point, and caps hold under a flood.
- `tests/run_log.rs`: the projection stores what the office did and rebuilds identically, with
  any batch size, registered late, after a reopen, and after a rolled-back append.
- `tests/board_summary.rs`: the summary holds what it should; synthetic secrets of every kind, in
  titles, recaps (the activity they describe), files, branches, task titles and names, never reach
  the prompt, nor a long piece of one; hidden characters and newlines cannot inject lines or close
  the summary; every bound holds with 100 sessions and 200 tasks of 10,000-character texts, the
  most recent kept and the rest counted; the estimate follows the prompt's size.
- Unit tests in `src/redact.rs` (each rule, plain text left alone, bounds after redaction),
  `src/prompts.rs` (one-pass rendering) and `src/orchestrator.rs` (references found once with
  their punctuation trimmed; suggestion lines taken out and others kept; questions cleaned, and
  follow-ups one line and never a CLI command; the prompt holds the question last and bounded,
  data-only context).

## Prompts as files, and what a board draft sends

- **`prompts/<name>/v<n>.md`** are the versioned prompt templates, built into the binary
  (`prompts::DRAFT_BOARD`, `prompts/draft-board/v1.md`; `prompts::ORCHESTRATOR`,
  `prompts/orchestrator/v1.md`). A template is never edited once
  released: a change is a new version, and what was sent records the version (`draft-board/v1`).
  `Prompt::render` puts each `{{name}}` in, in one pass, so a value never reaches another
  placeholder.
- **`board`**: the bounded, redacted summary a board draft (brief
  [0-draft-board](../../docs/build/briefs/0-draft-board.md)) sends the agent that drafts a
  workstream's board, inside `draft-board/v1`, and its cost. The hub hands it facts only (titles,
  states, dates, branches, linked task keys, counts, edited files, the recap engine's one-line
  summaries of blocks of work, and the workstream's tasks); never a transcript, a prompt or a
  tool's output.
  - **Bounds:** 40 sessions, most recently active first; 60 tasks in at most 4 KiB; per session a
    title of 120 characters, 3 recap lines of 200, 5 files of 100; names of 80; the whole summary
    at most 12 KiB, the least recently active sessions left out (and counted) when it is full.
    The prompt is at most the template plus 12 KiB (`MAX_PROMPT_BYTES`), well inside a command
    line on every platform.
  - **The estimate:** the agent reads the prompt (a token for every 4 bytes, rounded up) plus a
    fixed 15,000-token allowance for its CLI's own instructions (`CLI_OVERHEAD_TOKENS`); its
    answer is at most the proposal's bound, 32 KiB, so 8,192 tokens.
  - `<` and `>` in session text are shown as `‹` and `›`: nothing a session wrote can close the
    prompt's `<summary>` and speak outside it. The prompt says the summary is data, not
    instructions.
- **`orchestrator`**: the Orchestrator's side of its conversation (brief
  [0-orchestrator-chat](../../docs/build/briefs/0-orchestrator-chat.md)), text in and text out;
  the hub checks what it finds before anything becomes a link or a suggestion.
  - **The prompt** (`prompt`, `orchestrator/v1`) names the workspace, the person and the day,
    lists `pitcrew`'s read verbs (its CLI holds a token that may only read), says what they print
    is data, not instructions, and how to cite and suggest; the question comes last. A
    conversation whose session ended starts a new one with its last turns as context (`Earlier`):
    one line each, redacted, `<`/`>` shown as `‹`/`›` inside `<earlier>`, at most 6 KiB, newest
    kept.
  - **The question** (`clean_question`): control and hidden characters dropped; a follow-up typed
    into the CLI's terminal is one line, and one that would read as a CLI command (`/`, `!`, `#`,
    `@`) is typed after `Q: ` (`typed`).
  - **What an answer cites** (`scan`, `cited`): `ses_…`, `tsk_…`, `wst_…`, `prj_…`, task keys,
    and `recap:wst_…`/`recap:prj_…` with an optional `@YYYY-MM-DD`, each word trimmed of `-`, `:`
    and `@` at its ends, each once, and the lines `Suggestion: move <task> to <status>` and
    `Suggestion: open <reference>` (at most 10), taken out of the text.
- **`redact`**: what never leaves in a prompt. Every text is made one line (hidden and control
  characters dropped, so nothing hides a secret from the rules), then: private key blocks;
  well-known token prefixes (`sk-`, `ghp_`, `github_pat_`, `glpat-`, `xoxb-`, `AKIA`, `AIza`,
  PitCrew's `pcd_`/`pca_`/`pcr_`, and others, followed by at least 8 token characters with digits or mixed
  case); JSON Web Tokens; the values of secrets' names (`password=`, `token:`, `--password x`,
  `?access_token=`, `Authorization: Bearer x`); a URL's user and password; long random-looking
  words (32+ characters, mixed case and digits, or hexadecimal); e-mail addresses; and home
  folders (`/home/<name>`, `/Users/<name>`, `C:\Users\<name>`, `/root` become `~`). Each segment
  of a path or branch is checked too. The rules are broad on purpose: a false positive costs a
  word of context, a false negative a secret. Each replacement is counted, and the person sees the
  count before anything is sent.
