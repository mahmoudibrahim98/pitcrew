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
