# Brief 0 · The daemon's terminals: TmuxRuntime and transcript pages

- **Stream:** 0 · Composition root (integration work). **Branch:** `integrator/daemon-runtime`.
  **Paths:** `crates/daemon/**`, the transcript paging paragraph of
  `docs/build/contracts/api-v1.md` (see 3), and `Cargo.lock`.
- **First read:** [README.md](README.md), the `crates/daemon` README (the runner, routes, stop),
  the `crates/runtime` README (`TmuxRuntime`, `TmuxOptions`, detection, attaching by hand, the
  review rounds' notes, and "call `screen()` from a blocking thread"), the `crates/runner` README
  ("For stream 0: replacing the daemon's stand-in", `RunnerTerminals`, `RunnerTranscripts`,
  `PageOptions`), and `docs/build/contracts/api-v1.md` (sessions, terminals, "Transcript paging").

## Goal

Sessions that PitCrew starts run in real terminals: on a machine with tmux 3.2 or newer, the
daemon's terminals are tmux windows on PitCrew's private server. A person can watch, type into,
resize and end them from the app, or `tmux attach` by hand, and they survive a daemon restart.
Transcript pages come from the runner itself.

## What to build

1. **The runtime.**
   - At start, detect tmux off the async executor (`detect_async`).
   - When it is usable, give `TmuxRuntime` to the runner's terminals in place of `NoRuntime`, and
     report `Capability::Tmux` in host info.
   - Otherwise keep `NoRuntime`, and log why (no tmux, too old, the socket directory refused).
   - The socket follows the runtime's default (a private `$XDG_RUNTIME_DIR`, else
     `/tmp/pitcrew-<uid>`). A hidden option or an environment variable (e.g.
     `PITCREW_TMUX_SOCKET`) sets it for tests and development. **Tests never use the default**,
     so they can't touch a real PitCrew's terminals.
   - Every call that may block (`screen()` above all) runs on the blocking pool, bounded as the
     terminal route already is.
2. **Stop.** Drop the runtime after the runner has stopped, so the terminals' exact offsets are
   stored and a restart resumes from them. Terminals themselves keep running: a stop never kills
   an agent. The stop stays bounded (about 20 seconds).
3. **Transcript pages.** Replace the daemon's stand-in with the runner's `RunnerTranscripts`, as
   the runner README says.
   - Delete `Found`, `Recorded` and `names`.
   - Keep the route's own checks (400 for a bad query, 404 for an unknown session, 503 for
     another machine or no runner).
   - Map the runner's answers, and write this into the contract's "Transcript paging":
     - a session this hub's runner has not indexed (a demo session, or a dispatched one whose
       transcript doesn't exist yet) answers **an empty page with `at_start: true`**, as today;
     - a transcript that is gone or can't be read, and a busy or timed-out read, answer
       `503 unavailable`.
4. **README:** the runner's terminals, attaching by hand (`tmux -S <socket> attach`), the stop,
   and the transcript route. Remove what is now wired from "Not wired yet".

## Acceptance

- **Tests** (where tmux 3.2+ exists, skipped with a message otherwise; temp state dir, temp
  socket, `--homes` in a temp dir, a fake `claude` on `PATH` as the runner's own tests do):
  - `POST /v1/sessions` starts a terminal running the fake CLI;
  - the terminal WebSocket streams its output;
  - `send`, `keys`, `interrupt` and `end` reach it;
  - host info reports `Capability::Tmux`;
  - after a daemon restart, the session's terminal is found again and output resumes from its
    offset;
  - the stop leaves the terminal running, and a later `end` kills it;
  - no tmux server or process is left after the tests (a marker and a sweep, as the runtime's
    tests do).
- **Transcript tests:**
  - an indexed transcript's pages;
  - an unindexed session's empty page;
  - a deleted transcript's 503.
- With no tmux (simulate it with a bad socket directory or a missing binary), the daemon serves
  as today with `NoRuntime`, and host info has no `Tmux`.
- fmt, clippy (Linux and Windows target), `cargo test --workspace`, `cargo deny check`, and the
  guards all pass.

## Out of scope

The PTY runtime (stream B's next brief), the dispatcher and session-id adoption (stream D), and
the desktop's terminal view (already built against the route).
