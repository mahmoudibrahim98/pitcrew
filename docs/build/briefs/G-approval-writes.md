# Brief G · Outward writes through an approval queue

- **Stream:** G · Integrations. **Start after G-sync-wiring merges** (it builds on its routes and loop).
  **Branch:** `integrator/approval-writes`.
  **Paths:** `crates/sync-github/**`, `crates/sync-jira/**`, `crates/store/migrations/04*`;
  `crates/daemon/**`, `crates/hub-work/**`; `crates/protocol/**` (regenerate `packages/protocol-ts`);
  `apps/ui/src/**` (Inbox approvals, the task drawer); `docs/build/contracts/api-v1.md`;
  `apps/mock-hub/**`, `tests/conformance/**`; the threat model; the READMEs of what you touch.
- **First read:**
  - [README.md](README.md) and the root `AGENTS.md` (or `CLAUDE.md`);
  - `docs/build/streams/G.md` items 3 and 4, [G-sync-wiring.md](G-sync-wiring.md) and its PR;
  - the asks model in `api-v1.md` (kind `approval`) and `apps/ui/src/projects/` (Inbox).
- **Suggested agent:** an Opus-class agent.

## Goal

PitCrew can change GitHub and Jira — create an issue from a task, comment, close, label, set a
milestone or epic — but **every outward write is approved by a person first**, and each field has
one owner (G.3).

## What to build

1. **Field ownership tables** per system (which side owns title, body, status, labels, assignee,
   milestone/epic), applied both ways; a conflict on a field the hub doesn't own becomes an ask.
2. **The queue:** a hub change that implies an outward write creates an ask of kind `approval`
   showing exactly what will be sent (system, scope, operation, fields, before → after). It runs only
   after a person approves; the result (success, or the upstream error) is an event; denial records
   that nothing was sent. Idempotent on retry.
3. **The writes** in both crates (REST v3 / GitHub REST), with recorded fixtures.
4. **UI:** approvals in the Inbox with the diff; the task drawer shows pending and done writes.
5. **Tests:** "no outward write without an answered approval ask" (G's acceptance), denial,
   retry, conflicts; recorded fixtures only; conformance on both targets.

## Acceptance

- fmt, clippy with `-D warnings`, the tests of the crates touched, `cargo test -p pitcrew-protocol
  --features ts`, `npm test`, the UI's checks, both conformance targets, and the guards pass. Every
  CI job passes on the pull request.
