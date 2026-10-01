# Brief P · Benchmark harness and release pipeline skeleton

- **Stream:** P · Packaging and release. **Branch:** `s/P/bench-and-release`. **Paths:**
  `benches/**`, `packaging/**`, `.github/workflows/release*.yml`.
- **First read:** [README.md](README.md), then `docs/build/streams/P.md` (the budgets table),
  ADR-0002, ADR-0009, and the merged crates on `main`.

## Goal

1. A **benchmark harness** that measures the budgets we can already measure, with a baseline
   and a regression check.
2. A **release workflow skeleton** that builds signed-ready artefacts for the helper and the CLI.

## What to build

1. **`benches/`** (the `pitcrew-benches` crate; it is already a workspace member):
   - criterion benchmarks for what exists:
     - transcript `read_page` newest page, and full `read_from` (Claude and Codex), on
       generated 20 MB and 200 MB files;
     - store `append` batches and `since`/`before` pages;
     - the control-mode parser's throughput;
     - the delta stream's end-to-end latency (append → frame) over `MemorySource`, and over
       the real `Store` if cheap.
   - Generated inputs go in a temp dir, never committed.
   - A small **runner script** (`benches/run.sh` or a Rust bin) writes a JSON summary with the
     budget name, value, unit and budget, and compares it to `benches/baseline.json`. It fails
     on a regression over 10%. Commit a baseline from this machine, noting the machine class.
2. **`packaging/`:**
   - how to build the static helper: `x86_64-unknown-linux-musl` and `aarch64`, via
     `cargo-zigbuild` or `cross`;
   - a script that produces `pitcrewd` and `pitcrew` release binaries plus a
     `SHA256SUMS` manifest.

   Try the x86_64 musl build locally if the toolchain installs without sudo
   (`rustup target add`, zig via pip); report what worked.
3. **`.github/workflows/release.yml`**, triggered on `v*` tags:
   - minimal `permissions`; every action **pinned by commit SHA** (look them up with
     `gh api`, as `ci.yml` does);
   - build matrix: Linux musl x86_64 and aarch64, macOS universal, Windows x86_64;
   - artefacts plus `SHA256SUMS`, an SBOM (e.g. `cargo-cyclonedx`), and
     `actions/attest-build-provenance`;
   - a draft GitHub release.

   Signing steps are placeholders that read secrets by name and are skipped when absent. The
   desktop bundle (Tauri) is out of scope until stream K lands.

## Acceptance

- `cargo bench -p pitcrew-benches` runs (a quick mode for CI, full mode locally), and the runner
  script produces the JSON and passes against the committed baseline.
- `release.yml` is valid; check it with `actionlint` if available, otherwise explain.
- Report the numbers, and the local musl build result.

## Out of scope

The desktop bundle, actual signing identities, the update feed.
