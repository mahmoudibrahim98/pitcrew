# Brief 0 · Security checks in CI: CodeQL, zizmor and a CSP check

- **Stream:** 0 · Contracts (CI; stream Q proposed these). **Branch:** `integrator/ci-security`.
  **Paths:**
  - new workflow files under `.github/workflows/`;
  - fixes to the existing `ci.yml`, `fuzz.yml` and `release.yml` for what zizmor finds;
  - `scripts/ci/**` (a new check and its tests);
  - `docs/security/threat-model.md`, a row for each new control.
- **First read:**
  - [README.md](README.md) and the root `AGENTS.md` (or `CLAUDE.md`);
  - `docs/build/streams/Q.md` (item 4);
  - `.github/workflows/*.yml`, `scripts/ci/README.md` if present, and the CSP in
    `apps/desktop/src-tauri/tauri.conf.json`.
- **Suggested agent:** Codex, or any coding agent.
- **Overlaps:** `0-installer-size` may also edit `release.yml`; keep your edits there to what zizmor
  needs.

## Goal

Three checks the security stream proposed and nobody added. The repository is public, so its
workflows are attack surface too.

## What to build

1. **CodeQL:** a `codeql.yml` workflow on pull requests and pushes to `main`, plus a weekly schedule.
   - Cover `actions` and `javascript-typescript`, and `rust` if CodeQL's released support covers it;
     check, and say which.
   - Give it minimal permissions (`security-events: write`, `contents: read`) and actions pinned by
     commit SHA, as the other workflows do.
   - It must not need any secret.
2. **zizmor:** run it on `.github/workflows/` in a new workflow, or a job in one, pinned to an exact
   version.
   - Fix what it finds in the existing workflows: template injection, over-broad permissions,
     credentials persisted by checkout, unpinned actions, and so on.
   - Where a finding is a false positive, suppress it inline with a comment saying why.
   - `release.yml`'s signing and publishing steps must keep working: say what you changed there
     line by line.
3. **A CSP check** (`scripts/ci/csp-check.mjs`, with tests run by the existing Node tests job):
   - **The policy:** fail if the desktop's CSP in `tauri.conf.json` allows `'unsafe-inline'` or
     `'unsafe-eval'` in `script-src`, or any remote origin in `script-src`, `connect-src` or
     `default-src`.
   - **The built UI:** fail if `apps/ui/dist/index.html` contains inline `<script>` or event-handler
     attributes.
   - Run it in the UI job after the build, or in its own job that builds the UI.
4. **The threat model:** add each control as a row, linking the workflow or script.

## Acceptance

- The new workflows run green on the pull request, and the existing jobs stay green.
- zizmor reports no unsuppressed findings, and every suppression has a reason.
- The CSP check's tests show it catching each forbidden case.

## Out of scope

Changing the CSP itself (report anything wrong in it), Dependabot settings, and secrets.
