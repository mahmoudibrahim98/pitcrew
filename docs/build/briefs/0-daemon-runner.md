# Brief 0 · `pitcrewd` runs the runner: real sessions, hooks and terminals

- **Stream:** 0 · Composition root (integration work). **Branch:** `integrator/daemon-runner`.
  **Paths:** `crates/daemon/**` (+ `Cargo.lock`).
- **First read:** [README.md](README.md), [D-hub-link.md](D-hub-link.md) and
  [D-hub-link-2.md](D-hub-link-2.md) (merged), the `crates/runner` README (the in-process link:
  `StoreSink`, `RunnerHooks`, `RunnerTerminals`, `RunnerCommands`, `SessionAgents`, the hook
  ownership rule, `RunnerConfig`), the `crates/daemon` README ("Not wired yet"), the
  `crates/runtime` README (which runtimes exist), and `docs/build/contracts/api-v1.md` (hooks,
  terminals, sessions).

## Goal

The real daemon sees the agent sessions on its own machine, live: transcripts from Claude Code,
Codex and OpenCode flow into the store; agent hooks update session state at once (by the ownership
rule); and the console's terminal attaches to real terminals. Until now `pitcrewd` only had the
demo's data.

## What to build

1. **The runner in-process,** started by `pitcrewd serve`:
   - Watch the engine homes the runner already knows by default (the person's `~/.claude`, Codex
     home and OpenCode store).
   - `--homes <dir>…` overrides them, for tests and for people who keep their homes elsewhere.
     `--no-runner` turns the runner off.
   - **With `--demo`, no real home is watched unless `--homes` is given,** so a demo never shows
     a person's real sessions.
   - `StoreSink` writes into the daemon's store. Its configured owner is the workspace's person.
   - `roles` in `GET /v1/host/info` gains `runner` when it runs.
2. **Hooks:** the API's hook route delivers to `RunnerHooks`.
   - **`SessionAgents`, in the daemon, over hub-work's sessions and members.** It must see the
     hub's latest writes (no stale cache answering "no agent"), resolve a sub-agent session to its
     parent's agent where the data allows, and answer `Unknown` when unsure.
   - Pass it with `RunnerConfig::with_agents`.
3. **Terminals:** the terminal route uses `RunnerTerminals` over the runtime from `crates/runtime`
   (tmux where present, otherwise what the crate provides). A session with no terminal answers as
   the contract says.
4. **`pitcrewd connect`,** if `s/J/tunnel` has merged when you get there: the stdio-bridge CLI
   subcommand from the J-tunnel report (the integrator gives you the notes). Otherwise leave it.
5. **Shutdown:** the runner stops cleanly before the store closes, in the same order as the back
   office.
6. **Not in this brief:** the dispatcher (starting an agent for a task) waits until the runner
   adopts the dispatch's session id. Keep today's 503, and say why in the README.

## Privacy, firmly

Never run the daemon, its tests or the e2e against the real engine homes of the machine you work
on. That means `~/.claude`, `~/.codex` and OpenCode's store, in WSL and on Windows. They hold the
person's private transcripts. Every test and manual run uses temporary homes filled from
`crates/fixtures` (`--homes`). Never read, list or copy anything from a real home. The person will
try their own homes themselves.

## Acceptance

- **Tests** (`crates/daemon/tests`), with temporary homes:
  - the fixture Claude transcript appears as a session through the API, with its transcript;
  - an appended record shows up live on the stream;
  - a hook from the session's own agent changes its state, and one from another agent doesn't
    (through the real hook route and real tokens);
  - `--demo` without `--homes` watches nothing real (assert it on the runner's configured homes);
  - shutdown order.
- **The parity replay** still shows only expected differences (`roles` now includes `runner`).
- **The UI shell e2e against the daemon** still passes.
- fmt, clippy (Linux and Windows target), `cargo test --workspace`, `cargo deny check`, and the
  guards all pass.

## Out of scope

The dispatcher and session-id adoption, agent tokens for real sessions (how a real agent's hooks
authenticate is a separate design), creating the first person outside `--demo`, and remote
runners.
