# Brief L · Shell polish and the data layer for the new contract

- **Stream:** L · UI foundation. **Branch:** `s/L/shell-polish`. **Paths:** stream L's
  (`apps/ui/*`, `src/design`, `src/shell`, `src/data`, `src/lib`, `tests`, `e2e`).
- **First read:** [README.md](README.md), [L-shell.md](L-shell.md) (merged), and
  `docs/build/contracts/api-v1.md`. The task-editing contract change is now merged, adding
  `PATCH /v1/tasks`, `POST /v1/projects`, `POST /v1/workstreams` and `Brief.proposal`.

## Why

Streams M (console), N (projects) and O (onboarding) wired their features into the shell and
found gaps in it. One of them is an accessibility bug.

## What to build

1. **Focus rings** (accessibility bug). Buttons that combine `outline-none` with
   `focus-visible:outline-2` show no ring in Tailwind v4, because the computed `outline-style`
   is `none`. Seen at `src/shell/sidebar.tsx:52` and `:166`, `top-bar.tsx:44`, and
   `orchestrator.tsx:29`.
   - Fix it everywhere in L's paths, for example with `focus-visible:outline-solid`, or one
     shared focus utility in `src/design`.
   - Add an e2e check that tabbing to these controls shows a visible outline.
2. **`CreateEntry` with a reason:** an optional `disabled?: string` that shows the "+ New" item
   disabled, with its reason as an accessible description. Stream M wants a "Session" item that
   is disabled until starting a session works. Update the shell README and the registry tests.
3. **Breadcrumb:** at a 700 px window the top bar's breadcrumb truncates to "Agent …". Make it
   degrade gracefully, e.g. collapse the middle segments first, with the full path in a tooltip
   or accessible label.
4. **TypeScript coverage for feature e2e configs:** `tsconfig.node.json` doesn't reach
   `src/**/e2e/**`, so stream O needed `/// <reference types="node" />` in its Playwright config.
   Include `src/**/e2e/**` in `tsconfig.node.json`, keep it out of `tsconfig.app.json`, and
   remove O's reference line. That file is in `src/onboarding/e2e/`; touch only that one line.
5. **The data layer for the merged contract** (`src/data`):
   - types: `TaskPatch`, `NewProject`, `NewWorkstream`, `BriefProposal`, `Brief.proposal`, and
     `BriefAccepted`'s `next` and `receipts`;
   - typed calls: `patchTask`, `createProject`, `createWorkstream`.
   - Writes never touch the cache directly; the events they cause invalidate it. Check that
     `project_created`, `workstream_created` and `task_updated` invalidate the right keys.
   - Test each call against the mock hub, including one 400 and the 409 cycle case for
     `blocked_by`.
6. **Optional, if small:** `NavEntry.to` accepting search params, so a filtered console view
   can be a sidebar entry. Skip it if it means redesigning the nav model, and say so.

## Acceptance

- Typecheck, lint, the tests, the build and `size` pass.
- The shell e2e passes, with axe clean in both themes and layouts, including the new focus check.
- No feature folder changed apart from the one reference line in item 4.

## Out of scope

Routes outside `/w/$ws`. The hub always has exactly one workspace, created when `pitcrewd`
first starts, so onboarding runs inside it.
