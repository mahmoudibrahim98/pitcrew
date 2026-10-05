# Brief 0 · Data you can trust: sub-agents, worktrees, links, attribution

- **Stream:** 0 · Composition root (ingest, runner, work model, console and Home UI).
  **Branch:** `integrator/trusted-data`.
  **Paths:** `crates/ingest/**`, `crates/runner/**`, `crates/daemon/**`, `crates/hub-work/**`,
  `crates/api/**` (activity), `crates/protocol/**` (regenerate `packages/protocol-ts`),
  `apps/ui/src/console/**`, `apps/ui/src/projects/**`, `apps/ui/src/onboarding/**`,
  `apps/ui/src/shell/**` (Home only), `apps/ui/src/data/**`, `apps/mock-hub/**`,
  `tests/conformance/**`, `docs/build/contracts/api-v1.md`, the threat model, and the READMEs of
  what you touch. Mechanical edits elsewhere are fine; say which in the report.
- **First read:**
  - [README.md](README.md) and the root `AGENTS.md` (or `CLAUDE.md`);
  - the audit of 5 Oct 2026 (summarised below; the integrator holds the full log);
  - `crates/ingest/src/claude/mod.rs` (`discover`, `to_meta`), `crates/ingest/src/scan.rs`
    (`git_root`, `build_suggestions`, `aggregate_counts`), the Codex and OpenCode adapters'
    `is_subagent`, and `crates/daemon/README.md` ("A sub-agent runs as its parent").
- **Suggested agent:** an Opus-class agent.

## Goal

What PitCrew shows matches what happened. The audit, on synthetic homes shaped like real use,
found that most of what feels broken comes from the data:

- **Sub-agents count as agents.** Live discovery returns every `<session>/subagents/*.jsonl` as its
  own transcript, and the import counts sub-agents as sessions (14 imported where the scan found
  10). They appear in "Agents now", the console and activity as separate agents, with no link to
  their parent.
- **Worktrees become projects.** `git_root` stops at the first `.git`, and a worktree's `.git` is a
  file pointing at its repo's common dir. So every worktree, including `.claude/worktrees/*`, is
  proposed as its own project: three repos became five projects.
- **Sessions don't land in their projects.** The import creates workstreams only for the odd
  cases, so the real repos get none, and 12 of 14 sessions stay Unsorted.
- **Attribution and order are wrong.** Activity says "@alex started the session" and "finished a
  turn" for sessions the person never started in PitCrew, and for a run dispatched to an agent.
  The feed is ordered by import revision, not by time.

## What to build

1. **Sub-agents are children.**
   - Every adapter sets a sub-agent's `parent`: Claude's `subagents/` folder and the older
     `isSidechain` files, Codex `source.subagent`, OpenCode `parent_id`.
   - Import counts and suggestions exclude sub-agents.
   - The console nests children under their parent (collapsed, "2 sub-agents").
   - "Agents now" and the sidebar counts show parents only.
   - Activity folds a sub-agent's turns into its parent's.
   - The parent's chat shows where each sub-agent ran, linking to its transcript.
2. **Worktrees belong to their repo.**
   - Resolve a `.git` file (`gitdir:` → `commondir`) to the main worktree.
   - The import proposes one project per repo, with one workstream per worktree (named by branch)
     plus the main checkout.
   - Folders outside any repo stay as they are.
3. **Every imported session is linked.**
   - Each project gets a default workstream at its root.
   - Sessions are linked by folder: a worktree session goes to its workstream, a root session to
     the default one.
   - The console's "Link to…" accepts several sessions at once, accepts a project (its default
     workstream), and can create a workstream inline.
4. **Attribution.**
   - Events from discovered sessions name the agent (engine and session), not the person.
   - A dispatched run names the dispatched agent.
   - "@person started the session" appears only when that person started it from PitCrew.
5. **Order and noise.**
   - Activity is ordered by when things happened.
   - "Since you last looked" leaves out the person's own actions and folds started/finished-turn
     pairs into one line per session.
   - "Agents now" shows active sessions first, then at most a few recent ones, with "Show all".
6. **Details read the model and account** from the transcript where it records them.
7. **Tests:**
   - synthetic homes like the audit's: a repo with two worktrees (one under `.claude/worktrees`),
     Claude sub-agents in both formats, a Codex sub-agent and a Codex exec run;
   - assert the counts, suggestions (3 projects), links (0 unsorted), nesting, attribution and
     order;
   - conformance for any API change;
   - mock-hub parity.

## Acceptance

- On the audit's synthetic homes, the scan proposes 3 projects, imports 10 sessions (with 4 nested
  sub-agents), leaves nothing Unsorted, and Home lists no sub-agent as an agent.
- fmt, clippy with `-D warnings`, the tests of the crates touched, `cargo test -p pitcrew-protocol
  --features ts`, `npm test`, the UI's checks, both conformance targets, and the guards pass. Every
  CI job passes on the pull request.
