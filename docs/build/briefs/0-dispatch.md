# Brief 0 · Start an agent from a task, end to end

- **Stream:** 0 · Composition root (protocol, runner, work model and daemon together).
  **Branch:** `integrator/dispatch`.
  **Paths:**
  - `crates/protocol/src/runner.rs`;
  - `crates/runner/**`;
  - `crates/hub-work/**`;
  - `crates/daemon/**`;
  - `tests/conformance/**`;
  - `docs/build/contracts/api-v1.md`;
  - the READMEs of those crates, `docs/security/threat-model.md`, and the lockfiles.
- **First read:**
  - [README.md](README.md) and the root `CLAUDE.md` (or `AGENTS.md`);
  - `crates/hub-work/README.md` ("Dispatch", ~293-319), `crates/runner/README.md` ("Session ids today, and dispatch", ~98-117) and `crates/daemon/README.md` ("Dispatch", ~571-579);
  - `api-v1.md` "Dispatch" (~178-184) and "Sessions" (~198-203);
  - `docs/build/streams/D.md` item 5 and `E.md` items 3–4;
  - `tests/conformance/MISMATCHES.md` (rows D1, D2).

## Goal

A person picks an agent on a task and presses Dispatch (`apps/ui/src/projects/task-drawer.tsx`
`DispatchForm`). That agent's CLI starts on the right machine, in the workstream's folder, as a
session already linked to the task. The task moves to in progress when the agent starts working, and
to review when it reports done.

Today the daemon answers every dispatch with 503 and records nothing (`crates/daemon/src/serve.rs`
~255, built without `.with_dispatcher`). The reason is that the runner mints its own `SessionId` for
each transcript, so a dispatched CLI would appear as a second session with no agent, its hooks would
be refused, and the dispatch's own `starting` session would never move.

## What to build

1. **The protocol and contract:**
   - `RunnerCommand::StartSession` gains `session: Option<SessionId>`: the id the new session must
     be reported under. `DispatchRequest::start_command()` (`hub-work/src/dispatch.rs` ~106) fills
     it in.
   - Update `api-v1.md` if anything about dispatch or `POST /v1/sessions` changes.
2. **The runner adopts the id.**
   - Today the id is minted in `add()` (`watch.rs` ~1052, `new_row()` ~2156) before the transcript
     is read, and `claim_terminal` (`store.rs` ~485) only matches afterwards. Make `add()` take the
     session from a waiting `TerminalRow` when one matches:
     - **Claude:** an exact `native_id`, from the pre-chosen `--session-id` UUID;
     - **Codex and OpenCode:** folder and start time, as `claim_terminal` does, with the session
       pre-set on the row.
   - Two starts in one folder inside the claim window are ambiguous for Codex and OpenCode. Refuse
     the second dispatch there (409, saying why) rather than guess.
   - Re-statements keep `agent: None`; the hub keeps the agent and firm links
     (`projection/sessions.rs`). Sub-agents keep minted ids, with `parent` set.
   - Tests for each engine, using the existing stand-in CLIs, never real ones.
3. **The daemon wires the dispatcher.**
   - Implement hub-work's `Dispatcher` over the runner's `RunnerCommands`.
   - The runner is attached after `WorkService` exists (`Attached`, created in `serve.rs` ~516 and
     filled later by setup). So either build the dispatcher over an `Arc<Attached>` made in
     `open_with`, or add a late `set_dispatcher` to `WorkService` like `set_hub_machine`. Say which,
     and why.
   - `dispatch_task` checks the dispatcher before planning (`dispatch.rs` ~290). With a dispatcher
     present, the plan's 404/400/409 checks answer as the contract says (conformance row D2).
   - **Before setup, or with no runner attached:** 503 with a clear reason.
   - `POST /v1/sessions` with `agent` or `task` (`daemon/src/sessions.rs` ~344) stops refusing
     with 503, and goes through the same adoption.
4. **The task moves itself** (E.md item 3):
   - **In progress:** when the dispatched session first reports `working`, call
     `WorkService::dispatch_working` (`commands.rs` ~695, which has no caller today).
   - **Review:** the office rule `DispatchToReview` (`crates/office/src/rules.rs` ~38) already
     moves the task on `DispatchFinished{Succeeded}`, but nothing emits that.
     - Emit `Succeeded` when the agent reports done for the task (`pitcrew report <task> --done`).
     - Emit `Failed` when the session ends with an error or never starts, and something neutral if
       it ends without a report.
     - Write the rule into the contract.
   - Wire `mirror_plan` (`commands.rs` ~745) if it fits; otherwise say why it waits.
5. **Crash reconciliation.** If the daemon crashed between appending the dispatch and starting the
   CLI, the hub-work README's open case applies: a `starting` session plus an open dispatch. At start,
   adopt the session if its CLI really started; otherwise finish the dispatch as `Failed` and end the
   session. Test it.
6. **Security.**
   - Only a person can dispatch, as today.
   - The started CLI gets an **agent** token bound to that agent member and its owner (as I.md
     describes: `PITCREW_TOKEN` set by the runner), never the person's token. Verify this and test
     it.
   - Add or update a threat-model row.
7. **Conformance:** rows D1 and D2 pass against the daemon. Remove them from the expected
   deviations and from `MISMATCHES.md`.

## Acceptance

- An end-to-end daemon test with a stand-in Claude (and one for Codex):
  - dispatch;
  - the session appears once, under the dispatch's id, linked to the task;
  - the task goes to in progress on `working`, then to review on a done report.
- fmt, clippy with `-D warnings`, `cargo test --workspace --no-fail-fast` (in the background), the
  conformance suite against both targets, `npm test`, the guards, and every CI job pass.

## Out of scope

The dispatch queue, per-agent concurrency and hand-off (E.md item 4: a later brief); a remote
runner over the JSON-lines link; and UI changes beyond what's needed (the drawer already posts the
dispatch).
