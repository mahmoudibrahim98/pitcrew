# Packaging

Release builds of `pitcrewd` (the daemon, which is also the remote helper), `pitcrew` (the agent
CLI and hook entry point), `pitcrew-ptyd` (the terminal supervisor) and `pitcrew-askpass` (ssh's
prompt helper), the desktop installers that carry them, with checksums, SBOMs and signing hooks.
**Owned by stream P.**

| Script | What it does |
|---|---|
| [`build-release.sh`](build-release.sh) | Builds the four binaries for one or more targets into `dist/` and writes `SHA256SUMS`. |
| [`desktop/build.sh`](desktop/build.sh) | Builds one OS's desktop installers with Tauri, from `build-release.sh`'s binaries (see [The desktop installers](#the-desktop-installers)). |
| [`desktop/check.sh`](desktop/check.sh) | Checks those installers: size, layout, the helpers' checksums, owners and modes, `pitcrew://`. On Windows it runs [`desktop/check-windows.ps1`](desktop/check-windows.ps1), which installs, upgrades and removes. |
| [`desktop/smoke.sh`](desktop/smoke.sh) | Starts the installed app once in a throwaway home and checks it used its own `pitcrewd` and `pitcrew-askpass` (Linux, macOS). |
| [`portable/build.sh`](portable/build.sh) | Builds the portable Windows zip, `pitcrew-windows-x64-portable.zip` (see [The portable Windows zip](#the-portable-windows-zip)). |
| [`portable/smoke.ps1`](portable/smoke.ps1) | Unzips it into a fresh folder on Windows and checks it there, without opening a window. |
| [`notices.mjs`](notices.mjs) | Writes `THIRD-PARTY-NOTICES.txt`: every crate the programs link and every JavaScript package the window bundles, with their licence texts. |
| [`sha256sums.sh`](sha256sums.sh) | Writes `DIR/SHA256SUMS` over every file in `DIR`. |
| [`verify.sh`](verify.sh) | Checks `DIR` against its `SHA256SUMS`: every hash matches, and no file is unlisted. |
| [`sbom.sh`](sbom.sh) | Writes CycloneDX SBOMs: `pitcrewd`, `pitcrew`, `pitcrew-ptyd`, `pitcrew-askpass` and `pitcrew-desktop` (`.cdx.json`). |
| [`sign.sh`](sign.sh) | Signing placeholders: skipped when none of a kind's secrets is set, failing when any is. |
| [`test.sh`](test.sh) | Tests `sha256sums.sh`, `verify.sh`, `sign.sh`, the desktop's staging, `desktop/check.sh` on a stand-in `.deb`, `portable/build.sh` and `notices.mjs` (bash and Node only, no Rust). |
| [`zig-requirements.txt`](zig-requirements.txt) | Zig from PyPI for `cargo-zigbuild`, pinned by version and wheel hash. |
| [`../.github/workflows/release.yml`](../.github/workflows/release.yml) | Runs all of the above on a `v*` tag. |
| [`../.github/workflows/release-portable.yml`](../.github/workflows/release-portable.yml) | Builds, checks and uploads the portable Windows zip: on demand, on every push to `main`, and on pull requests that touch packaging or the desktop. |

## The static helper

The desktop uploads `pitcrewd` to remote machines over SSH and checks its sha256 against a value
compiled into the desktop (ADR-0009). Those machines may have an old glibc, no compiler, no root
and no network, so the Linux helper is a **static musl binary**. It is cross-compiled with
[`cargo-zigbuild`](https://github.com/rust-cross/cargo-zigbuild), which uses Zig as the C
compiler and linker (the SQLite in `rusqlite` is C), for both architectures from one host, with
no root and no containers:

```bash
rustup target add x86_64-unknown-linux-musl aarch64-unknown-linux-musl
# Zig, as a Python package: no root. Pinned by hash; or plain `pip install --user ziglang`.
python3 -m pip install --user --require-hashes -r packaging/zig-requirements.txt
cargo install cargo-zigbuild --locked
packaging/build-release.sh --zig x86_64-unknown-linux-musl aarch64-unknown-linux-musl
```

The result is `dist/<bin>-<target>` for `pitcrewd`, `pitcrew`, `pitcrew-ptyd` and
`pitcrew-askpass`, plus `dist/SHA256SUMS`. The Linux desktop ships the x86_64 ones as its own
`pitcrewd`, `pitcrew-ptyd` and `pitcrew-askpass`: one static build for the helper and the app.

[`cross`](https://github.com/cross-rs/cross) is the alternative (`cross build --release --target
aarch64-unknown-linux-musl -p pitcrew-daemon -p pitcrew-cli`); it runs the build in a container,
so it needs Docker or Podman. The release workflow uses `cargo-zigbuild`.

### Tried on the stream's machine

2026-09-30, WSL2 Ubuntu 22.04 on x86_64, Rust 1.94.0, no sudo: the targets installed with
`rustup`, Zig 0.16.0 with `pip install --user ziglang`, and `cargo-zigbuild` 0.23.4 with
`cargo install --locked` (about 2 minutes). Then:

- `packaging/build-release.sh --zig x86_64-unknown-linux-musl aarch64-unknown-linux-musl`
  built all four binaries in 221 s (two build jobs, a busy machine). Each is about 350 KB and
  `file` reports `statically linked`; `ldd` says `not a dynamic executable`;
  `sha256sum --check SHA256SUMS` passes; the x86_64 binaries run.
- `cargo zigbuild --release --target <musl target> -p pitcrew-store --tests` compiled the
  bundled C SQLite for both musl targets, and the static x86_64 `event_log` test binary passed
  its 14 tests. So the helper keeps building once `pitcrewd` links the store.

Not tried here: running the aarch64 binaries (no emulator), and the old-glibc container (the
release workflow runs the x86_64 helper in `centos:7` with `--network none`).

## Other platforms

| Target | Where | Command |
|---|---|---|
| `universal-apple-darwin` | macOS (needs `lipo`) | `packaging/build-release.sh universal-apple-darwin` |
| `x86_64-pc-windows-msvc` | Windows, Git Bash | `packaging/build-release.sh x86_64-pc-windows-msvc` |
| any host triple | anywhere | `packaging/build-release.sh <triple>` |

`universal-apple-darwin` builds `aarch64-apple-darwin` and `x86_64-apple-darwin` and joins each
binary with `lipo`. Windows binaries keep their `.exe`.

## The desktop installers

[`desktop/build.sh`](desktop/build.sh) builds one OS's installers with Tauri's bundler, from
`build-release.sh`'s binaries of the same commit, so that the app works out of the box: it starts
its own daemon, has terminals where tmux is missing, asks for SSH passwords in its window, and
can deploy a helper to a remote machine.

| OS | Installers | Next to the app's executable | The helpers |
|---|---|---|---|
| Linux x86_64 | `PitCrew_<version>_amd64.deb`, `PitCrew-<version>-1.x86_64.rpm`, `PitCrew_<version>_amd64.AppImage` | `/usr/bin/` (in the AppImage `usr/bin/`): `pitcrew-desktop`, `pitcrewd`, `pitcrew-ptyd`, `pitcrew-askpass`, the static x86_64 musl builds | `/usr/lib/PitCrew/helpers/` |
| macOS, universal | `PitCrew_<version>_universal.dmg` | `PitCrew.app/Contents/MacOS/` | `PitCrew.app/Contents/Resources/helpers/` |
| Windows x86_64 | `PitCrew_<version>_x64-setup.exe` (NSIS, for the current user, no administrator) | `%LOCALAPPDATA%\PitCrew\` (`.exe`) | `%LOCALAPPDATA%\PitCrew\helpers\` |

`helpers/` is where the app looks (`resource_dir()/helpers`, see the desktop's README, "Remote
workspaces"): `pitcrewd-x86_64-unknown-linux-musl`, `pitcrewd-aarch64-unknown-linux-musl` and
`pitcrewd-universal-apple-darwin` (`pitcrew_remote::Platform::artefact()`), and `manifest.json`:

```json
{"version":"0.0.0","sha256":{"pitcrewd-aarch64-unknown-linux-musl":"…","pitcrewd-universal-apple-darwin":"…","pitcrewd-x86_64-unknown-linux-musl":"…"}}
```

`version` is `pitcrewd --version`'s second word. **The same bytes are compiled into the app** as
`PITCREW_HELPERS_MANIFEST` (ADR-0009): a release build trusts only those, never the file, which is
there for debug builds and for people to read.

### How it is built

```bash
packaging/build-release.sh --zig x86_64-unknown-linux-musl aarch64-unknown-linux-musl  # Linux
packaging/build-release.sh universal-apple-darwin                                       # on macOS
corepack pnpm install --frozen-lockfile && corepack pnpm --filter @pitcrew/ui build
cargo install tauri-cli --locked --version 2.12.1
packaging/desktop/build.sh x86_64-unknown-linux-gnu    # or universal-apple-darwin, x86_64-pc-windows-msvc
packaging/desktop/check.sh --manifest dist/desktop-stage/helpers/manifest.json dist/desktop/*
packaging/desktop/smoke.sh dist/desktop/*.AppImage     # starts the app in a throwaway home
```

`desktop/build.sh`:

1. **Stages** the inputs in `dist/desktop-stage/`, under `umask 022` and with explicit modes (the
   programs 0755, the helpers and the manifest 0644), whatever the runner's umask:
   `bin/<name>-<target>` for Tauri's `externalBin`, which installs each next to the app without
   the target; `helpers/` with the three helpers.
2. **Writes the manifest** from the staged helpers' sha256 and the staged `pitcrewd --version`.
3. **Writes `tauri.bundle.json`**, merged over `apps/desktop/src-tauri/tauri.conf.json` by
   `tauri build --config`: `externalBin`, `resources` (`helpers/`), the `.desktop` template and
   the NSIS hooks. These are not in `tauri.conf.json` because `tauri-build` checks on every
   `cargo build` that the sidecars and resources exist, so the desktop's CI job would fail
   without them. `tauri.conf.json` keeps the rest: targets, publisher, licence, category,
   descriptions, `openssh-client` recommended by the `.deb` and `openssh-clients` by the RPM,
   a per-user NSIS installer, and
   `bundle.active: false`, so a plain `tauri build` makes no installer without the helpers.
4. Runs `cargo tauri build --target <target> --features custom-protocol --bundles <…> --config …`
   with `PITCREW_HELPERS_MANIFEST` set, and copies the installers to `dist/desktop/`. The app's
   own `custom-protocol` feature is named because a release build refuses to compile without it
   (`src/app.rs`), and Tauri 2's CLI turns on only `tauri/custom-protocol`.

Paths in `tauri.bundle.json` are relative to `apps/desktop/src-tauri`, so the stage must be inside
the repository (Tauri drops a Windows drive letter from an absolute path).

### What the app's trust check needs

The app refuses a `pitcrewd`, `pitcrew-askpass` or helper that someone else could have planted
(`locate::check_trusted`: on Unix the file, its folder and what it resolves to are owned by root
or the person, and nobody else can write them; on Windows, no `Zone.Identifier`).

- **.deb and RPM:** Tauri writes every entry owned by root with 0755 or 0644, which the package
  manager installs. RPM uses XZ level 6 for its payload. `check.sh` queries RPM metadata with
  `rpm -qp`, extracts with `rpm2cpio` and `cpio`, and applies the same layout, hash, desktop-entry
  and sidecar execution checks as the deb; it never installs the RPM.
- **AppImage:** the image's files are root's (`unsquashfs -lln`); mounted or extracted, the app
  sees them as root's or the person's. Tauri's image carries some files (the bundled libraries'
  copyright notices) with mode 0777, so `build.sh` rebuilds its file system with only the owner able
  to write. This needs `squashfs-tools`; the runtime in front of the file system is kept as it is.
- **DMG:** the person drags the app out, and owns the copy. **Run it from Applications, not from
  the mounted DMG:** a mounted image reports its files as owned by uid 99, so the app would
  refuse its own `pitcrewd` there.
- **Windows:** NSIS writes the files itself, so they carry no `Zone.Identifier` even when the
  installer was downloaded.

### Registrations, and how each is checked

| | `pitcrew://` links | Notifications |
|---|---|---|
| `.deb` | `/usr/share/applications/PitCrew.desktop` has `MimeType=x-scheme-handler/pitcrew;` and `Exec=pitcrew-desktop %u` ([`desktop/pitcrew.desktop.hbs`](desktop/pitcrew.desktop.hbs): Tauri's own template has no `%u`, so the link would not reach the app); dpkg's trigger updates the MIME cache. **Checked:** `check.sh` reads the entry (and runs `desktop-file-validate`); the workflow installs the `.deb` and `smoke.sh` starts it. | D-Bus; nothing to register. |
| RPM | The same `.desktop` template and `Exec=pitcrew-desktop %u` as the deb. **Checked:** owners and modes from RPM metadata, extraction and `desktop-file-validate`. | D-Bus. |
| AppImage | The same entry inside; the app registers itself on first start (`scheme.rs`: a hidden handler in `~/.local/share/applications` and `mimeapps.list`). **Checked:** `check.sh` reads the entry; `smoke.sh` finds the handler named in the throwaway home's `mimeapps.list`. | D-Bus. |
| macOS | `CFBundleURLTypes` in `Info.plist`; Launch Services registers it when the app is copied or first opened. **Checked:** `check.sh` reads `CFBundleURLSchemes` with PlistBuddy. | Sent as the bundle id, `org.pitcrew.desktop`. **Checked:** `CFBundleIdentifier`. |
| Windows | NSIS writes `HKCU\Software\Classes\pitcrew` (`URL Protocol`, `shell\open\command` = `"…\pitcrew-desktop.exe" "%1"`), and removes it on uninstall. **Checked:** `check-windows.ps1` after a real install, and again after removal. | Toasts need an AppUserModelID registered by a Start menu shortcut. NSIS sets `System.AppUserModel.ID` = the app's identifier, `org.pitcrew.desktop`, on its Start menu and desktop shortcuts, and a release build of the app sends its toasts as that identifier already (`notify/windows.rs`), so the app needs no change. **Checked:** `check-windows.ps1` reads the shortcut's property and finds the id in `Get-StartApps`. |

### The checks in the workflow

- **Size:** each installer against the 25 MB budget. Over it is a `::warning::` and a line in
  the job's summary, not a failure.
- **Layout and checksums:** the four programs side by side; `helpers/` complete; each helper's
  sha256 equal to the manifest's; `manifest.json` and the manifest compiled into the app
  (searched for in the executable) equal to the staged one.
- **Owners and modes**, as above; universal binaries in the DMG (`lipo -archs`).
- **The programs run** where the runner can run them: `pitcrewd --version` and
  `pitcrew-ptyd --version` give the manifest's version, and `pitcrew-askpass` refuses to run
  outside ssh (exit 2). `pitcrew-desktop` has no `--version` and needs a display, so instead
  `smoke.sh` (Linux under Xvfb: the installed `.deb` and the AppImage; macOS: the app copied out
  of the DMG) and `check-windows.ps1` start it once in a throwaway home and state folder, and
  read its log: `pitcrewd` found next to it, started and ready, the window up, no askpass
  warning.
- **Windows upgrades:** `check-windows.ps1` installs again while a `pitcrew-ptyd.exe` from the
  install folder runs. A running program cannot be overwritten, so
  [`desktop/installer-hooks.nsh`](desktop/installer-hooks.nsh) moves one in use into `.old\`,
  where it keeps running (ptyd's terminals outlive upgrades); the next install or removal deletes
  what has ended there. A running `pitcrewd.exe` is moved aside the same way: the app goes on
  using that daemon until it stops.

### Tried in a cloud VM

2026-10-02, Ubuntu 24.04 x86_64 with WebKitGTK 2.52, Rust 1.97, as root, Tauri CLI 2.12.1
(`cargo install --locked`, 5.5 minutes):

- `build-release.sh --zig` for both musl targets: all four binaries static, in under 3 minutes
  each; `pitcrewd` is 13 MB.
- `desktop/build.sh x86_64-unknown-linux-gnu`, with a few-byte stand-in for the macOS helper
  (the VM cannot build it): `PitCrew_0.0.0_amd64.deb` (23.2 MB) and `PitCrew_0.0.0_amd64.AppImage`
  (91.2 MB). `check.sh` passed on both (owners and modes from `dpkg-deb -c` and `unsquashfs
  -lln`, `desktop-file-validate` included), and `smoke.sh` started the app from the unpacked
  `.deb` and from the AppImage under Xvfb: its own `pitcrewd` ready, the window up, no askpass
  warning, and the AppImage's `pitcrew://` handler registered.
- `check-windows.ps1` parses (PowerShell 7.4), and `installer-hooks.nsh` compiles with
  `makensis -WX` in a minimal installer; neither has run on Windows here.

### Size

The budget is 25 decimal MB for DMG, deb, RPM and NSIS. AppImage carries WebKitGTK/GTK
and is exempt; its separate budget is its measured size plus 10% (see P.md). The checker
and tables use decimal MB (1,000,000 bytes), with exact bytes in the contents report.
`bash packaging/desktop/contents.sh dist/desktop/*` unpacks each installer without installing it
and prints the largest twenty files, installer bytes and total payload bytes. The release
workflow records this on Linux, macOS and Windows. DMGs are mounted read-only on macOS; NSIS
is inspected with 7-Zip on Windows. These two paths require the integrator's release run.

**Safe changes.** Both Rust release profiles use `opt-level = "z"`, retaining thin LTO, one
codegen unit and symbol stripping. Keep `panic = "unwind"`: hook sinks, adapter scans, runner
jobs, recap operations and the tray use `catch_unwind` to isolate failures. The CLI's hook panic
handler also guarantees exit zero. Changing to abort would break recovery. AppImage
`bundleMediaFramework = false` is explicit; this was already Tauri's default, so its measured
saving is **zero**, not the 15–35 MB sometimes attributed to that option. Libraries WebKitGTK
itself requires remain. RPM uses XZ level 6; it is a new format with no historical baseline.

**Local comparison (2026-10-03).** Debian trixie x86_64 cloud container, Rust 1.99, Zig 0.16,
Tauri CLI 2.12.1, WebKitGTK 2.52.6. Both musl targets were really built. The universal macOS
helper could not be built here, and the existing release artifact was blocked by environment
egress policy. The **same baseline x86_64 Linux daemon** was used in its resource slot before
and after, with valid hashes and a manifest compiled into the desktop. These are local lab
installers, **not distributable releases**; the unchanged substitute holds that input constant
but cannot establish production sizes. The native desktop was compiled with `custom-protocol`
and the staged manifest/config, then packaged with `tauri bundle`; AppImage modes were checked
and repaired by the existing build-script function.

| Linux lab installer | Before bytes (MB) | After bytes (MB) | Change | 25 MB budget |
|---|---:|---:|---:|---|
| deb | 30,678,044 (30.678) | 24,037,596 (24.038) | −21.6% | within |
| RPM, XZ 6 in both comparisons | 20,394,313 (20.394) | 16,097,309 (16.097) | −21.1% | within |
| AppImage | 116,726,264 (116.726) | 117,553,656 (117.554) | +0.7% | over |

The desktop executable alone goes from 15,414,856 to 12,362,960 bytes (−19.8%). These
installer rows measure the two Rust profile changes together; the raw sidecar table below
isolates the root profile. RPM compression measured separately on the optimized payload is
24,033,756 bytes with Gzip level 6 versus 16,097,309 with XZ level 6 (−33.0%). Media-framework
configuration changes no files because it was already false.

The AppImage payload shrinks from 360,152,395 to 345,283,203 bytes, yet the compressed lab
installer increases by 827,392 bytes. The fixed substitute equals the baseline native/Linux
helper, so the before/after images have different opportunities to deduplicate daemon copies.
The bundled libraries and their largest sizes are unchanged. This artificial comparison is
not evidence of a production AppImage reduction; a real Mac helper is required to establish
that delta. It remains far over budget and is why the larger options below stay relevant.

The root profile's raw binary changes (bytes):

| Binary | Before | After | Reduction |
|---|---:|---:|---:|
| x86_64 daemon | 13,408,512 | 9,377,264 | 30.1% |
| aarch64 daemon | 11,834,888 | 8,275,888 | 30.1% |
| x86_64 CLI | 1,799,584 | 1,483,912 | 17.5% |
| aarch64 CLI | 1,538,160 | 1,338,448 | 13.0% |
| x86_64 PTY helper | 1,291,736 | 1,131,432 | 12.4% |
| aarch64 PTY helper | 1,201,952 | 1,068,560 | 11.1% |
| x86_64 askpass | 517,624 | 481,752 | 6.9% |
| aarch64 askpass | 483,744 | 444,320 | 8.1% |

The AppImage's largest library payloads before optimization are WebKitGTK 96,606,841 bytes,
JavaScriptCore 32,892,473 and ICU data 31,868,993. The deb/RPM instead depend on the system's
WebKitGTK and GTK; their space goes to the desktop executable and the daemon copies. The
macOS/Windows file rankings are **not measured here**: run the release workflow on this branch
and read its new contents summary before quoting savings. Expect the optimized desktop and
native sidecars to shrink there too; Linux helpers shrink identically in every installer. The
universal Mac helper requires a real Mac rebuild. No numeric DMG/NSIS reduction is claimed.

The baseline and optimized real Linux daemons passed synthetic demo/API and CLI smoke checks.
100 runs per `whoami`, `task list`, `task show` stayed below 3.5 ms p95 on both builds (50 ms
budget); this is a smoke check on an empty demo, not the 10k-history benchmark. All optimized
x86_64 sidecars ran their version/refusal checks. The aarch64 executables were built and hashed
but not executed (no emulator). A desktop interactive smoke run was not available without a
display/Xvfb, and this container's ancestor ownership also blocks private Unix sockets.

**Installer-size-2 local comparison (2026-10-04).** From `origin/main` `1b3920a`,
with real builds of both musl targets and the desktop, the same fixed Linux stand-in in the
unavailable universal macOS helper slot, before → after: deb 24,946,986 → 17,039,982 bytes
(−31.7%), RPM 16,964,365 → 13,876,305 (−18.2%), AppImage 118,077,944 → 115,927,544 (−1.8%).
All installer checks pass. These lab inputs cannot be distributed as production packages.
AppImage already deduplicates identical raw files and compresses its filesystem, so its
saving is smaller. The lab uses `--appimage-budget-bytes 127520299` (measured size + 10%); the default
budget uses the real production measurement below.
**Production comparison.** Native release dry runs from `1b3920a` → installer code
`1326791`, with real universal macOS and static musl helpers: DMG 35,102,960 → 24,282,548
bytes (−30.8%), deb 27,757,708 → 19,048,126 (−31.4%), RPM 18,890,681 → 15,876,893 (−16.0%),
NSIS 18,132,784 → 18,504,084 (+2.0%), AppImage 99,711,480 → 97,065,464 (−2.7%). DMG,
deb, RPM and NSIS all meet 25 MB. NSIS already compresses its payload, so precompression
and the added decoder slightly increase the installer despite the smaller raw resources.
AppImage's separate production budget is 106,772,011 bytes, its measured size plus 10%.
Exact counts and largest files are published by the native comparison run linked in P.md.
When local policy blocks artifact/log storage, the release workflow's `measure_only` input
compares `desktop-*` artifacts from `before_run` and `after_run` on each native OS. It
builds and publishes nothing; exact byte counts and largest payload files are written to
both summaries and API-readable annotations. Its only extra permission is `actions: read`
for the cross-run artifact download.

**Helper storage and lookup (installer-size-2).** The manifest continues to name decoded
`Platform::artefact()` executables and their decoded SHA-256; its schema is unchanged.
Linux reuses `pitcrewd` beside the desktop for the x86_64 musl entry, and macOS reuses its
universal sidecar for the universal entry. Staging verifies byte identity before omitting
that resource. Windows has no native remote-helper platform, so all three resources remain.
The other entries are `helpers/<artefact>.xz` (XZ, level 6). Installed lookup prefers the native sidecar when the platform matches, then the XZ resource,
then an uncompressed development helper. An explicit helpers override uses only that folder
(XZ before raw); it never changes the compiled manifest trust rule. This order ignores raw
resources left behind by a previous installer. NSIS also removes its three known legacy raw
resources before installing the compressed replacements; its upgrade test plants synthetic
legacy files and checks removal.

Decoded resources enter an owner-only cache through an exclusively created 0600 temporary
file (protected owner-only DACL on Windows). Decoding has a 256 MiB output limit and a
64 MiB decoder-memory limit. The decoded executable must hash to the compiled manifest
before an atomic rename into the cache. Cache names include platform, version and hash;
a version upgrade cannot reuse the old entry. Every cache read is bounded and hashed again.
Symlink/reparse-point cache entries and directories are refused, never repaired. Nothing
from this cache executes locally; the remote installer still checks bytes, hash and version
before its own atomic installation. Adjacent manifests remain development-only.

**Proposals, not implemented:**

- Fetch the Mac helper only when adding a Mac. Fetch from a version-pinned release over HTTPS;
  keep the expected decoded SHA-256 compiled into the desktop, enforce a size limit and atomic
  owner-only cache, and fail closed on hash/version errors. Downloads and their redirect domains
  become part of the product, with offline use and release retention costs. TLS alone is not the
  existing manifest trust guarantee. Windows/Linux users avoid carrying the universal executable.
- A thinner AppImage could rely on host WebKitGTK/GTK, or ship a separate runtime package. This
  sacrifices the self-contained installation and compatibility promise and adds platform/version
  checks. Deb/RPM already make that trade-off. Dropping libraries merely because they look large
  is unsafe; WebKit's linked libraries and codec dependencies remain even without media bundling.

## The portable Windows zip

For a Windows machine that cannot run installers: one zip to unpack into a folder the person may
run programs from, and start PitCrew there. No installer, no administrator, no registry keys.

### Getting it

1. On GitHub, **Actions → Portable Windows zip**, and a successful run on `main` (the newest is
   at the top; a pull request's run has one too). Downloading needs a GitHub sign-in.
2. Under **Artifacts**, `pitcrew-windows-x64-portable.zip`. It is the zip itself (uploaded with
   `archive: false`), kept for 30 days. Its SHA-256 is in the run's summary, with its files and
   their sizes, and in the upload step's log as the artifact's digest.
3. **Before unzipping, unblock it:** Properties → **Unblock**, or
   `Unblock-File .\pitcrew-windows-x64-portable.zip`. Windows marks downloaded files, Explorer
   passes the mark on to what it unzips, and the app refuses its own programs while they carry it
   (`Zone.Identifier`, see "What the app's trust check needs").
4. Unzip into a folder of its own, and start `pitcrew-desktop.exe`.

The zip holds, at its root:

| File | What |
|---|---|
| `pitcrew-desktop.exe` | The app (the release build, the UI inside). |
| `pitcrewd.exe`, `pitcrew-ptyd.exe`, `pitcrew-askpass.exe`, `pitcrew.exe` | The daemon, the terminal supervisor, ssh's prompt helper and the agent CLI, side by side with the app as the installer puts them. |
| `LICENSE`, `NOTICE` | PitCrew's licence (Apache-2.0). |
| `THIRD-PARTY-NOTICES.txt` | Every crate the programs link (`cargo metadata`, normal dependencies, for `x86_64-pc-windows-msvc`) and every JavaScript package the window bundles (`apps/ui`'s production dependencies), each with its licence, source and the licence files its package carries; packages sharing a text are listed under it once. ([`notices.mjs`](notices.mjs)) |
| `README-portable.txt` | For the person: unblocking, checking, where the data is, what differs. ([`portable/README-portable.txt`](portable/README-portable.txt)) |
| `portable.txt` | The marker: the app is a portable copy. |
| `SHA256SUMS` | Every other file's SHA-256, as `sha256sum --check` reads it. |

### Checking it

After unzipping, in PowerShell, in the folder (the same lines are in `README-portable.txt`, and
the smoke test runs them from there):

```powershell
Get-Content SHA256SUMS | ForEach-Object {
  $sum, $file = $_ -split '  ', 2
  if ((Get-FileHash -Algorithm SHA256 -LiteralPath $file).Hash -eq $sum) { "ok   $file" } else { "BAD  $file" }
}
```

Elsewhere, `sha256sum --check SHA256SUMS`. `.\pitcrew-desktop.exe --check-layout | Out-String`
shows what the app finds next to itself without opening a window.

### What portable mode changes

The app is a portable copy when `portable.txt` is next to it (`apps/desktop/src-tauri/src/portable.rs`,
see the desktop's README, "A portable copy"):

- **Finding the programs** does not change: the layout is the installed one, so `pitcrewd`,
  `pitcrew-askpass` (next to the app), `pitcrew-ptyd` and `pitcrew` (next to `pitcrewd`) are
  found as installed, with the same trust check.
- **The state** does not move: it stays where an installed PitCrew keeps it (`%LOCALAPPDATA%\PitCrew\data`,
  `%APPDATA%\org.pitcrew.desktop`, `%LOCALAPPDATA%\org.pitcrew.desktop`), never next to the
  programs, so moving between the zip and the installer keeps it. The smoke test checks nothing
  is written into the unzipped folder.
- **Updates** are never installed: a newer version is shown as usual, and **Update** opens the
  workflow's successful runs on `main` instead (the
  [contract](../docs/build/contracts/desktop-gateway.md#desktop-updates)).
- **Notifications** show as Windows PowerShell's: Windows shows toasts only for an
  AppUserModelID a Start menu shortcut registers.
- **Not in the zip:** `pitcrew://` links (the installer registers them), the remote helpers
  (`helpers/`; adding a machine that needs one says so), signing, and WebView2, which Windows 10
  and 11 include (without it the app says so, and the Evergreen Bootstrapper is
  <https://go.microsoft.com/fwlink/p/?LinkId=2124703>).

### How it is built

[`release-portable.yml`](../.github/workflows/release-portable.yml) on `windows-2025`, as the
release workflow's Windows jobs build (the channel from `rust-toolchain.toml`, actions pinned by
commit SHA, `permissions: {}` and only `contents: read`, no caches):

```bash
corepack pnpm install --frozen-lockfile && corepack pnpm --filter @pitcrew/ui build
CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS="-C target-feature=+crt-static" \
  packaging/build-release.sh x86_64-pc-windows-msvc       # Git Bash, on Windows
packaging/portable/build.sh                              # the app, the notices, the zip
pwsh -File packaging/portable/smoke.ps1 -Zip dist/portable/pitcrew-windows-x64-portable.zip
```

- **The C runtime is linked into every program** (`+crt-static` for the four, tauri-build's
  `staticVCRuntime` for the app), so nothing needs the Visual C++ Redistributable, which is an
  installer. The installers' programs link it dynamically, as before.
- `portable/build.sh` builds the app with `cargo build --release --locked --features
  custom-protocol --target x86_64-pc-windows-msvc` (no Tauri bundle, no helpers manifest compiled
  in), writes the notices, copies the rest, writes `SHA256SUMS` and checks it, and zips the folder
  with `zip`, or 7-Zip where `zip` is missing.
- `portable/smoke.ps1` unzips it into a fresh folder outside the checkout and checks: exactly
  those files, matching `SHA256SUMS` (also by `README-portable.txt`'s own lines); no program
  importing the Visual C++ runtime (read from the PE headers); `pitcrewd`, `pitcrew` and
  `pitcrew-ptyd` answering `--version` with one version, `pitcrew-askpass` refusing outside ssh;
  `pitcrew-desktop --check-layout` finding its four programs and the marker (redirected, and piped
  to `Out-String` as `README-portable.txt` says) and refusing a `pitcrewd.exe` marked as downloaded
  until it is unblocked; `pitcrewd.exe` with a temporary state directory and the demo workspace
  (no agent home watched) answering `GET /v1/host/info` on a loopback port with its runner's
  terminals in the `pitcrew-ptyd` next to it (`pty`); and nothing written into the folder.
- A separate job runs [`test.sh`](test.sh), which builds the zip from stand-ins.

## Checksums

`SHA256SUMS` lists every file in the directory as `<hex>  <name>`, sorted by name, which is what
`sha256sum --check` reads:

```bash
sha256sum --check --ignore-missing SHA256SUMS
```

The desktop installers carry the helpers' checksums from the same workflow run (see
[The desktop installers](#the-desktop-installers)); `SHA256SUMS` lists the installers too.

## The release workflow

On a `v*` tag, [`release.yml`](../.github/workflows/release.yml):

| Job | Runs on | Permissions | Does |
|---|---|---|---|
| `build` | Ubuntu (musl x86_64 and aarch64, via Zig), macOS (universal), Windows (x86_64) | `contents: read` | Builds, checks the Linux binaries are static, runs the x86_64 `pitcrewd` and `pitcrew-ptyd` in `centos:7` (glibc 2.17) with no network, signs (placeholder), uploads the binaries. |
| `desktop` | Ubuntu (x86_64), macOS (universal), Windows (x86_64) | `contents: read` | From the `build` job's binaries: builds the UI and the installers (`desktop/build.sh`), checks them (`desktop/check.sh`; on Windows a real install, upgrade and removal), starts the installed app (`desktop/smoke.sh`, Linux and macOS), signs (placeholder), uploads the installers. |
| `manifest` | Ubuntu | `contents: read` | Runs `test.sh`, writes SBOMs and `SHA256SUMS` over everything (binaries and installers), signs `SHA256SUMS` (placeholder), verifies the directory. |
| `attest` | Ubuntu | `id-token: write`, `attestations: write` | Verifies the downloaded files against `SHA256SUMS`, then one build-provenance attestation whose subjects are every file in it. |
| `release` | Ubuntu | `contents: write` | Verifies again, then a **draft** release with every file; a tag with a `-` is a pre-release. |

The workflow has no default permissions. Actions are pinned by commit SHA, tools by version
(`cargo install --locked --version`, the Tauri CLI included) or by hash (Zig), and there is no
build cache. The Rust channel comes from `rust-toolchain.toml`. A manual run (`workflow_dispatch`)
stops after `manifest`, so the pipeline can be tried without a tag: `attest` and `release` run
only for a pushed `v*` tag, so a manual run publishes nothing. Its artefacts are on the run's page
for a week.

Check an attestation with the GitHub CLI:

```bash
gh attestation verify pitcrewd-x86_64-unknown-linux-musl --repo <owner>/<repo>
```

### Signing secrets

The signing steps are placeholders. Each reads its kind's secrets by name and is **skipped**
when none of them is set. If **any** is set, the step **fails**: nothing signs yet, an unsigned
file must not look signed, and a partly configured kind is a mistake to fix, not to skip. Only
secret names are ever printed. `bash packaging/test.sh` checks these rules.

| Step | Secrets |
|---|---|
| macOS: codesign and notarise | `APPLE_CERTIFICATE` (base64 .p12), `APPLE_CERTIFICATE_PASSWORD`, `APPLE_SIGNING_IDENTITY`, `APPLE_ID`, `APPLE_TEAM_ID`, `APPLE_APP_PASSWORD` |
| Windows: Authenticode | `WINDOWS_CERTIFICATE` (base64 .pfx), `WINDOWS_CERTIFICATE_PASSWORD` |
| Linux and all: sign `SHA256SUMS` | `MINISIGN_SECRET_KEY`, `MINISIGN_PASSWORD` |

Secrets live only in the repository's secrets and reach only the step that uses them.

### Signing the desktop installers

Tauri's bundler signs while it bundles, so the desktop's placeholders stand before the Bundle step
and read the same secrets. To sign for real, the maintainer provides:

- **macOS:** a Developer ID Application certificate (`.p12`, base64) and its password, the signing
  identity's name, and for notarisation an Apple ID with an app-specific password and the team
  id (or an App Store Connect API key: `APPLE_API_KEY`, `APPLE_API_ISSUER`, the key file). Then:
  1. `sign.sh macos` in the `build` job signs every binary in `dist/` with
     `codesign --options runtime --timestamp`, **the universal helper included, before the
     desktop job reads its sha256**. Notarisation needs every Mach-O in the app signed, and
     Tauri seals `Resources/helpers/` without changing it, so the compiled checksums still match.
  2. The desktop job passes `APPLE_CERTIFICATE`, `APPLE_CERTIFICATE_PASSWORD`,
     `APPLE_SIGNING_IDENTITY`, `APPLE_ID`, `APPLE_PASSWORD` (from `APPLE_APP_PASSWORD`) and
     `APPLE_TEAM_ID` to the Bundle step instead of the placeholder. Tauri imports the
     certificate into a temporary keychain, signs the programs next to the app and the app with
     the hardened runtime, notarises and staples it, then builds the DMG.
- **Windows:** an Authenticode certificate (`.pfx`, base64) and its password, or a signing service
  that `signtool` or a command can reach, and an RFC 3161 timestamp URL. Then `sign.sh windows`
  in the `build` job signs the `.exe` files in `dist/`, and `desktop/build.sh` adds
  `bundle.windows.signCommand` (or `certificateThumbprint`, once the `.pfx` is imported) to
  `tauri.bundle.json`: Tauri then signs the app, its programs, the NSIS plugins, the uninstaller
  and the installer.
- **Linux:** the `.deb` and the AppImage are covered by the signed `SHA256SUMS` (minisign, as
  above).

Each kind stays off until all of its secrets are set and its step is implemented.

The desktop sidecars include `pitcrew` alongside `pitcrewd`, `pitcrew-ptyd` and
`pitcrew-askpass` on Linux, macOS and Windows. Hook installation requires this CLI
even when an app starts without the developer's PATH. Staging and extracted
installer checks require the CLI and verify its version; NSIS moves a running CLI
aside during upgrades with the other sidecars.

## Signed desktop updates

The desktop uses [Tauri's updater](https://v2.tauri.app/plugin/updater/). Checks run at startup
and daily; Settings offers **Check now** and an explicit pre-release opt-in. Installation needs
confirmation and restarts the app. The plugin checks the minisign signature before installation.
Linux self-update supports AppImage; deb/rpm users use their package manager.

One-time maintainer setup, on a trusted machine, outside the repository:

```bash
cargo install tauri-cli --locked --version 2.12.1
cargo tauri signer generate -w /secure/pitcrew-updater.key
```

Choose a password and keep the private key and password backed up securely. Do not add either
to the repository or paste them into logs. Add the **contents** of the private key file to the
repository Actions secret `TAURI_SIGNING_PRIVATE_KEY`, and its password to
`TAURI_SIGNING_PRIVATE_KEY_PASSWORD` (empty for an unencrypted key). Add the entire contents of
`pitcrew-updater.key.pub` to the repository Actions **variable** `TAURI_UPDATER_PUBLIC_KEY`.
The public key is public configuration: `updater-config.mjs` puts it in the Tauri build overlay,
which is compiled into each signed app. Keep it stable: existing installs trust that key.
For a local signed build set these same environment variables before `desktop/build.sh`.

Without the private key, builds still produce every installer but compile an empty public key,
create no updater artifacts/signatures, and print that automatic updates are unsigned and
disabled. A private key without a public key, or a password without a key, fails rather than
silently producing an unusable signed release. Initial unsigned installs require one manual
installation of a signed build before they can update themselves.

With the key, Tauri builds the updater artifacts. The script copies the macOS `.app.tar.gz`
archive, signs the **final** AppImage after its permissions are repaired, and signs the NSIS
installer and macOS archive. Signatures sit beside those files. `updater-feed.mjs` requires all
four platform entries (the universal macOS archive serves both architectures) and generates
`latest.json` before `SHA256SUMS`, SBOM verification and provenance attestation. The release tag
sets the compiled desktop version as well as the feed version. Manual runs use the config's
version and publish nothing. Installer checks inspect installers only, not `.sig` or updater
archives. These updater signatures are independent of the installer code-signing placeholders.

On stable checks the app reads `/releases/latest/download/latest.json`. With pre-releases enabled
it selects the greatest newer semantic version with a feed in the latest 100 published GitHub
releases, then reads that tag's `latest.json`. Drafts are never offered. Publishing a draft makes
its feed accessible; publishing a pre-release does not replace GitHub's stable latest feed.
Release notes open the fixed GitHub tag page in the system browser. The feed is untrusted
metadata; signing uses `--app-version`, and the app requires that version in the authenticated
signature (`requireSignedVersion`). Relabeling an old signed artifact as a newer release fails.

Run `node --test packaging/updater.test.mjs` for the feed/config checks; the manifest job runs
these alongside `bash packaging/test.sh`. Dispatch Release on the branch for the three-OS
installer checks without publication. Do not rotate the public key without planning a manual
reinstall or a transition signed with the old key.
