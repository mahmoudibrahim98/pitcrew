# Stream Q · Security (reviewer)

**Goal:** keep PitCrew safe by design and by review: a living threat model, fuzzing of every
parser that reads untrusted input, and review of every security-sensitive change.

**Owns:** `docs/security/**`, `fuzz/**`.  **Depends on:** stream 0.  **Model:** Opus-class.
**Read first:** ADR-0006, ADR-0009, ADR-0003; `SECURITY.md`.

## Work packages

1. **Threat model** (`docs/security/threat-model.md`): assets, actors (other users on shared
   machines, malicious transcript/issue/file content, compromised dependencies, a stolen token),
   trust boundaries (webview ↔ gateway ↔ daemon ↔ runner ↔ agent CLIs ↔ SSH), and the control
   for each threat. Keep it current as streams land.
2. **Fuzz targets** (`fuzz/`, `cargo-fuzz`): transcript parsers (A), runner protocol
   `decode_line` (0), API request bodies (H), tmux control-mode parser (B), SLURM output parsing
   (J). Seed corpora from the fixtures.
3. **Reviews:** every pull request touching `crates/auth`, `crates/api`, `crates/remote`, the
   runner's files API, hooks, the desktop gateway, CSP, or release workflows gets a Q review
   before merge.
4. **CI proposals** (via the integrator): CodeQL, `zizmor` for workflows, `cargo-audit`, a CSP
   check on the built UI.
5. **Milestone reviews** at M3 and M5.

## Acceptance

- Each fuzz target runs for 5 minutes in CI without a crash (nightly job).
- The threat model covers every item in the plan's threat table, with an owner stream per
  control.

## Do not

Change other streams' code directly: file findings with a failing test or a clear reproduction.
