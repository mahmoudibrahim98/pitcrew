# Stream L · UI foundation

**Goal:** the UI's base: the app skeleton, components on the design tokens, the shell with both
layouts and the switcher, routing, the palette, the Orchestrator panel frame, and the data layer
every feature uses.

**Owns:** `apps/ui/*` (package.json, Vite and TS config, index.html), `apps/ui/public/**`,
`apps/ui/src/*` (entry files), `apps/ui/src/{design,shell,data}/**`.
**Depends on:** stream 0 (tokens, mock hub).  **Model:** Opus-class.
**Read first:** ADR-0008, ADR-0003 (CSP); `packages/tokens/`; `docs/build/contracts/api-v1.md`.

## Work packages

1. **Skeleton:** Vite, React 19 with the React Compiler, TypeScript strict, Tailwind 4 on the
   tokens, Radix primitives, TanStack Router, Query and Virtual, zustand for UI state only.
   Fonts bundled (no CDN). No inline scripts (CSP).
2. **Design system** (`src/design`): buttons, inputs, menus, dialogs, drawers, tabs, badges,
   avatars (person vs agent), status pills, empty states, toasts, virtualised list and table.
   Light and dark, two densities, reduced motion, WCAG 2.2 AA, keyboard first.
3. **Data layer** (`src/data`): API client (Bearer header in dev against the mock hub; the Tauri
   gateway in the app), the `/v1/stream` connection with resume by `since`, and an event →
   query-key invalidation map. Features add their own query hooks in their folders.
4. **Shell** (`src/shell`): sidebar (workspace switcher, Home, Inbox, My tasks, projects with
   their workstreams, Agent console), the layout toggle (**Ctrl .**), workspace-scoped routes
   (`/w/:ws/...`), the palette (**Ctrl K**), the Orchestrator panel frame (**Ctrl J**), "+ New".
5. **Feature registration:** each feature folder (`console`, `projects`, `onboarding`) exports
   its routes and nav entries from `index.ts`; the shell composes them. Document the interface.
6. **Tests:** Vitest and Testing Library; Playwright against the mock hub; an axe check.

## Acceptance

- `pnpm dev` runs the UI against `npm run mock-hub` and shows the demo workspace.
- Every component usable by keyboard; axe reports no violations on the shell.
- A delta on the stream updates exactly the affected queries (tested).
- Initial JS for the shell < 250 KB gzipped; routes are code-split.

## Do not

Build feature pages (M, N, O do); call the daemon except through `src/data`.
