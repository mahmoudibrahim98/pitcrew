# Stream P · Packaging and release

**Goal:** signed, reproducible releases people can trust, and the benchmark harness that keeps
PitCrew fast.

**Owns:** `packaging/**`, `.github/workflows/release*.yml`, `benches/**`.
**Depends on:** stream 0.  **Model:** Sonnet-class.
**Read first:** ADR-0002, ADR-0003, ADR-0009; the budgets table below.

## Work packages

1. **Static helper builds:** `pitcrewd` for Linux musl x86_64 and aarch64, macOS and Windows;
   sha256 manifest consumed by the desktop build (for helper verification, ADR-0009).
2. **Desktop bundles:** Tauri installers for Windows, macOS (universal) and Linux (AppImage,
   deb, rpm).
3. **Signing:** macOS notarisation; Windows signing (e.g. an open-source signing programme);
   Linux artefact signatures; Tauri updater signatures and the update feed.
4. **Supply chain:** SBOM per release; GitHub build-provenance attestations; release workflow
   with minimal permissions and pinned actions.
5. **Benchmark harness** (`benches/`): runs the budgets below on reference hardware in CI and
   fails on a regression over 10%.

| Metric | Budget |
|---|---|
| `pitcrewd` idle CPU, 50 live sessions | ≤ 0.5% of one core |
| `pitcrewd` memory, 10k sessions indexed | ≤ 80 MB RSS |
| Cold start to serving (index present) | ≤ 300 ms |
| First scan, 10k transcripts on SSD | ≤ 60 s, streamed |
| Hook event → UI change | ≤ 300 ms local, ≤ 1 s over SSH |
| `pitcrew` verb round trip | ≤ 50 ms |
| `pitcrew hook` wall time | ≤ 10 ms |
| Desktop cold start to interactive | ≤ 1.5 s |
| Desktop idle RAM, 3 workspaces | ≤ 300 MB |
| Transcript open to first paint | ≤ 300 ms at any size |
| Installer size (DMG, deb, RPM, NSIS) | ≤ 25 MB (decimal) |
| AppImage size | Separate budget: measured size + 10%; exempt from 25 MB because it carries WebKitGTK and GTK |

## Installer-size-2 measurement

Linux lab, 2026-10-04, Rust 1.99, Tauri CLI 2.12.1, WebKitGTK 2.52.6:

| Installer | Before bytes | After bytes | Change |
|---|---:|---:|---:|
| deb | 24,946,986 | 17,039,982 | −31.7% |
| RPM | 16,964,365 | 13,876,305 | −18.2% |
| AppImage | 118,077,944 | 115,927,544 | −1.8% |

Both Linux package formats meet 25,000,000 bytes. AppImage's measured size plus 10%,
rounded up to a byte, is **127,520,299 bytes (127.52 MB)**, its separate checker budget.
WebKitGTK (96.6 MB), JavaScriptCore (32.9 MB) and ICU data (31.9 MB) dominate its raw
payload; keeping these libraries is part of the AppImage's compatibility promise.

Both comparisons build the desktop and both static musl targets from `origin/main`
`1b3920a`, with only this brief's changes in the after build. The unavailable universal
macOS helper uses the same fixed 13,408,512-byte Linux executable stand-in in both;
these are **local lab measurements, not distributable installers or production sizes**.
Production DMG, NSIS and Linux measurements are recorded by the release dry runs and
reported in the PR. This table does not claim macOS or Windows savings.

## Acceptance

- A tagged release produces signed artefacts, checksums, SBOM and attestations.
- The helper binary runs on an old-glibc Linux container with no network.

## Do not

Store signing secrets anywhere but repository secrets; widen workflow permissions beyond need.
