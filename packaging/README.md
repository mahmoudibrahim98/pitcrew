# Packaging

Release builds of `pitcrewd` (the daemon, which is also the remote helper) and `pitcrew` (the
agent CLI and hook entry point), with checksums, SBOMs and signing hooks. **Owned by stream P.**
The desktop bundle (Tauri) is added once stream K lands.

| Script | What it does |
|---|---|
| [`build-release.sh`](build-release.sh) | Builds both binaries for one or more targets into `dist/` and writes `SHA256SUMS`. |
| [`sha256sums.sh`](sha256sums.sh) | Writes `DIR/SHA256SUMS` over every file in `DIR`. |
| [`verify.sh`](verify.sh) | Checks `DIR` against its `SHA256SUMS`: every hash matches, and no file is unlisted. |
| [`sbom.sh`](sbom.sh) | Writes CycloneDX SBOMs, `pitcrewd.cdx.json` and `pitcrew.cdx.json`. |
| [`sign.sh`](sign.sh) | Signing placeholders: skipped when none of a kind's secrets is set, failing when any is. |
| [`test.sh`](test.sh) | Tests `sha256sums.sh`, `verify.sh` and `sign.sh` (bash only, no Rust). |
| [`zig-requirements.txt`](zig-requirements.txt) | Zig from PyPI for `cargo-zigbuild`, pinned by version and wheel hash. |
| [`../.github/workflows/release.yml`](../.github/workflows/release.yml) | Runs all of the above on a `v*` tag. |

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

The result is `dist/pitcrewd-<target>` and `dist/pitcrew-<target>` plus `dist/SHA256SUMS`.

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

## Checksums

`SHA256SUMS` lists every file in the directory as `<hex>  <name>`, sorted by name, which is what
`sha256sum --check` reads:

```bash
sha256sum --check --ignore-missing SHA256SUMS
```

The desktop build (stream K) takes the helper's checksums from the release's `SHA256SUMS`.

## The release workflow

On a `v*` tag, [`release.yml`](../.github/workflows/release.yml):

| Job | Runs on | Permissions | Does |
|---|---|---|---|
| `build` | Ubuntu (musl x86_64 and aarch64, via Zig), macOS (universal), Windows (x86_64) | `contents: read` | Builds, checks the Linux binaries are static, runs the x86_64 helper in `centos:7` (glibc 2.17) with no network, signs (placeholder), uploads the binaries. |
| `manifest` | Ubuntu | `contents: read` | Runs `test.sh`, writes SBOMs and `SHA256SUMS` over everything, signs `SHA256SUMS` (placeholder), verifies the directory. |
| `attest` | Ubuntu | `id-token: write`, `attestations: write` | Verifies the downloaded files against `SHA256SUMS`, then one build-provenance attestation whose subjects are every file in it. |
| `release` | Ubuntu | `contents: write` | Verifies again, then a **draft** release with every file; a tag with a `-` is a pre-release. |

The workflow has no default permissions. Actions are pinned by commit SHA, tools by version
(`cargo install --locked --version`) or by hash (Zig), and there is no build cache. The Rust
channel comes from `rust-toolchain.toml`. A manual run (`workflow_dispatch`) stops after
`manifest`, so the pipeline can be tried without a tag; its artefacts are on the run's page for
a week.

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
