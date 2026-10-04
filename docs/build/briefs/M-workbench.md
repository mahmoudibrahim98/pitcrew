# Brief M · The console workbench: tabs, splits and a fuller file viewer

- **Stream:** M · Agent console.
  **Branch:** `integrator/workbench`.
  **Paths:** `apps/ui/src/console/**`, `apps/ui/src/projects/**` (reusing the Files tab's viewer),
  `apps/ui/src/data/**`, `apps/ui/src/design/**` (only new primitives the workbench needs), and the
  desktop CSP (`apps/desktop/src-tauri/tauri.conf.json`) only if a viewer needs a new source (say why).
- **First read:**
  - [README.md](README.md) and the root `AGENTS.md` (or `CLAUDE.md`);
  - `docs/build/streams/M.md` items 4–6 (terminal, workbench, links to the work);
  - [N-files-tab.md](N-files-tab.md) and PR #43 (the file viewer and its blob previews);
  - `apps/ui/src/console/README.md` (or the console's code) and the e2e tests.
- **Suggested agent:** an Opus-class agent, or Codex.

## Goal

Work with several things at once: sessions, terminals and files open side by side, in tabs and
splits, the way an IDE's editor area works.

## What to build

1. **Tabs and splits:** open a session's chat, its terminal, or a file in a tab; split horizontally
   or vertically; drag tabs between panes; close, reorder; the layout persists per workspace (local
   storage is fine) and survives reloads.
2. **The file viewer** (shared with the Files tab): text with line numbers and, for common languages,
   syntax colouring (a small, pure-JS highlighter; no raw HTML); images; **PDF** via the browser's
   own viewer or a bundled pdf.js worker, within the desktop CSP; edit and save with the existing
   revision/conflict flow.
3. **Details sidebar:** the session's task, workstream, machine, state, model and account; actions
   from M.6 that exist today (link to task, hand off where available).
4. **Keyboard:** switch tabs and panes, close, open the command palette entry for each.
5. **Tests:** unit tests for the layout model; e2e against the mock hub (open, split, drag, reload
   restores); axe in both themes.

## Acceptance

- The UI's checks (typecheck, lint, test, build, e2e), the CSP check, `npm test` and the guards
  pass. Every CI job passes on the pull request.

## Out of scope

TeX compile, annotations on PDFs, remote files (brief 0-remote-files).
