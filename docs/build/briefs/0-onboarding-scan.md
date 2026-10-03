# Brief 0 · Onboarding: scan a machine, then create projects and workstreams from it

- **Stream:** 0 · Contracts (the scan contract with streams A and O, and the daemon's route).
  **Branch:** `integrator/onboarding-scan`.
  **Paths:**
  - `docs/build/contracts/api-v1.md` (and `docs/build/contracts.md` row 60);
  - `crates/protocol/**` (a `scan` module);
  - `crates/ingest/src/scan.rs`, only to move types into the protocol;
  - `crates/daemon/**`;
  - `apps/mock-hub/**`, `tests/conformance/**`;
  - `apps/ui/src/onboarding/**` (`hub-api.ts`, the scan and create steps, `steps.ts`);
  - the READMEs.
- **Can run alongside `0-dispatch`.** Keep the daemon changes in a new module and one line of route
  wiring, so the two merge cleanly.
- **First read:**
  - [README.md](README.md) and the root `CLAUDE.md` (or `AGENTS.md`);
  - `docs/build/streams/O.md` items 1, 2 and 4, and `0.md` "Next contract work";
  - `crates/ingest/README.md` (~87-97, moving the scan types into the protocol) and
    `crates/ingest/src/scan.rs`;
  - `apps/ui/src/onboarding/api.ts` (`OnboardingApi`, the types ~120-215), `hub-api.ts` (`NOT_YET`),
    `fake-api.ts` and the onboarding README (~112-141, the proposed shapes);
  - `api-v1.md` (setup, machines, projects, workstreams).

## Goal

After setup, the real first run skips straight to Done: every step after "Workspace" is "not yet" in
`hub-api.ts`. Make the next two steps real:
- **scan** this machine's agent homes, showing progress, counts and suggested projects;
- **create** the chosen projects and workstreams from the suggestions, with their folders and
  branches as locations, so the linking work can file sessions into them.

## What to build

1. **The contract first:**
   - a scan route on the hub, e.g. `POST /v1/machines/{id}/scan` (person-only), with progress over
     `/v1/stream` or a streamed response; choose one, and say why;
   - the result types in a new `pitcrew_protocol::scan` module, moved from `crates/ingest/src/scan.rs`
     (`ScanCounts`, `Suggestion`, `WorkstreamSuggestion`, `ScanReport`, `ScanProgress`);
   - JSON field names follow the API's existing convention (check `api-v1.md`). The UI adapts in
     `hub-api.ts`; the UI's own types (camelCase, `byEngine` as a record) stay UI-side.
   - **Only the local machine** in this brief. A remote machine answers 501 or 409 with a clear
     reason, and is a later brief.
   - **Privacy:** the scan reads the machine's real agent homes. That's the product's job, but the
     result goes only to the person (never an agent token), and tests use temporary homes only
     (`pitcrew_fixtures::homes`).
2. **The daemon route:** runs `pitcrew_ingest::scan` over `default_homes()` (as the runner already
   resolves them) on the blocking pool, streams progress, and returns the report. Only one scan at a
   time per machine: a second one gets 409.
3. **The mock hub:** the same route against fixture homes, so the UI can be developed against it.
4. **The UI** (`apps/ui/src/onboarding/hub-api.ts`):
   - **`streamScan`:** implement it against the route.
   - **`createFromScan`:** build it on `POST /v1/projects` and `POST /v1/workstreams`.
     - `ProjectSelection` has no path, branch or project key, so look the suggestion up by
       `suggestionId` from the scan result.
     - The project key follows `ProjectKey`'s rules, unique in the workspace; say how it's derived.
     - The root and workstream locations come from the suggestion's path and branch.
   - Remove those two calls from `NOT_YET`, so `stepsFor` shows the scan and create steps.
   - Import, hooks and safety stay "not yet".
5. **Conformance:** add the scan route's cases (auth, 409 for a concurrent scan, the result's shape)
   against both targets.

## Acceptance

- **Daemon tests** over temporary synthetic homes (Claude, Codex and OpenCode fixtures): progress
  arrives, and the counts and suggestions match the fixtures; a concurrent scan gets 409; an agent
  token gets 403.
- **UI:** unit tests for the mapping and the project-key rule, and an end-to-end first run against
  the mock hub, through Welcome → Workspace → Scan → Create → Done, with an axe check in both
  themes.
- fmt, clippy with `-D warnings`, the workspace tests (in the background), the UI's checks,
  `npm test`, the conformance suite, the guards, and every CI job pass.

## Out of scope

Importing sessions with filters (the runner indexes everything today; a later brief), "draft the
board from history", hooks and safety over HTTP, scanning remote machines, and the legacy importer.
