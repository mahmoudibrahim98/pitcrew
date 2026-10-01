# Brief 0 · `pitcrewd` wires the back office and the activity index

- **Stream:** 0 · Composition root (integration work). **Branch:** `integrator/daemon-wiring`.
  **Paths:** `crates/daemon/**` (+ `Cargo.lock`).
- **First read:** [README.md](README.md), [0-daemon-solo.md](0-daemon-solo.md) (merged), the
  `crates/daemon` README, the `crates/hub-work` README (sections "Wiring" and "The back office"),
  `crates/office` (`Config`, `Office`, `RunLog`), and the `crates/api` README (the `EventRefs`
  seam and `Activity::new(..).with_refs(..)`).

## Goal

The real daemon runs the back office, and answers the project and workstream activity filters,
exactly as stream E and stream H documented.

## What to build

1. **The activity index:** mount `pitcrew_api::Activity::new(events).with_refs(..)` with the
   `WorkRefs` adapter from stream H's report (it is in the `crates/api` README). Clone the
   `WorkService` before it moves into `device_routes()`.
   - Remove the daemon README's note that `project=` and `workstream=` answer 400.
   - The parity replay's `task=` and `project=` rows should now match. Re-run the parity replay
     and report.
2. **The back office:**
   - **Its member.** Create (or reuse, on restart) the office's agent member `@office`, owned by
     the workspace's owner.
     - Decide how it's created: an event appended at first start, or seeded for `--demo`.
     - Document it.
     - Its token, if it needs one, never leaves the daemon.
   - **Build it.** Build the `BackOffice` with stream F's default rules and `Config`, and register
     its run log with `projections_with_office`.
   - **Run it.** Run the loop exactly as the hub-work README's "Wiring" describes:
     - `subscribe()`, then read `last = latest_rev()` straight away;
     - for each batch, run `run_office(last + 1 ..= to_rev)` on the blocking pool;
     - move `last` forward only on success;
     - on a lag, use `latest_rev()` as the upper bound;
     - after a restart, safely re-run the last range (`run_office` is idempotent).
   - **A switch.** Add `--no-office` to turn it off. It is on by default; say why in the README.
3. **Shutdown:** the office loop stops cleanly on SIGTERM, SIGHUP or Ctrl+C before the store
   closes.
4. **Tests** (`crates/daemon/tests`):
   - with `--demo`, a dispatch that finishes moves its task to review, authored by `@office`;
   - a forbidden action is refused and logged;
   - a restart doesn't duplicate office actions;
   - `GET /v1/events?project=…` answers 200 with the right events.

## Acceptance

- The parity replay shows only expected differences, and the activity rows now match.
- The UI e2e against the daemon still passes.
- fmt, clippy (Linux and Windows target), `cargo test --workspace`, `cargo deny check`, and the
  guards all pass.

## Out of scope

The runner link (D is not merged), the hook-only token, GitHub and Jira.
