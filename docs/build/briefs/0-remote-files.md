# Brief 0 · Files on remote machines

- **Stream:** 0 · Composition root (runner, remote link, daemon).
  **Branch:** `integrator/remote-files`.
  **Paths:** `crates/runner/**`, `crates/daemon/**`, `crates/remote/**` (only the link that carries
  runner commands), `crates/protocol/**` (regenerate `packages/protocol-ts`),
  `docs/build/contracts/api-v1.md`, `tests/conformance/**`, the threat model, and the READMEs of
  what you touch.
- **First read:**
  - [README.md](README.md) and the root `AGENTS.md` (or `CLAUDE.md`);
  - [0-files-api.md](0-files-api.md) and PR #37 (the rules, the runner's file operations, T80);
  - `crates/protocol/src/runner.rs` (`RunnerCommand`, `CommandOutcome`) and how the daemon reaches
    a remote machine's runner today.
- **Suggested agent:** Codex, or any coding agent.

## Goal

The files API answers 501 for a location on another machine. Make list, read and write work there
too, with exactly the same rules, by running the same file operations on that machine's runner.

## What to build

1. **Runner commands** for list, read and write (or one `Files` command with an operation), carried
   over the existing hub ↔ runner link; the remote runner applies `pitcrew_runner::files` with its
   own roots (the location's path on that machine), its own backups, and the same path rules.
2. **The daemon** routes a request for a remote location to that machine's runner and maps the
   outcome to the same HTTP answers (413, 409, 403, 404); 503 when the machine is unreachable.
3. **Limits:** the existing size caps apply end to end; bodies are not buffered twice beyond the cap.
4. **Tests:** a fake remote runner over the link; the path-rule table on the remote side; an
   unreachable machine; conformance where the targets support it; T80 updated.

## Acceptance

- fmt, clippy with `-D warnings`, the tests of the crates touched, `cargo test -p pitcrew-protocol
  --features ts`, `npm test`, both conformance targets, and the guards pass. Every CI job passes on
  the pull request.
