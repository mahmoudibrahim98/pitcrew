# Brief 0 · The mock hub's first run

- **Stream:** 0 · Contracts (integration work). **Branch:** `integrator/mock-setup`. **Paths:**
  `apps/mock-hub/**`.
- **First read:** [README.md](README.md), `docs/build/contracts/api-v1.md` ("Host and workspace" and
  "The first run: `POST /v1/setup`"), `crates/protocol/src/api.rs` (`Setup` and friends), and the
  `apps/mock-hub` README, sources and tests.

## Goal

The UI's onboarding (stream O) can build the first-run screens against the mock, before the real
daemon supports setup.

## What to build

1. **`GET /v1/workspace` returns `setup_needed`:** false for the demo, true in fresh mode.
2. **Fresh mode,** with `PITCREW_MOCK_FRESH=1`:
   - the workspace has no members, machines or work (projects, tasks, sessions, asks, events);
   - the dev device token acts as a member id nothing knows;
   - `GET /v1/me` answers 404 until setup.
3. **`POST /v1/setup`,** exactly as the contract says:
   - validation (400);
   - once only, with 409 after setup or on a handle clash;
   - 403 for an agent token.
   - It appends `member_added` and `machine_added` to the mock's event log and stream, so a
     connected UI sees them live. After setup the person is the dev token's member.
   - In demo mode it always answers 409.
4. **Tests** for every case, in both modes, including the stream receiving the two events. Update
   the mock README.

## Acceptance

- `npm test` passes, with the new tests.
- The UI's existing suites still pass against the mock in demo mode. Run the shell e2e at least.

## Out of scope

The real daemon (streams E and 0), and the onboarding screens (stream O).
