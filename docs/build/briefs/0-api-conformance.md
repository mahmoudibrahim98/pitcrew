# Brief 0 · One API test suite, run against the mock hub and the real daemon

- **Stream:** 0 · Contracts (the API contract and the mock hub are stream 0's; stream H proposed
  this). **Branch:** `integrator/api-conformance`.
  **Paths:**
  - a new `tests/conformance/**` (register it for stream 0 in `docs/build/ownership.json` and
    `ownership.md`);
  - `apps/mock-hub/**`, only to fix where it breaks the contract;
  - a new workflow file under `.github/workflows/`;
  - the root `package.json` and lockfile if the suite joins the pnpm workspace.
- **First read:**
  - [README.md](README.md) and the root `AGENTS.md` (or `CLAUDE.md`);
  - `docs/build/contracts/api-v1.md` (the whole contract);
  - `apps/mock-hub`'s README;
  - `crates/daemon`'s README ("serve", `--demo`, `--listen tcp:`, the token files) and
    `crates/daemon/tests/common/mod.rs` (how its tests start a daemon);
  - `docs/build/streams/H.md` (item 8).
- **Suggested agent:** Codex, or any coding agent.

## Goal

The UI is developed against the mock hub and runs against the real daemon. Nothing checks that both
answer the contract the same way. Write one black-box suite that runs against either, and find where
they differ.

## What to build

1. **The suite** (Node's `node:test`, or vitest if the workspace already uses it for Node):
   - it takes a base URL and two tokens (a person's and an agent's) from the environment, so it
     knows nothing about which server it is testing;
   - cover every route in `api-v1.md`: status codes, response shapes (validate them, e.g. with
     schemas written from the contract), error bodies, query filters and paging (`before`, `limit`,
     the edges: `limit=0`, a bad cursor);
   - **auth:** no token gives 401; an agent token on a person-only route gives 403;
     `GET /v1/host/info` needs no token;
   - **the delta stream** (`/v1/stream`): hello, replay from `since`, and a change made through the
     API arriving as a frame;
   - the terminal WebSocket only as far as the contract defines it without a real terminal (e.g.
     refusals).
2. **Two runners:**
   - **against the mock hub:** start it on a free port with its dev tokens;
   - **against the real daemon:** build `pitcrewd` and start it as
     `pitcrewd --state-dir <fresh tmp> serve --demo --listen tcp:127.0.0.1:<port>`, reading the
     tokens from the state directory as its README says;
   - always in a fresh temporary folder, never a real home, and stopped afterwards.
3. **A mismatch report:** a table of every case where the two disagree, or either breaks the
   contract, saying which side is wrong per `api-v1.md`.
   - **Fix the mock hub** where it is wrong.
   - **Don't change the daemon:** list its deviations for a follow-up brief.
   - Where the contract itself is ambiguous, say so and don't guess.
4. **CI:** a new workflow that runs the suite against both. Known daemon deviations may be marked
   as expected failures, each linked to its row in the report, so the job is green while they wait
   for a fix.

## Acceptance

- The suite passes against the mock hub, apart from the contract ambiguities listed.
- Against the daemon, it passes apart from the listed and marked deviations.
- One command runs each target, and both are in the suite's README.
- fmt, clippy, `npm test`, the guards, and every CI job pass.

## Out of scope

Changing the daemon or the contract (list proposals instead), and load testing.
