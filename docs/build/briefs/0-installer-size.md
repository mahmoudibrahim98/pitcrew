# Brief 0 · Smaller installers, and an .rpm

- **Stream:** 0 · Contracts (packaging plus the release profile in the root `Cargo.toml`).
  **Branch:** `integrator/installer-size`.
  **Paths:**
  - `packaging/**`;
  - `.github/workflows/release.yml`;
  - `apps/desktop/src-tauri/tauri.conf.json` (bundle settings);
  - the `[profile.*]` sections of the root `Cargo.toml` and `apps/desktop/src-tauri/Cargo.toml`.
- **First read:**
  - [README.md](README.md) and the root `AGENTS.md` (or `CLAUDE.md`);
  - `packaging/README.md` ("The desktop installers", sizes);
  - `docs/build/streams/P.md` (budgets: installer ≤ 25 MB);
  - `.github/workflows/release.yml`;
  - `crates/remote`'s README (helpers and their sha256 manifest).
- **Suggested agent:** Codex, or any coding agent.
- **Overlaps:** `0-ci-security` may edit `release.yml` for zizmor findings.

## Goal

The first full release build gave these installer sizes (budget 25 MB):

| Installer | Size |
|---|---|
| Windows NSIS | 21.9 MB |
| macOS universal DMG | 41.4 MB |
| Linux `.deb` | 33.8 MB |
| Linux AppImage | 100.6 MB |

Find out what takes the space, take the safe reductions, add an `.rpm`, and propose the rest.

## What to do

1. **Measure:** for each installer, list its biggest files with sizes, from Tauri's bundle output,
   or by unpacking a local Linux build.
   - The remote helpers in `helpers/` alone are about 27 MB: musl x86_64 7.2 MB, aarch64 6.8 MB, and
     the universal macOS build 13.5 MB.
   - The AppImage also carries WebKitGTK and GTK.
2. **Safe reductions:**
   - release-profile settings (`opt-level`, `lto`, `codegen-units`, `panic`, `strip`) for the sidecars
     and helpers. `panic = "abort"` only if nothing relies on unwinding: check for `catch_unwind`, and
     the hook's "always exits 0 even if it panics" guarantee in `crates/cli`;
   - Tauri bundle options that drop what the app doesn't use (e.g. the AppImage's media framework);
   - report each change's effect on each installer.
3. **An `.rpm`:**
   - add Tauri's rpm bundle to the Linux job;
   - give it the same checks the `.deb` gets in `packaging/desktop/check.sh`: owners and modes, the
     helpers' sha256 against the manifest, the desktop entry, sidecars that run;
   - extend `packaging/test.sh` where it can test without a real rpm.
4. **Propose, don't build:** the bigger options with their trade-offs, for example:
   - compressing the helpers in the bundle (the sha256 manifest and the desktop's trust checks must
     stay sound: say how);
   - downloading the macOS helper only when a Mac is added;
   - a thinner AppImage.

   These change the trust chain or the product, so write them up in `packaging/README.md` under
   "Size", and don't implement them.

## Acceptance

- The Linux bundle builds in the environment, and `packaging/desktop/check.sh` passes on the `.deb`,
  the AppImage and the new `.rpm`. Report the sizes before and after.
- macOS and Windows are built by the integrator, who runs the release workflow on your branch. Say
  what you expect to change there.
- `packaging/test.sh`, fmt, clippy, the guards, and every CI job pass.

## Out of scope

Signing, the update feed, and the trust-chain changes in item 4.
