# Brief 0 · Follow-ups from the onboarding scan's review (#32)

- **Stream:** 0 · Contracts (daemon route, ingest options, conformance and CI).
  **Branch:** `integrator/scan-followups`.
  **Paths:**
  - `crates/daemon/src/scan.rs`, `crates/ingest/src/scan.rs` (and the ingest README);
  - `tests/conformance/**`, `docs/build/contracts/api-v1.md`;
  - `apps/mock-hub/**`, only to align host info;
  - `.github/workflows/ci.yml` (one step).
- **First read:**
  - [README.md](README.md) and the root `AGENTS.md` (or `CLAUDE.md`);
  - [0-onboarding-scan.md](0-onboarding-scan.md);
  - the integrator's review on PR #32 (`#issuecomment-5969543690`);
  - `crates/ingest/README.md`.
- **Suggested agent:** Codex, or any coding agent.

## What to fix

1. **A scan must end when nobody waits for it.**
   - **The problem:** the walk can't be cancelled and has no time limit, while the desktop gateway gives up on a request after 120 s. A huge home, or a session folder on a hung network share, leaves every retry at 409 until the walk ends.
   - **Fix:** add a cancel flag and a time budget to `ScanOptions` (ingest), checked between files. The daemon cancels when the client disconnects, and gives up after a budget (say 10 minutes), answering with what it has, marked partial.
   - **Tests:** a cancelled scan, and a budget that runs out.
2. **A client that stops reading must not hold the scan.** `crates/daemon/src/scan.rs` ~258 and ~284 use `blocking_send` for the last tick and the final frame. Give both a timeout of about 30 s, then drop the stream and release the claim.
3. **Conformance is stricter than ingest.** `tests/conformance/scan.test.mjs` ~65-66 asserts that `by_folder` and `by_month` each add up to `sessions`, but ingest leaves out sessions without a folder or a start time. Use `<=`, and say in api-v1 that such sessions are left out of those lists.
4. **Host info.** The mock lists a `scan` capability in `/v1/host/info` and the daemon doesn't. Make them agree, the daemon advertising it being the likely fix, and cover it in conformance.
5. **Run the fresh-config end-to-end tests in CI.** Add `playwright test --config e2e/fresh.config.ts` to the UI job, so the first-run test (Welcome → Workspace → Scan → Create → Done) runs on every PR.
6. **No paths in ingest's warnings.** `crates/ingest/src/scan.rs` ~301 and `opencode/mod.rs` ~389 can write transcript or store paths to the daemon log. Log a count and a reason instead, or a path hashed per run.

## Acceptance

- fmt, clippy with `-D warnings`, the tests of the crates touched, `npm test`, the conformance suite against both targets, and the guards pass. Every CI job passes on the pull request, including the new fresh-config step.

## Out of scope

Scanning remote machines, and import filters.
