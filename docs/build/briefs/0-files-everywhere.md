# Brief 0 · Files everywhere

- **Stream:** 0 · Composition root (files UI, console workbench).
  **Branch:** `integrator/files-everywhere`.
  **Paths:** `apps/ui/src/files/**` (new, or move the Files tab's code here),
  `apps/ui/src/console/**` (the workbench), `apps/ui/src/projects/**` (the Files tab),
  `apps/ui/src/shell/**` (the sidebar entry and the palette's quick open), `apps/ui/src/data/**`,
  `crates/runner/**` and `crates/daemon/**` (only for git status or ignore-aware listing),
  `crates/protocol/**`, `apps/mock-hub/**`, `tests/conformance/**`,
  `docs/build/contracts/api-v1.md`, and the READMEs of what you touch.
- **First read:**
  - [README.md](README.md) and the root `AGENTS.md` (or `CLAUDE.md`);
  - [0-files-api.md](0-files-api.md) and [M-workbench.md](M-workbench.md);
  - the threat model's T80.
- **Suggested agent:** Codex.

## Goal

A file viewer and browser exist, but only as a workstream's Files tab, and people don't find them.
The tab also lists a worktree's `.git` file, and has no icons, search or breadcrumbs. Make files
reachable wherever work happens.

## What to build

1. **An explorer panel** in the console and in the Projects layout: the tree of the current
   workstream's locations, collapsible, with file-type icons. `.git` and `.gitignore`d entries are
   hidden by default, with a "show hidden" toggle.
2. **Open files as workbench tabs.** From the explorer, from search, and from file paths in a
   session's chat (Read, Edit and Write tool calls, and diffs), at the right line. Splits work as
   for sessions.
3. **Quick open** (Ctrl P): fuzzy search over the files of the current workstream, bounded.
4. **Breadcrumbs** on the file viewer; copy path; on the desktop, "Reveal in folder" and "Open in
   editor" through the shell's allow-listed opener.
5. **Git status badges** (modified, untracked) where the runner can provide them cheaply, with the
   same path rules as the files API.
6. **Tests:**
   - explorer, quick open and tabs against the real hub and the mock;
   - hidden files;
   - path rules unchanged (T80).

## Acceptance

- From a session that edited `sections/method.tex`, one click opens that file at the edited line
  in a workbench tab.
- fmt, clippy with `-D warnings`, the tests of the crates touched, `npm test`, the UI's checks, both
  conformance targets, and the guards pass. Every CI job passes on the pull request.
