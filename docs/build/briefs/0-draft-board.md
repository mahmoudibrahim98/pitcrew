# Brief 0 · Draft the board from history

- **Stream:** 0 · Composition root (dispatch, work model, onboarding UI).
  **Branch:** `integrator/draft-board`.
  **Paths:** `crates/daemon/**`, `crates/hub-work/**`, `crates/office/**` (prompts as files),
  `crates/protocol/**` (regenerate `packages/protocol-ts`), `apps/ui/src/onboarding/**` and
  `apps/ui/src/projects/**`, `docs/build/contracts/api-v1.md`, `apps/mock-hub/**`,
  `tests/conformance/**`, the threat model, and the READMEs of what you touch.
- **First read:**
  - [README.md](README.md) and the root `AGENTS.md` (or `CLAUDE.md`);
  - `docs/build/streams/O.md` item 1 ("optional 'draft the board from history' (cost shown first)")
    and `docs/build/streams/F.md` (prompts as files, briefs, proposals);
  - [0-dispatch.md](0-dispatch.md) and PR #33 (starting an agent's CLI from a task);
  - `docs/adr/0010-real-cli-sessions.md`.
- **Suggested agent:** an Opus-class agent, or Codex.

## Goal

After onboarding, a person can ask PitCrew to **draft** each workstream's board — open tasks,
what's in progress, what looks done — from the sessions already linked to it. Nothing is created
until the person accepts it.

## What to build

1. **No API keys:** the draft is written by an agent the person already uses — a dispatched agent
   CLI session (as dispatch does) given a versioned prompt from `crates/office/prompts/` and a
   bounded, redacted summary of the workstream's sessions (titles, recaps, recent activity; never
   whole transcripts, never secrets). It returns the proposal through the `pitcrew` CLI.
2. **Cost first:** before starting, show what will be sent (sizes, number of sessions) and an
   estimate of the agent's usage; the person confirms per workstream.
3. **The proposal** is stored as a proposal (like "where it stands" proposals): tasks with titles,
   states and links to their evidence sessions. The person accepts all, some, or none; accepted ones
   become tasks through the normal commands, marked as drafted.
4. **UI:** the optional step in the wizard and a "Draft board" action on a workstream; the review
   screen with accept/reject per task.
5. **Tests:** the summary's bounds and redaction; nothing is created without acceptance; a fake
   agent returns a proposal end to end; conformance for the routes.

## Acceptance

- fmt, clippy with `-D warnings`, the tests of the crates touched, `cargo test -p pitcrew-protocol
  --features ts`, `npm test`, the UI's checks, both conformance targets, and the guards pass. Every
  CI job passes on the pull request.
