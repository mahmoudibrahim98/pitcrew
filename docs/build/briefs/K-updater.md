# Brief K · The desktop updater

- **Stream:** K · Desktop shell (with the release workflow from P).
  **Branch:** `integrator/updater`.
  **Paths:** `apps/desktop/src-tauri/**`, `apps/ui/src/shell/**` (the update prompt),
  `.github/workflows/release.yml`, `packaging/**`, `docs/build/contracts/desktop-gateway.md`, the
  threat model, and the READMEs of what you touch.
- **First read:**
  - [README.md](README.md) and the root `AGENTS.md` (or `CLAUDE.md`);
  - `docs/build/streams/K.md` item 6 and `docs/build/streams/P.md` (signing, release);
  - `.github/workflows/release.yml` and `packaging/README.md` (the draft release, SHA256SUMS,
    provenance, the signing placeholders).
- **Suggested agent:** Codex (local, Windows), or any coding agent.

## Goal

An installed PitCrew learns that a newer release exists and updates itself after the person
agrees, with every update signed and verified.

## What to build

1. **Tauri's updater plugin**, configured for the GitHub releases feed (`latest.json` published by
   the release workflow), checking on start and daily; never updating without the person's consent;
   pre-releases only if the person opted in.
2. **Signing:** the update artefacts are signed with the updater's key (minisign, as Tauri uses).
   The **private key is a repository secret** read by the release workflow and skipped when absent
   (like the existing signing placeholders); the **public key** is compiled into the app. Document in
   `packaging/README.md` how the maintainer generates the key pair and adds the secret. Never commit a
   private key.
3. **The release workflow** writes `latest.json` and the signatures next to the installers when the
   key is present; a run without the key still builds everything and says updates are unsigned and
   disabled.
4. **UI:** an unobtrusive "update available" prompt with the release notes link; Settings → check
   now, opt into pre-releases.
5. **Tests:** the feed parsing and the version comparison; an update whose signature doesn't verify
   is refused (with a test key generated in the test, never a real one); a threat-model row.

## Acceptance

- The desktop crate's fmt, clippy and tests, the UI's checks, the packaging tests, the release
  workflow's manual run (no publish), and the guards pass. Every CI job passes on the pull request.

## Out of scope

Code signing of the installers themselves (needs the maintainer's certificates).
