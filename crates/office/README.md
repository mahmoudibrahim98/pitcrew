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
  most recent kept and the rest counted; the estimate follows the prompt's size; files say which
  file and not whose folder (`relative_to`, the end of other absolute paths, the file name of a
  long one).
- Unit tests in `src/redact.rs` (each rule, plain text left alone, bounds after redaction, a value
  after a lone separator or in quotes, a token behind punctuation, control characters that would
  split a secret, other absolute paths) and `src/prompts.rs` (one-pass rendering; the draft's
  prompt asks for `--file proposal.json` and to read nothing else).

## Prompts as files, and what a board draft sends

- **`prompts/<name>/v<n>.md`** are the versioned prompt templates, built into the binary
  (`prompts::DRAFT_BOARD`, `prompts/draft-board/v1.md`). A template is never edited once
  released: a change is a new version, and what was sent records the version (`draft-board/v1`).
  (`draft-board/v1` was changed before it was released, with #54: the proposal goes in
  `proposal.json` and `pitcrew board submit <draft> --file proposal.json`, since PowerShell and
  `cmd.exe` have no heredoc, and the draft reads nothing but its prompt.)
  `Prompt::render` puts each `{{name}}` in, in one pass, so a value never reaches another
  placeholder.
- **`board`**: the bounded, redacted summary a board draft (brief
  [0-draft-board](../../docs/build/briefs/0-draft-board.md)) sends the agent that drafts a
  workstream's board, inside `draft-board/v1`, and its cost. The hub hands it facts only (titles,
  states, dates, branches, linked task keys, counts, edited files, the recap engine's one-line
  summaries of blocks of work, and the workstream's tasks); never a transcript, a prompt or a
  tool's output.
  - **Bounds:** 40 sessions, most recently active first; 60 tasks in at most 4 KiB; per session a
    title of 120 characters, 3 recap lines of 200, 5 files of 100 (cut at their start, so the
    file name stays); names of 80; the whole summary at most 12 KiB, the least recently active
    sessions left out (and counted) when it is full. The prompt is at most the template plus
    12 KiB (`MAX_PROMPT_BYTES`); it goes in the draft's `prompt.md`, never on a command line.
  - **Files** are named relative to the session's folder or the workstream's
    (`relative_to`, which the hub calls); any other absolute path keeps only its end
    (`redact::path_tail`).
  - **The estimate:** the agent reads the prompt (a token for every 4 bytes, rounded up) plus a
    fixed 15,000-token allowance for its CLI's own instructions (`CLI_OVERHEAD_TOKENS`); its
    answer is at most the proposal's bound, 32 KiB, so 8,192 tokens.
  - `<` and `>` in session text are shown as `‹` and `›`: nothing a session wrote can close the
    prompt's `<summary>` and speak outside it. The prompt says the summary is data, not
    instructions.
- **`redact`**: what never leaves in a prompt. Every text is made one line (hidden characters and
  control characters other than whitespace dropped, never made spaces, so nothing hides or splits
  a secret), then: private key blocks; well-known token prefixes (`sk-`, `ghp_`, `github_pat_`,
  `glpat-`, `xoxb-`, `AKIA`, `AIza`, PitCrew's `pcd_`/`pca_`/`pcs_`, and others, followed by at
  least 8 token characters with digits or mixed case), also behind or inside punctuation
  (`**ghp_…**`, `$sk-…`, `#glpat-…`, the punctuation kept); JSON Web Tokens; the values of secrets'
  names (`password=`, `token:`, `--password x`, `?access_token=`, `Authorization: Bearer x`, and
  with a lone `:`/`=`/`:=`/`=>` or quotes between, `"password": "x"`, `password = x`); a URL's user
  and password; long random-looking words (32+ characters, mixed case and digits, or
  hexadecimal); e-mail addresses; home folders (`/home/<name>`, `/Users/<name>`,
  `C:\Users\<name>`, also as `\\?\C:\Users\<name>` and WSL's `/mnt/c/Users/<name>`, and `/root`
  become `~`); and any other absolute path, cut to its end (`path_tail`: its last two parts after
  `…/` when it has five or more, its file name when it has one, else `…`), so a user name in
  `/scratch/<group>/<user>` does not go. Each segment of a path or branch is checked too; `path`
  is `line` for a path, cut at its start. The rules are broad on purpose: a false positive costs a
  word of context, a false negative a secret. Each replacement is counted, and the person sees the
  count before anything is sent.
