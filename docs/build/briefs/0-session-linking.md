# Brief 0 · Link sessions to workstreams: by folder and branch, and by hand

- **Stream:** 0 · Composition root (daemon, work model, console UI).
  **Branch:** `integrator/session-linking`.
  **Paths:**
  - `crates/daemon/**`;
  - `crates/hub-work/**`;
  - `crates/runner/**`, only for the small accessor item 1 needs;
  - `crates/protocol/**`, only if item 3 needs it;
  - `apps/ui/src/console/**` and `apps/ui/src/data/**` (item 4);
  - `apps/mock-hub/**`, `tests/conformance/**` and `docs/build/contracts/api-v1.md`;
  - the READMEs.
- **Start after `0-dispatch` merges.** Both change the runner's session handling and the daemon's
  wiring.
- **First read:**
  - [README.md](README.md) and the root `CLAUDE.md` (or `AGENTS.md`);
  - `crates/runner/src/link.rs` (the linker and its rules);
  - `crates/daemon/README.md` ~968 ("the daemon does not pass it the workstreams' locations yet");
  - `crates/daemon/src/agents.rs` (`HubAgents`, the model for the adapter);
  - `api-v1.md` sessions (~198);
  - `tests/conformance/MISMATCHES.md` (row D3);
  - `docs/build/streams/D.md` item 4 and `M.md` item 6.

## Goal

Today every session lands in "Unsorted". The runner already has a complete folder-and-branch linker
(`crates/runner/src/link.rs`: the deepest folder wins, branch beats folder at equal depth, a tie
links neither, and firm links are never overridden). But the daemon never gives it the workstreams'
locations, and a person can't link a session by hand. Make sessions land in their workstreams.

## What to build

1. **Locations from the hub:**
   - add a `HubLocations` adapter in the daemon, implementing the runner's `Locations` over
     `pitcrew_hub_work::query::workstreams` and `query::session` (for `link_of`), the way
     `HubAgents` does;
   - pass it with `RunnerConfig::with_locations`;
   - call the runner's `locations_changed()` when a workstream is created (the store's `subscribe()`,
     as the office loop uses it). `RunnerHandle` is private inside the daemon's `Runner`, so add the
     smallest accessor that does this.
2. **Manual links:** `POST /v1/sessions/{id}/link` (`{workstream?, task?}` → `Session`, emitting
   `session_linked` with `link_basis: "manual"`), as the contract says and the mock hub already does
   (`apps/mock-hub/src/routes.ts` `linkSession`):
   - 400 with neither field, and 400 when the task isn't in that workstream;
   - person-only, with an agent token getting 403 (the conformance suite now checks that);
   - the command lives in hub-work, the route in its `device_routes`.
3. **One meaning for `Imported`.** hub-work treats `imported` as inferred, so a later folder or
   branch link may replace it. The runner treats it as never overridden (`link.rs` ~10-12).
   - Decide which one the product wants. The onboarding import (a later brief) will set it.
   - Write it into the contract, and make both sides agree, with a test on each side.
4. **UI:** a "Link to…" action in the console's session row menu (M.md item 6). Pick a workstream
   and optionally a task, then post the link, and the session moves out of "Unsorted" through the
   stream.
5. **Conformance:** row D3 passes against the daemon. Remove it from the expected deviations and
   from `MISMATCHES.md`.

## Acceptance

- **Daemon tests:**
  - a workstream with a location;
  - a session started in that folder, and one on that branch, gets linked (`folder`, `branch`);
  - a workstream created later relinks existing sessions;
  - a manual link survives a later folder match.
- **UI:** unit tests and an end-to-end test of the link action against the mock hub, with an axe
  check.
- fmt, clippy with `-D warnings`, the workspace tests (in the background), the UI's checks, the
  conformance suite, the guards, and every CI job pass.

## Out of scope

Editing a workstream's locations or a project's root after creation (no event exists for it yet:
propose one in the report), remote runners over the JSON-lines link, and the onboarding import.
