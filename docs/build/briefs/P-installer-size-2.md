# Brief P · Installer size: dedupe the daemon and compress the helpers

- **Stream:** P · Packaging (with the desktop's resource lookup).
  **Branch:** `integrator/installer-size-2`.
  **Paths:**
  - `packaging/**`, `.github/workflows/release.yml`;
  - `apps/desktop/src-tauri/**` (resource lookup, helper installation and its tests);
  - `crates/remote/**` (only where helpers are found, verified and installed);
  - `docs/build/streams/P.md` (budgets, see 3) and `docs/security/threat-model.md`.
- **First read:**
  - [README.md](README.md) and the root `AGENTS.md` (or `CLAUDE.md`);
  - [0-installer-size.md](0-installer-size.md), PR #26, and `packaging/README.md` ("Proposals, not
    implemented");
  - `crates/remote/README.md` (helpers and their sha256 manifest).
- **Suggested agent:** Codex (Linux for the AppImage, deb and rpm; CI builds the DMG).

## Goal

The installers are: Windows NSIS 21.9 MB (within budget), macOS universal DMG 41.4 MB,
Linux .deb/.rpm 33.8 MB, and AppImage 100.6 MB. The budget is 25 MB. The remote helpers alone are
about 27 MB. Two of #26's proposals are worth building; the others are not.

## What to build

1. **Don't carry the native daemon twice.**
   - Where the bundle has an identical `pitcrewd` as both a sidecar and a helper, keep one, and
     change the resource lookup to find it.
   - Document the lookup and manifest contract, and test install and upgrade on all three OSes.
2. **Compress the helpers in the bundle, and verify after decoding.**
   - Store each remote helper compressed (zstd or xz).
   - On first use, decode to an owner-only temporary file: exclusive creation, a bounded decoded
     length, no symlink following.
   - Hash the **decoded executable** against the sha256 manifest compiled into the desktop, then
     install it atomically. Never trust a downloaded or adjacent manifest.
   - A version upgrade invalidates the cache. Any hash or size error fails closed.
   - Add a threat-model row.
3. **Budgets.** Measure every installer before and after.
   - **macOS DMG and .deb/.rpm:** target 25 MB. If one stays over, report by how much and what's
     left in it.
   - **AppImage:** exempt, because it carries WebKitGTK and GTK by design. Say so in P.md and give
     it its own budget at the measured size plus 10%.

## Acceptance

- The packaging tests (`packaging/test.sh`) and the desktop crate's tests pass, plus new tests for
  the decoded-hash check, a corrupt helper, an oversized decode, a symlinked cache, and upgrade
  invalidation.
- Every CI job passes, and the release workflow builds all installers (a dry run on the pull
  request is fine).

## Out of scope

Downloading the Mac helper on demand, a thinner AppImage, and code signing.
