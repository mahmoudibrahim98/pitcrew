# Brief 0 · The Orchestrator panel's conversation

- **Stream:** 0 · Composition root (dispatch, office, shell UI).
  **Branch:** `integrator/orchestrator-chat`.
  **Paths:** `apps/ui/src/shell/**` (the panel), `apps/ui/src/data/**`, `crates/daemon/**`,
  `crates/hub-work/**`, `crates/office/**` (prompts as files), `crates/cli/**` (read verbs the agent
  uses), `crates/protocol/**` (regenerate `packages/protocol-ts`), `docs/build/contracts/api-v1.md`,
  `apps/mock-hub/**`, `tests/conformance/**`, the threat model, `Cargo.lock`, and the READMEs of
  what you touch. Mechanical edits elsewhere are fine; say which in the report.
- **First read:**
  - [README.md](README.md) and the root `AGENTS.md` (or `CLAUDE.md`);
  - `docs/adr/0008-two-layouts.md` (the panel is everywhere) and
    `docs/adr/0010-real-cli-sessions.md`;
  - [0-dispatch.md](0-dispatch.md) and [0-draft-board.md](0-draft-board.md): the same "no API keys,
    run an agent the person already uses" pattern;
  - the panel today: `apps/ui/src/shell/` (Ctrl J). It says "Its conversation arrives in a later
    version."
- **Suggested agent:** an Opus-class agent. Run it after 0-draft-board, whose prompt-and-proposal
  plumbing it should reuse.

## Goal

The Orchestrator panel answers questions about the person's work across projects, sessions and
machines: what each agent did today, what's blocked, where something was decided, which session
touched a file. It answers with links to the sessions, tasks and recaps it used.

## What to build

1. **No API keys:** each question runs as a session of an agent CLI the person already uses (their
   choice of engine, remembered), started by the hub in a private scratch folder with:
   - a versioned prompt from `crates/office/prompts/`;
   - an **agent token limited to reading**, through `pitcrew` CLI read verbs: search, sessions,
     recaps, tasks, activity. Add verbs only where one is missing.
   It can't write files outside its scratch folder, move tasks, or send to sessions. Any action is
   returned as a suggestion the person clicks.
2. **Streaming:** the answer streams into the panel from the session's transcript, as the console
   does. Links in the answer resolve to app routes (session, task, workstream, recap). Untrusted text
   is rendered as text, never as HTML.
3. **Conversation:**
   - follow-up questions reuse the same session while it lives;
   - the history is kept per person and per workspace, and can be cleared;
   - a "new conversation" action;
   - a visible cost or usage note for each answer, from the transcript.
4. **Bounds:** time and size limits on each answer, cancellation (Esc), and a clear state when no
   engine is installed or signed in, linking to onboarding's sign-in.
5. **Tests:**
   - a fake agent CLI answers end to end;
   - the token can't write: every write route returns 403 for it;
   - cancellation and limits;
   - the mock hub's parity;
   - conformance for the routes;
   - a threat-model entry: prompt injection from transcripts can only produce suggestions, never
     actions.

## Acceptance

- Asking "what did my agents do today?" on a workspace with sessions returns an answer with working
  links, against the real hub with a fake CLI in CI.
- fmt, clippy with `-D warnings`, the tests of the crates touched, `cargo test -p pitcrew-protocol
  --features ts`, `npm test`, the UI's checks, both conformance targets, and the guards pass. Every
  CI job passes on the pull request.
