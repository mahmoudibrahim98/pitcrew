# Brief E · The first run: setting up a fresh hub

- **Stream:** E · Work model. **Branch:** `s/E/setup`. **Paths:** `crates/hub-work/**`
  (+ `Cargo.lock`).
- **First read:** [README.md](README.md), `docs/build/contracts/api-v1.md` ("Host and workspace" and
  "The first run: `POST /v1/setup`"), `crates/protocol/src/api.rs` (`Setup`, `SetupPerson`,
  `SetupDone`), the `crates/hub-work` README (commands, single writer, routes, how
  `GET /v1/workspace` is served), and the `crates/daemon` README (start-up steps 2–7, which say
  how the device token acts before a person exists).

## Goal

A fresh hub, which has a device token but no person, machine or name, can be set up once through
the API. From then on it works like the demo does.

## What to build

1. **A command:** `WorkService::set_up(caller, Setup) -> SetupDone`, through the one writer.
   - Validate exactly as the contract says (lengths, the handle's shape, no control characters).
   - Refuse with conflict when the workspace already has a person, or when the handle is taken
     (e.g. by `@office`).
   - Append, in one append, `member_added` for the caller's member id (kind `human`, no owner)
     and `machine_added` (kind `local`, liveness `live`, a new id), authored by the caller.
   - Return the member and the machine.
2. **The routes,** in hub-work's routes:
   - `POST /v1/setup`, device tokens only (agent tokens get 403);
   - `GET /v1/workspace` gains `setup_needed` (true while no person exists; omitted when false).
3. **A seam for the daemon:** the workspace's name lives outside the event log (`workspace.json`),
   and the daemon must start its runner and back office after setup.
   - Give `WorkService` (or the routes) a small hook trait, e.g. `SetupListener` with
     `fn set_up(&self, done: &SetupDone)`, called after the append commits. Also a way for
     `GET /v1/workspace` to read the current name.
   - Document both for the daemon. Don't edit the daemon.
4. **Tests:**
   - each validation case;
   - setup succeeds once, then gives 409;
   - a handle clash gives 409;
   - an agent token gives 403;
   - after setup, `GET /v1/me` answers the person and `setup_needed` is false;
   - the listener is called exactly once, after the commit;
   - setup is idempotent under a retried request: a second identical call gives 409, never a
     second person.

## Acceptance

- The tests above pass. The demo seed and existing routes are unchanged.
- fmt, clippy (Linux and Windows target), `cargo test --workspace`, `cargo deny check`, and the
  guards all pass.

## Out of scope

The daemon's wiring (`workspace.json`, starting the runner, `pitcrewd init`; stream 0), the mock
(stream 0), and the onboarding UI (stream O).
