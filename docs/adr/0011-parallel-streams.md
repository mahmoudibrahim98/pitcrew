# 0011. Build in parallel streams with exclusive path ownership

- **Status:** Accepted
- **Date:** 2026-09-30

## Context

PitCrew is built by several agents at once, each in its own worktree. Parallel work fails in
familiar ways: two branches edit the same file, a shared manifest conflicts, one stream waits for
another, or an agent quietly changes a contract others depend on.

## Decision

- **Stream 0 comes first and is small:** the protocol and event schema, the runtime and
  source-adapter traits, fixtures, a mock hub, the store's initial migration, CI and these
  documents. Every other stream codes against those contracts.
- **One owner per path.** `docs/build/ownership.json` maps every path to exactly one stream. CI
  runs a **path guard**: a pull request from `s/<stream>/<topic>` may only touch that stream's
  paths (plus lockfiles). An **ownership audit** fails if any file has no owner or two.
- **Shared files belong to the integrator** (the Cargo and pnpm manifests, the protocol crate,
  CI, the migration index). Streams **expose** instead of editing shared files: each crate exports
  `routes()` for the API crate to compose, each UI feature exports its routes and navigation for
  the shell, and each stream owns a migration number range.
- **Mocks, not waiting.** UI streams build against `apps/mock-hub`; runner streams against fakes
  (`pitcrew-interfaces::fake`); hub streams against recorded events (`crates/fixtures`).
- **Contracts change only through a contract PR** (`s/0/contract-<topic>`), reviewed by the
  integrator, with a version bump when breaking.
- **Agents never merge.** The integrator merges. Every report ends with "What I did not do".
- A **scrub gate** blocks private data: patterns held as a CI secret (never in the repository), plus SHA-256 hashes of long, unguessable tokens. The guards themselves run from the base branch, so a pull request cannot weaken them.

## Consequences

- Up to ~6 agents work at once; the limit is review capacity.
- Milestones (M1–M6) are integration checkpoints, not phases that block work.
- The process is described in `docs/build/`.
