# Brief D · In-process hub link, hooks and terminals

- **Stream:** D · Runner service. **Branch:** `s/D/hub-link`. **Paths:** `crates/runner/**`.
- **First read:** [README.md](README.md), then [D-watch-and-index.md](D-watch-and-index.md)
  (merged), `docs/build/streams/D.md` (work packages 3–5 and 7), ADR-0009, and on `main`: the
  `EventSink` in `crates/runner`, `crates/store` (`append_new`, `log_id`), `crates/api`
  (`HookSink`/`HookEvent`, and `Terminals`/`Attachment` in `terminal.rs`, **including the
  contract rules in its docs**), and `crates/interfaces/src/runtime.rs`.

## Goal

Connect the runner to the hub **in the same process** (the solo case in ADR-0009), so the daemon
(stream 0) can wire: runner → store; agent hooks → session state; the API's terminal WebSocket →
real terminals.

## What to build

1. **`StoreSink`:** an `EventSink` that writes accepted batches to `pitcrew_store::Store` with
   `append_new`, so a re-sent tail after a crash is stored once. Stamp events for sessions
   without an agent with the configured owner, as the runner already does.
2. **`RunnerHooks`:** a `HookSink` that turns agent hooks into session state:
   - Claude `SessionStart`, `UserPromptSubmit`, `Stop`, `SessionEnd`, `Notification`;
   - Codex's notify payloads.

   Map the hook to the runner's session: the `session_id` in the payload is the CLI's native id;
   use the runner index to find the `SessionId`. Emit `session_state_changed` and
   `session_ended` promptly (hooks beat the transcript watcher on latency). The two sources
   must agree: dedupe against transcript-derived state, and never go backwards on a stale hook.
   Unknown hooks are ignored.
3. **`RunnerTerminals`:** implements `pitcrew_api::terminal::Terminals` over any
   `pitcrew_interfaces::runtime::Runtime`. The runner owns the session → terminal mapping
   (terminals it started, and `native_target` for tmux ones it finds). Honour the `Attachment`
   contract:
   - `exited()` is true only once all output is readable;
   - every call is bounded in time;
   - provide the optional `changes()` push hint if the runtime can give one.
4. **Linking (work package 4):**
   - Match a session's `cwd` or branch against workstream **locations** via a small
     `Locations` trait, so this doesn't depend on stream E's tables. Provide an in-memory
     implementation.
   - Emit `session_linked` with basis `folder` or `branch`.
   - Never override `dispatch`, `claimed` or `manual` links.
5. **Commands (work package 5), in-process form:**
   - `start_session`, `send_text`, `send_keys`, `interrupt` and `end_session`, via the
     `Runtime`, idempotent by `CommandId`;
   - with `FakeRuntime` in tests. The real tmux and PTY runtimes come from stream B.

## Acceptance

- With a temp `Store`: the fixture transcript flows through the runner into the store exactly
  once, even across a simulated crash-and-resend.
- **Hooks:** a `Stop` hook moves a working session to idle before the transcript watcher does,
  and a stale hook doesn't regress state.
- **Terminals:** the API's terminal integration tests pass against `RunnerTerminals` over
  `FakeRuntime`.
- **Linking** is table-tested: longest-prefix folder match, branch match, manual links win.
- Idle CPU is unchanged (report it).

## Out of scope

The remote runner protocol over the network (a later brief), real tmux and PTY runtimes (B), the
daemon's composition (stream 0).
