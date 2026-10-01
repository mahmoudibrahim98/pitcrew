# Brief M · Console wiring: routes, the three panes, and the shared data layer

- **Stream:** M · UI: Agent console. **Branch:** `s/M/console-wiring`. **Paths:**
  `apps/ui/src/console/**`.
- **First read:** [README.md](README.md), then [M-console-components.md](M-console-components.md)
  (merged), `docs/build/streams/M.md` (work packages 1, 2, 3 and 6), `apps/ui/src/shell/README.md`
  (feature registration, routes, commands, `ownsShellKeys`), and `apps/ui/src/data/README.md`
  plus `api.ts`, `types.ts` and `hooks.ts` (stream L added the console's calls and types there).

## Goal

Make the Agent console a working layout in the app: a person opens it from the shell and sees
sessions filtered on the left, the list in the middle, and the chosen session's chat with the
composer on the right. It runs against the mock hub with live updates.

## What to build

1. **Feature registration** in `src/console/index.ts` (keep the lazy component exports):
   - routes under the workspace, for example `/w/$ws/console` and
     `/w/$ws/console/s/$session`;
   - nav entries and palette commands (open the console, jump to a session, filter by machine or
     state);
   - "+ New" items if the shell supports them (a new session; disabled with a reason until the
     start-session flow exists).

   Follow the shell README exactly. If something you need is missing from the feature API, stop
   and describe it, and don't edit `src/shell`.
2. **The three panes:**
   - filters on the left (controlled; kept in the URL search params, so a link reproduces the
     view);
   - the list in the middle;
   - the session on the right: header, chat and composer.
   - Panes are resizable with the shell's design components and remember their sizes.
   - Keyboard: move between panes; the list's arrows choose a session.
   - Narrow window: one pane at a time, with back navigation.
3. **The shared data layer:**
   - Switch to `src/data`'s calls, types and `useMachines`.
   - Delete the console's duplicates in `console/api.ts` and `console/types.ts`, or make them
     thin re-exports.
   - Remove the tail refetch workaround in `useTranscript` now that `session_state_changed`
     invalidates the transcript tail.
4. **Links to the work:** the header's task and workstream links navigate to the Projects
   layout's routes. Use the shell's navigation helpers; if Projects hasn't registered its routes
   yet, link by path as the shell README describes.
5. **Review follow-ups** from `M-console-components`:
   - **Bound the markdown emphasis matcher** (`render/markdown-parse.ts` `findOpener`/`emphasis`):
     a run like `a* b* c* …` repeated tens of thousands of times is O(n²).
     - Cap the total delimiter-matching work, as `diff-parse.ts` does with `MAX_WORK`, falling
       back to plain text.
     - Test with a large adversarial string under a time bound.
   - **Test the asks-loading guard:** delay `/v1/asks`, and check that a transcript question
     can't be answered with keys while asks are unknown, and can afterwards in the right mode.
   - **Safari IME:** also treat `keyCode === 229` as composing.
6. **Where the composer's keys go:** the composer and chat must not lose Ctrl K/J/B to the shell
   (the shell already yields inside text fields). Use `ownsShellKeys` only where a surface truly
   owns keys; the terminal brief will need it.

## Acceptance

- In the running app against the mock hub (Playwright, your own ports, `PLAYWRIGHT_CHANNEL=msedge`):
  - open the console from the sidebar and the palette;
  - filter by machine and state (the URL updates; reloading keeps it);
  - pick SES0001 and see the full transcript;
  - send a prompt and see the reply arrive live;
  - answer the question in SES0003;
  - follow the task link into Projects.
- axe passes on the console in both themes and both layouts, if the shell's e2e has them.
- No code from `src/console` loads before the console route is visited (check the build
  output).
- Typecheck, lint, the tests and the build pass; `pnpm size` stays within budget.

## Out of scope

The terminal (xterm.js, a new dependency through stream L), the workbench tabs and splits, the
file viewer, the model, effort and account chips, and Hand off, Fork and Review.
