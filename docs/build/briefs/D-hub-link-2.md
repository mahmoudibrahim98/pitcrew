# Brief D · Hub link, round 2: who may change a session through a hook

- **Stream:** D · Runner service. **Branch:** `s/D/hub-link-2`, started from the committed state
  of `s/D/hub-link`. **Paths:** `crates/runner/**` (+ `Cargo.lock`).
- **First read:** [README.md](README.md), [D-hub-link.md](D-hub-link.md) (this continues it), the
  `crates/runner` README, `crates/api` (`HookSink`, `HookEvent.caller`), and
  `crates/hub-work/src/projection/sessions.rs`. The integrator gives you the review notes from the
  first two rounds.

## Goal

Finish the in-process hub link so it can merge. The blocking gap from its review: a hook could
change any session's state, whoever sent it. Close it with the rule below, and make the runner
robust to floods and panics while it holds hooks for sessions it hasn't seen yet.

## What to build

1. **Bring the branch up to date:** merge `main` (the integrator authorises it) and resolve any
   conflicts inside `crates/runner` only.
2. **The hook ownership rule** (the integrator's decision; document it in the README and on
   `RunnerHooks`):
   - **Agent token:** the hook applies only to a session whose `agent` is the caller's member.
   - **Device token (a person):** the hook applies only to a session with no agent, or whose
     agent that person owns.
   - Anything else is dropped and logged at debug with the reason. It is never applied and never
     held.
   - **Held hooks** (session not indexed yet) keep their caller and are checked at discovery. One
     that fails is dropped.
   - A Codex `notify` follows the same rule.
   - Carry the caller from `HookSink::deliver` to where the session is resolved. The origin is an
     explicit enum (`Runner` or `Hook(sender)`), so skipping the check is visible.
   - When unsure who a session's agent is, answer "unknown", and treat unknown as deny.
3. **Held hooks under pressure:**
   - each sender has a quota (e.g. 32 held entries);
   - at its quota, a sender evicts its own oldest entry;
   - at the global cap, evict from the sender holding the most;
   - rate-limit, per sender, the rediscovery that a new key triggers.
4. **Panic safety:** the agent lookup runs on the watcher thread, so guard it; a panic counts as
   unknown.
5. **Docs:**
   - the runner mints its own session ids and emits no agent, while the hub keeps an existing
     agent on re-statements (check this on `main`);
   - `SessionAgents` implementations must see the hub's latest session writes;
   - the in-memory implementation is for tests, or for a host that fills it;
   - the agent lookup must not call back into the runner.

## Acceptance

- **Tests:**
  - an unrelated agent's hook changes nothing;
  - the right agent's hook works;
  - a person's hook works on an unowned session and on their own agent's session, but not on
    another person's agent's session;
  - a held hook from the wrong caller is dropped at discovery, and the right one is applied;
  - unknown, end to end;
  - a flood from one sender leaves another sender's held hook intact;
  - unit tests for holding and eviction;
  - the panicking lookup.
  - Each test's allowed and refused hooks differ in state or status line, so timing alone can't
    pass them.
- Everything in [D-hub-link.md](D-hub-link.md)'s acceptance still passes.
- fmt, clippy (Linux and Windows target), `cargo test --workspace`, `cargo deny check`, and the
  guards all pass.

## Out of scope

The daemon's composition (stream 0), the remote runner protocol, and the real runtimes (B).
