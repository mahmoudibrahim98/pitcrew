# Stream I · Agent CLI and hooks

**Goal:** `pitcrew`, the command agents use to report, ask and coordinate, and `pitcrew hook`,
the near-zero-cost hook for every agent turn.

**Owns:** `crates/cli/**`.  **Depends on:** stream 0; H's API contract (build against the mock
hub or a fake socket).  **Model:** Sonnet-class.
**Read first:** ADR-0006, ADR-0010; `docs/build/contracts/api-v1.md` (routes marked **agent**).

## Work packages

1. **Transport:** connect to the daemon's socket (`PITCREW_SOCKET`) with the agent token from
   `PITCREW_TOKEN` (set by the runner for sessions it starts; a 0600 file for others). Clear
   errors when absent.
2. **Verbs** (agent-facing, stable, scriptable, `--json` on all): `whoami`, `claim <task>`,
   `report <task> --done|--blocked|--note`, `task show|list|move|plan`, `ask <to> …`,
   `reply <ask>`, `check` (my asks and mentions), `brief <task>`, `needs`, `who`, `comment`.
3. **Hooks:** `pitcrew hook <event>` for Claude (SessionStart, UserPromptSubmit, Stop,
   SessionEnd), Codex (its notify hook), and an OpenCode plugin. Fire-and-forget to the daemon,
   **≤ 10 ms wall time**, never blocking the agent when the daemon is down; inject context only
   where the event needs it.
4. **Install and uninstall:** show the exact **diff** of each CLI's config first; idempotent;
   never remove what it did not write. Works from sh, Git Bash, cmd and PowerShell.

## Acceptance

- Each verb tested against a fake server on a temp socket, including error paths.
- Hook wall time measured in CI (release build) ≤ 10 ms with the daemon up, ≤ 5 ms when down.
- Install then uninstall leaves config files byte-identical.

## Do not

Hold or accept device tokens; talk HTTP over TCP; edit agent configs without showing a diff.
