# Brief Q · Fuzz the new parsers; bring the threat model up to date

- **Stream:** Q · Security. **Branch:** `s/Q/fuzz-and-model-refresh`. **Paths:** `fuzz/**`,
  `docs/security/**`.
- **First read:** [README.md](README.md), [Q-threat-model-and-fuzz.md](Q-threat-model-and-fuzz.md)
  (merged), `docs/build/streams/Q.md`, `docs/security/threat-model.md` (especially §8.4 "Targets to
  add"), `fuzz/README` or the header of `fuzz/Cargo.toml`, `.github/workflows/fuzz.yml`, and the
  contracts `docs/build/contracts/api-v1.md` and `desktop-gateway.md`.

## Goal

Since your first round, most streams have merged parsers and new trust boundaries. Fuzz the
parsers that take untrusted bytes, and bring the threat model up to date with what main does now.

## What to build

1. **New fuzz targets.** Each one asserts more than "no panic" where a property exists:
   - **`crates/remote`:**
     - `probe::parse` on arbitrary output;
     - `config::list_hosts_in` on arbitrary config text, including `Include`;
     - a round-trip property: `quote::sh_quote`/`remote_command` against `sh_split`;
     - `askpass::classify`.
   - **`crates/ingest` OpenCode:** the SQLite reader on arbitrary database bytes. Keep the files
     small, and bound the time per input.
   - **`crates/api`:** the terminal control messages (resize JSON), and the activity query
     parameters.
   - **`crates/sync-github`:** the `Link` header parser, the wire JSON for issues, pull
     requests and milestones, and the origin path check. For the path check, the property is
     that no accepted path escapes the base.
   - **`crates/cli` hooks:**
     - the config readers and mergers for Claude `settings.json`, Codex `config.toml` and
       OpenCode, on arbitrary existing files;
     - the property: install then uninstall leaves the file meaning the same, and never drops a
       key that isn't ours.
   - **`crates/store`:** import of an arbitrary export file. It must be refused cleanly or
     imported whole, never half.
   - **`crates/recap`:** the builder over arbitrary event sequences. Every fact keeps its
     receipt, and `verify()` holds.
   - **`crates/remote` SLURM:** add the squeue and sacct line parsers and the site TOML reader
     **if `s/J/slurm` has merged** when you get there. If it hasn't, list them under §8.4 instead.
   - Seed every corpus from the fixtures and the crates' own test inputs (`fuzz/seed.py`). Add
     dictionaries where they help.
2. **Run each new target** locally for 60 seconds. Report the executions per second and any
   crash.
   - **A crash** is a finding: reduce it, and file it as a failing test or a clear reproduction for
     the owning stream, in your report. Don't change another stream's code.
   - **The nightly CI** (`fuzz.yml`) picks targets up from the directory. Check that it does,
     and that its time budget still fits with the extra targets. If it doesn't, propose the
     change to the integrator.
3. **The threat model** (`docs/security/threat-model.md`), brought up to date with main:
   - **The desktop gateway** (webview ↔ gateway ↔ daemon), from its contract: the token never
     in the webview, the path checks, back-pressure, and the CSP and capabilities (stream K is
     building them now; mark what is still unbuilt).
   - **The back office** acting as the agent `@office` inside the daemon: its scope, the rules
     re-checked by the hub, no token, and idempotent replay.
   - **SLURM jobs** (from J's brief and branch): the job script's integrity, never cancelling
     someone else's job, node-local sockets, and site recipes as trusted input.
   - **GitHub and Jira reads:** untrusted upstream text, credentials, JQL injection, and rate
     limits.
   - **The terminal WebSocket:** untrusted output into xterm (OSC 52, titles, links), and
     keystroke limits.
   - **Store leases on NFS:** the residual risk C documented.
   - Update §6 "Findings from reviews" from the merged briefs and their review follow-ups, as
     the briefs and crate READMEs record them. Update §7 "Open items" and §9 coverage.

If a function you need isn't public, don't change its crate. Fuzz the nearest public entry
point, and list the seam you'd want (for example a `#[doc(hidden)] pub` function) as a proposal
for the owning stream.

## Acceptance

- Every new target builds with `cargo +nightly fuzz build` and runs for 60 seconds without a crash
  (or the crash is reported as a finding with a reproduction).
- `fuzz/seed.py` seeds the new corpora; the corpora stay small (state the total size).
- The threat model names an owner stream for every new control, and §8.4 lists what is still
  missing.
- `npm test`, `node scripts/ci/path-guard.mjs --base main` and
  `node scripts/ci/ownership-audit.mjs --untracked` pass.

## Out of scope

Changing other streams' code, the desktop's CSP itself (stream K), and new CI jobs (propose
them).
