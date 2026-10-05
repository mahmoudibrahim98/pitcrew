# Brief L · Shell and onboarding polish

- **Stream:** L · Shell (and O · Onboarding's UI).
  **Branch:** `integrator/shell-polish`.
  **Paths:** `apps/ui/src/shell/**`, `apps/ui/src/design/**`, `apps/ui/src/onboarding/**`,
  `apps/ui/src/console/**` (workbench layout only), `apps/ui/src/index.css`,
  `apps/desktop/src-tauri/**` (only the splash or window background), and the READMEs of what you
  touch.
- **First read:**
  - [README.md](README.md) and the root `AGENTS.md` (or `CLAUDE.md`);
  - the gr8r Studio reference's shell (sidebar, header, palette, empty states);
  - `apps/ui/src/shell/README.md`.
- **Suggested agent:** an Opus-class agent with a strong design sense.

## Goal

The audit's polish list for the parts every screen shares, and for the first five minutes.

## What to build

1. **Shell.**
   - Real icons for every nav entry: Projects, Members and Calendar show the letters "P", "M" and
     "C" today.
   - Engine logos on agents.
   - A loading splash instead of a blank white window.
   - Designed empty states with the action that fills them.
   - Unfinished menu items ("Hand off · Soon") hidden.
   - A plain-words pass on the copy ("back office" becomes what it does).
2. **Palette.**
   - Create commands (New task, session, project, workstream, agent).
   - Actions on the open item (move, assign, dispatch, link).
   - Recent items.
   - Keyboard hints.
3. **Workbench.**
   - Minimum pane widths: a split that can't fit stacks or refuses.
   - The filter column collapses by default under a width.
   - Tab strips don't show scrollbars.
4. **Onboarding.**
   - **Welcome:** what PitCrew does, in one line and one picture.
   - **Workspace:** the machine name defaults to the computer's name; placeholders don't look like
     values.
   - **Create:** each suggestion's folder, merging two suggestions, correct plurals.
   - **Import:** the filter ticks the scanned folders instead of typing them, says "OpenCode", and
     offers "skip sub-agents".
   - **Hooks:** a plain summary per agent ("Adds 5 hooks to Claude Code: session start, …") with
     each file's diff folded.
   - **Safety:** plain words, and no layout jump.
   - **Done:** next steps (start a session, create a task, invite someone).
5. **Tests:** the shell's keyboard paths, the palette, and onboarding end to end on the real hub
   and the mock; axe in both themes.

## Acceptance

- No screen shows a letter where an icon belongs, a blank window while loading, an unexplained
  term, or an unfinished feature.
- `npm test`, the UI's checks, desktop shell checks if touched, and the guards pass. Every CI job
  passes on the pull request.
