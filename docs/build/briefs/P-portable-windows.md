# Brief P · A portable Windows build: one zip, no installer

- **Stream:** P · Packaging.
  **Branch:** `integrator/portable-windows`.
  **Paths:** `.github/workflows/**` (a new workflow or a job in `release.yml`), `packaging/**`,
  `apps/desktop/src-tauri/**` (only what portable mode needs: finding the helpers next to the
  executable, and the updater's portable behaviour), `docs/build/contracts/desktop-gateway.md` if
  the helper lookup changes, and the READMEs of what you touch. Mechanical edits outside the paths
  are fine; list them. If one item needs a substantive change outside the paths, skip that item,
  list it under Not done, and finish the rest. Never stop the whole brief.
- **First read:**
  - [README.md](README.md) and the root `AGENTS.md` (or `CLAUDE.md`);
  - `.github/workflows/release.yml` (the Windows build job and how it bundles NSIS) and
    `packaging/README.md`;
  - [P-desktop-bundle.md](P-desktop-bundle.md) and [K-updater.md](K-updater.md);
  - how the desktop finds `pitcrewd`, `pitcrew-ptyd`, `pitcrew-askpass` and `pitcrew` at run time
    (`apps/desktop/src-tauri/src/daemon/`, `tauri.conf.json` `externalBin`).
- **Suggested agent:** an Opus-class agent. Windows is checked only by CI: read the Windows jobs'
  logs when they fail.

## Goal

The maintainer's laptop can't run installers and can't build locally. They need a zip they can
unpack into a folder they are allowed to run programs from, and start PitCrew from there.

## What to build

1. **A CI job that uploads `pitcrew-windows-x64-portable.zip`.**
   - It runs on demand (`workflow_dispatch`) and on every push to `main`. Pull requests don't run
     it unless they touch packaging or the desktop shell.
   - It reuses the release workflow's Windows build: the same pinned toolchain, actions pinned by
     commit SHA, `permissions: {}` at the top and only `contents: read` on the build job, and no
     caches. Zizmor and the workflow-security checks must pass.
   - The zip holds `pitcrew-desktop.exe`, `pitcrewd.exe`, `pitcrew-ptyd.exe`,
     `pitcrew-askpass.exe`, `pitcrew.exe`, `LICENSE`, the third-party notices, a short
     `README-portable.txt` and a `SHA256SUMS` file.
   - It is uploaded as a workflow artifact with a 30-day retention. Nothing is published as a
     release.
2. **The desktop runs from the unzipped folder.**
   - It finds the helpers next to its own executable when they aren't in the installed layout.
   - It needs no registry keys, no elevation and no installer. WebView2 is assumed present
     (Windows 10 and 11 ship it). If it is missing, the app says so in plain words and links the
     Evergreen bootstrapper.
   - State stays where the installed app keeps it, so moving between the zip and an installer
     later keeps the person's data.
3. **The updater in portable mode.** A portable build never runs an installer. When an update
   exists, it shows the version and a link to the newest portable artifact or release page instead
   of installing. Portable mode is detected by a marker file `portable.txt` next to the executable,
   which the zip ships.
4. **A smoke test in the job, on the Windows runner.**
   - Unzip into a fresh folder outside the checkout.
   - Run `pitcrewd.exe --version`, `pitcrew.exe --version` and `pitcrew-ptyd.exe --version`.
   - Start `pitcrewd.exe` with a temporary state directory and check it answers `GET /v1/health`
     (or the equivalent readiness route) on its pipe or port.
   - Check the desktop executable's helper lookup with a unit test, or a `--check-layout` flag if
     the shell has one; don't launch the GUI.
5. **Docs:** `packaging/README.md` says how to get the zip (Actions → the run → Artifacts), how to
   check `SHA256SUMS`, and what portable mode changes.

## Acceptance

- A `workflow_dispatch` run on the PR branch produces the zip, and its smoke test passes. Link the
  run in the PR body.
- The workflow-security job (zizmor) passes. Every CI job passes on the pull request.
- fmt, clippy with `-D warnings` and the desktop shell's tests pass for anything changed in
  `apps/desktop/src-tauri`.
