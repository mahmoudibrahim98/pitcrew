# Brief 0 · The "+ New" dialogs: project, workstream, agent, team

- **Stream:** 0 · Composition root (work model, shell UI).
  **Branch:** `integrator/create-dialogs`.
  **Paths:** `apps/ui/src/shell/**`, `apps/ui/src/projects/**`, `apps/ui/src/data/**`,
  `apps/ui/src/design/**` (only to add a missing primitive), `crates/hub-work/**`,
  `crates/daemon/**` (wiring only), `crates/protocol/**` (regenerate `packages/protocol-ts`),
  `docs/build/contracts/api-v1.md`, `apps/mock-hub/**`, `tests/conformance/**`, the threat model,
  `Cargo.lock`, and the READMEs of what you touch. Mechanical edits elsewhere (an import, a match
  arm, a generated binding) are fine; say which in the report.
- **First read:**
  - [README.md](README.md) and the root `AGENTS.md` (or `CLAUDE.md`);
  - `apps/ui/src/shell/core.tsx` (the placeholder `CREATE` entries) and
    `apps/ui/src/projects/index.ts` (the real "Task" entry, which replaces the shell's by id);
  - `apps/ui/src/shell/README.md` (how a feature's `create` entry replaces the shell's);
  - `docs/build/contracts/api-v1.md` (`POST /v1/projects`, `POST /v1/workstreams`,
    `GET /v1/personas`, `GET /v1/teams`) and `crates/protocol/src/events.rs` (`persona_saved`,
    `team_saved`, `project_created`, `workstream_created`);
  - `crates/hub-work/src/projection/directory.rs` and `seed.rs` (personas and teams exist, read
    only).
- **Suggested agent:** Codex.

## Goal

"+ New" (and the palette) creates every kind of thing it lists. Today Task is real; **Project,
Agent and Team** open a dialog that says "not available yet". This is the gap people hit first
when they try the app on their own work.

## What to build

1. **New project:** name, key (suggested from the name, editable, `409` shown inline), root
   location (a machine from `GET /v1/machines` and a path; validate it is absolute for that
   machine's platform). `POST /v1/projects`, then navigate to the project. An optional first
   workstream in the same dialog (`POST /v1/workstreams`).
2. **New workstream** inside a project: an entry on the project page and in "+ New" when a project
   is in context. `POST /v1/workstreams`.
3. **New agent (persona):** the hub has `persona_saved` events and `GET /v1/personas` but no write
   route. Add `POST /v1/personas` (create) and `PUT /v1/personas/{id}` (edit), validated, emitting
   `persona_saved`, with the same authorization as creating a project. The dialog edits the
   persona's existing fields (read `Persona` in `crates/protocol`; add none unless a field is
   needed and say why).
4. **New team:** the same for teams: `POST /v1/teams` and `PUT /v1/teams/{id}`, emitting
   `team_saved`, members chosen from people and personas that exist (unknown ids refused).
5. Replace the shell's placeholder entries by id from the features that own them, and remove the
   placeholder dialog if nothing uses it. Keep the keyboard and focus behaviour the shell's tests
   expect.
6. **Mock hub** parity for every new route, and the contract doc.

## Acceptance

- Each dialog creates its thing against the real hub and the mock hub, and what's created appears
  without a reload (invalidation).
- Validation errors and conflicts are shown inline; nothing is half-created.
- fmt, clippy with `-D warnings`, the tests of the crates touched, `cargo test -p pitcrew-protocol
  --features ts`, `npm test`, the UI's checks (typecheck, lint, unit, e2e), both conformance
  targets, and the guards pass. Every CI job passes on the pull request.
- Merge `origin/main` just before opening the pull request: onboarding (#55, #56) and sync (#52)
  touch the same routes and are merging now.
