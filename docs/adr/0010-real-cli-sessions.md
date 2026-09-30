# 0010. Agents stay real CLI sessions

- **Status:** Accepted
- **Date:** 2026-09-30

## Context

Agent CLIs (Claude Code, Codex, OpenCode) change quickly and each has its own features, login and
permission model. Structured protocols (`stream-json`, app-server modes, SDKs) exist, but a tool
built on them replaces what the user knows, and breaks when the protocol shifts.

## Decision

PitCrew runs the **real CLIs in real terminals** (P7). The truth about a session is its
transcript on disk and its hooks. Users can always `tmux attach` or resume a session with the
CLI itself, without PitCrew.

- Source adapters read each CLI's transcripts **incrementally by byte offset**, read-only, and
  never copy them (stream A).
- Hooks are fire-and-forget to the daemon, under 10 ms, installed only after the user sees the
  diff (stream I).
- CLI logins stay on the machine where the agents run, in the CLIs' own files. PitCrew opens a
  terminal to run the CLI's own login; it never reads or copies OAuth tokens.
- The default permission mode is the CLI's own; skipping permissions is an explicit opt-in.

## Consequences

- Import is "read in place": existing sessions on a machine appear without moving anything.
- A headless worker engine on structured protocols may come later, next to real sessions, for
  dispatches that need no terminal.
