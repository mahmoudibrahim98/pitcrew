# Brief Q · Threat model and first fuzz targets

- **Stream:** Q · Security. **Branch:** `s/Q/threat-model-and-fuzz`. **Paths:**
  `docs/security/**`, `fuzz/**`.
- **First read:** [README.md](README.md), then `docs/build/streams/Q.md`, ADR-0006, ADR-0009,
  ADR-0003, `SECURITY.md`, and the merged code on `main`: `crates/{ingest,runtime,api,auth,store,protocol}`,
  plus `crates/remote` once merged. The review files that found real issues are good context:
  ask the integrator; they are not in the repo.

## Goal

A living **threat model** for PitCrew, and **fuzz targets** for every parser that reads
untrusted input and has already landed.

## What to build

1. **`docs/security/threat-model.md`:**
   - assets: tokens, transcripts, agent control, source code, credentials on remote machines;
   - actors: other users on shared machines, malicious transcript, issue or file content
     (prompt injection), a compromised dependency, a stolen token, a malicious remote host;
   - trust boundaries: webview ↔ gateway ↔ daemon ↔ runner ↔ agent CLIs ↔ tmux and PTY ↔
     SSH ↔ remote helper.

   For each threat: the control, **where it lives in code** (file or crate), how it is tested,
   and the owning stream. Mark gaps clearly as open items with an owner. It must cover every row
   of the security table in the ADRs, plus what the reviews found:
   - the nested-router auth bypass;
   - tmux format and `%exit` forging;
   - token-in-build;
   - unbounded parser inputs;
   - socket and pipe squatting.
2. **Fuzz targets** (`fuzz/`, a `cargo-fuzz` project; its own `Cargo.toml` workspace, **not**
   a member of the main workspace):
   - `ingest::claude::parse_line`;
   - the Codex parser's line function;
   - `ClaudeAdapter`/`CodexAdapter` `read_from` on arbitrary file bytes;
   - `runtime::ControlParser::feed`, arbitrary chunking included;
   - `protocol::runner::decode_line`;
   - `serde_json` into `Event`, `StreamFrame` and `TranscriptPage`.

   Seed corpora from the fixtures. Add dictionaries where useful.
3. **Local runs:** each target for **60 seconds** (not longer: several agents share this
   machine). Report executions per second and any findings. A crash found is a result: write a
   minimal reproducer as a regression test proposal for the owning stream, in your report; don't
   fix other streams' code.
4. **CI proposal:** a nightly workflow file content (5 minutes per target) in your report. The
   integrator adds it, because `.github/workflows` belongs to stream 0.

## Environment

`cargo-fuzz` needs a nightly toolchain: `rustup toolchain install nightly` and
`cargo install cargo-fuzz` inside WSL (no sudo needed). Use `CARGO_BUILD_JOBS=2`.

## Acceptance

- Every target builds and runs for 60 s locally; any crash has a minimized reproducer in the
  report.
- The threat model covers the table above, with code references that exist.

## Out of scope

Changing other streams' code; CodeQL and zizmor setup (proposal only).
