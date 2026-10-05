# Brief 0 · Resume sessions started elsewhere, and recaps with substance

- **Stream:** 0 · Composition root (runner, daemon, console UI, recaps).
  **Branch:** `integrator/resume-sessions`.
  **Paths:** `crates/runner/**`, `crates/daemon/**`, `crates/hub-work/**` (recaps),
  `crates/protocol/**` (regenerate `packages/protocol-ts`), `apps/ui/src/console/**`,
  `apps/ui/src/data/**`, `apps/mock-hub/**`, `tests/conformance/**`,
  `docs/build/contracts/api-v1.md`, the threat model, and the READMEs of what you touch.
- **First read:**
  - [README.md](README.md) and the root `AGENTS.md` (or `CLAUDE.md`);
  - [0-start-sessions.md](0-start-sessions.md) (PR #62) and `docs/adr/0010-real-cli-sessions.md`;
  - how a session's terminal is attached today.
- **Suggested agent:** Codex. Run it after #62 merges.

## Goal

Most sessions a person has were started outside PitCrew, so they have no terminal here. Today the
composer still looks usable on them. Send then fails with "Session ses_… has no terminal here:
PitCrew did not start it", truncated and showing an internal id. The Work tab's recap says only
"started a session, finished 2 turns".

## What to build

1. **Resume into PitCrew.** On a session with no terminal, the composer is replaced by
   "Resume in PitCrew".
   - It starts the engine's own resume (Claude `--resume <id>`, Codex's resume, OpenCode's
     equivalent) in a terminal on the session's machine and folder, attached to the same session.
   - It is refused, with a reason, when the CLI is missing, the folder is gone, or the session is
     still live in another terminal.
   - The arguments go as argv, never through a shell.
2. **Clear states.** Where nothing can be sent, the composer is disabled and says why in plain
   words, with no internal ids. Errors from Send are shown in full.
3. **Recaps with receipts.** The Work tab and recaps show:
   - a one-paragraph summary when one exists;
   - files changed, with counts, each opening the file or diff;
   - commands run, with their exit codes;
   - duration;
   - tokens and cost where the transcript records them.
   Every line links to its source turn. Bounded; nothing from the transcript is rendered as HTML.
4. **Tests:** resume end to end with a fake CLI; the refusal cases; recaps built from the fixture
   transcripts.

## Acceptance

- A session started outside PitCrew can be resumed and driven from the console in two clicks.
- fmt, clippy with `-D warnings`, the tests of the crates touched, `cargo test -p pitcrew-protocol
  --features ts`, `npm test`, the UI's checks, both conformance targets, and the guards pass. Every
  CI job passes on the pull request.
