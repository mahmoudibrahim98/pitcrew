# Brief P · Installers that carry everything the app needs

- **Stream:** P · Packaging and release (integration work: it also touches the desktop's bundle settings).
  **Branch:** `integrator/desktop-bundle`. **Paths:** `packaging/**`,
  `.github/workflows/release.yml`, and the desktop's bundle settings (`apps/desktop/src-tauri/tauri.conf.json`,
  its `build.rs` if needed). Changing more of `apps/desktop` needs a note in your report.
- **First read:** [README.md](README.md), the root `CLAUDE.md`, `docs/build/streams/P.md`,
  `packaging/` and `.github/workflows/release.yml`, and these READMEs:
  - `apps/desktop/src-tauri` ("Remote workspaces": helpers, `manifest.json`,
    `PITCREW_HELPERS_MANIFEST`, askpass);
  - `crates/remote` (`Platform::artefact()` names, `pitcrew-askpass`);
  - `crates/ptyd` (what to bundle).

## Goal

A release produces desktop installers for Windows, macOS and Linux that work out of the box:
- the local daemon;
- the terminal supervisor;
- the SSH askpass helper;
- the Linux helpers it deploys to remote machines, with their checksums compiled into the app;
- `pitcrew://` links and notifications registered.

## What to build

1. **The desktop bundle per OS** in the release workflow (Tauri's bundler: NSIS or MSI on Windows, a
   DMG on macOS, an AppImage and a `.deb` on Linux), built from the same commit as the helper
   binaries the workflow already builds.
2. **What goes inside:**
   - **next to the desktop executable:** `pitcrewd`, `pitcrew-ptyd` and `pitcrew-askpass` for that
     OS (`.exe` on Windows; `Contents/MacOS` on macOS);
   - **a `helpers/` folder** with the remote helpers under `Platform::artefact()` names
     (`pitcrewd-x86_64-unknown-linux-musl`, `pitcrewd-aarch64-unknown-linux-musl`,
     `pitcrewd-universal-apple-darwin`) and a `manifest.json`;
   - **`PITCREW_HELPERS_MANIFEST`** (`{version, sha256}`) set when the desktop is compiled, so a
     release build trusts only compiled checksums;
   - the files' modes and owners must pass the desktop's own trust checks (`check_trusted`).
3. **Registrations:**
   - the `pitcrew://` scheme (already configured for the deep-link plugin) is registered by each
     installer;
   - on Windows, an AppUserModelID for notifications (the desktop's notification code may need the
     id; say so if it does);
   - check how each one is verified, and document it.
4. **Checks in the workflow:**
   - each installer's size (budget ≤ 25 MB, report if over);
   - the bundled helpers' sha256 equal the manifest;
   - the desktop executable runs `--version`, or an equivalent smoke check, where a display isn't
     needed.
5. **Signing:** leave hooks for code signing (Windows Authenticode, macOS notarisation), off without
   secrets, and document what the maintainer must provide.

## Acceptance

- A manual run of the release workflow on your branch (`workflow_dispatch`), or a dry run of
  `packaging/` scripts in the VM for the Linux bundle, produces the installers with everything above.
  Report what you could and couldn't build from the VM. Windows and macOS bundles build on CI's
  runners; push and run the workflow on your branch if it allows manual runs.
- fmt, clippy, the guards, and the workflow's own lint (e.g. `actionlint`, if available) pass.

## Out of scope

Signing certificates, auto-update, and changes to the app's behaviour.
