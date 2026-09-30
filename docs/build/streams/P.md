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
| Installer size | ≤ 25 MB |

## Acceptance

- A tagged release produces signed artefacts, checksums, SBOM and attestations.
- The helper binary runs on an old-glibc Linux container with no network.

## Do not

Store signing secrets anywhere but repository secrets; widen workflow permissions beyond need.
