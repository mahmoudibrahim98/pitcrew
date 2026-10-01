# Brief 0 · The daemon's first run: setup, `init` and `connect`

- **Stream:** 0 · Composition root (integration work). **Branch:** `integrator/daemon-setup`,
  based on `s/E/setup` until it merges. **Paths:** `crates/daemon/**`, plus the small hub-work
  additions named below, `apps/mock-hub/**` for the trimming rule, and `Cargo.lock`.
- **First read:** [README.md](README.md), `docs/build/contracts/api-v1.md` ("The first run:
  `POST /v1/setup`"), the `crates/hub-work` README ("The first run", `SetupListener`,
  `set_workspace_name`), the `crates/daemon` README (all of it: the state directory, start, stop,
  the back office, the runner, "Not wired yet"), and `crates/remote/README.md` (the bridge:
  `pitcrew_remote::bridge::main`).

## Goal

A fresh `pitcrewd serve` (no `--demo`) can be set up once, from the desktop's onboarding or from
the command line. After setup it works like the demo does: it keeps its name, the back office
runs, and the runner watches this machine. A remote machine's helper can be reached over SSH's
stdio through `pitcrewd connect`.

## What to build

1. **Setup in the daemon.** Register a `SetupListener` on the one `WorkService`.
   - **The listener runs under hub-work's writer lock.** It must not call back into the
     `WorkService` or append. It only hands `SetupDone` to a daemon task (a channel), which does
     the rest after the lock is released.
   - **That task** writes `workspace.json` (id and name; atomic, private, as `--demo` writes it),
     and then starts the back office and the runner exactly as a start with a person would.
   - **The back office's member,** `@office`, is added through the `WorkService`'s writer, never
     straight to the store, because the hub is serving by then. Add a small hub-work command for
     it if there is none (e.g. `ensure_office_member(owner) -> Member`, found or added), and use
     it at start too if that simplifies the start-up path.
   - **The hub's machine:** `with_hub_machine` is a consuming builder. Add a way to set it on the
     shared `WorkService` after setup (e.g. `set_hub_machine(&self, MachineId)`, interior
     mutability), so a dispatch for a task without a folder works after setup without a restart.
   - **On every start,** call `set_workspace_name` with the name from `workspace.json`.
   - **The runner's homes** follow the existing privacy rules: `--homes` when given, none with
     `--demo`, and otherwise this user's own. The tests always pass `--homes`.
2. **Trimming,** as the contract now says: the three names are trimmed, then counted in code
   points, and stored trimmed. Apply it in hub-work's `set_up` and in the mock hub. Add test
   cases for both: all-whitespace is `400`, and padded names are stored trimmed.
3. **`pitcrewd init`**, for people without the desktop: `pitcrewd init --workspace <name> --name
   <person> --handle <@handle> --machine <name>`.
   - It calls `POST /v1/setup` on the running daemon of that state directory, over its private
     transport, with the device token from `device.token`. Reuse an existing client if there is
     one (the `pitcrew` CLI's); don't open the store.
   - No running daemon is a clear error that says to start `pitcrewd serve`. The API's `400` and
     `409` answers are printed as the API's message.
   - It never prints the token.
4. **`pitcrewd connect`** (the stdio bridge for remote helpers): a subcommand that takes the
   bridge's own trailing arguments and returns `pitcrew_remote::bridge::main(args)`.
   - Dispatch it before `StateDir::resolve`, so no state directory is created on a remote.
   - stdout carries only the bridge's bytes; logs go to stderr.
   - The exit codes are the bridge's: 2 usage, 3 not our socket, 4 no daemon, 1 otherwise.
   - Report what `pitcrew-remote` adds to `pitcrewd`'s dependencies and binary size, since it
     ships as a static helper to remote machines.
5. **README:** update the state directory table, the start steps, "The back office", "The runner",
   and remove what is now wired from "Not wired yet".

## Acceptance

- **Tests** (a fresh state directory per test, `--homes` pointing at a temp dir, never a real
  home):
  - a fresh start: `setup_needed` is true, `GET /v1/me` is 404, host info has no `runner` role,
    and the office is off;
  - `POST /v1/setup`, then without a restart: `GET /v1/workspace` has the name, `workspace.json`
    holds it, `@office` exists (owned by the person) and acts (an existing office test's trigger
    works), and host info shows the runner. A transcript placed in the temp home appears as a
    session;
  - a restart after setup keeps the name, and starts the office and runner at once;
  - a second setup is `409`, and a concurrent pair of setups gives exactly one `200`;
  - `pitcrewd init` against a running daemon, its error with no daemon, and its `409`;
  - `pitcrewd connect --socket <dir>/run/pitcrewd.sock` (Unix) carrying an HTTP request to a
    running daemon through stdin and stdout, plus its usage error;
  - `--demo` is unchanged: the existing daemon tests all pass.
- `npm test` passes, with the mock's new trimming cases.
- fmt, clippy (Linux and Windows target), `cargo test --workspace`, `cargo deny check`, and the
  guards all pass.

## Out of scope

The onboarding UI (it calls `POST /v1/setup` through the gateway), the desktop's supervisor of the
local daemon, and a migration of `--demo` to the setup path.
