# Brief L · UI skeleton and data layer

- **Stream:** L · UI foundation. **Branch:** `s/L/skeleton-and-data`. **Paths:** `apps/ui/*`,
  `apps/ui/public/**`, `apps/ui/src/*`, `apps/ui/src/{design,shell,data,lib,assets}/**`,
  `apps/ui/{tests,e2e}/**`, plus the shared `pnpm-lock.yaml`. Leave `src/console`, `src/projects`
  and `src/onboarding` to their streams (keep their README files).
- **First read:** [README.md](README.md), then `docs/build/streams/L.md`, ADR-0008, ADR-0003
  (CSP), `packages/tokens/`, `docs/build/contracts/api-v1.md`, `apps/mock-hub/README.md`.

## Goal

A running UI package whose **data layer** talks to the mock hub and stays live through the delta
stream. It includes a minimal page proving it works. This is work packages 1 and 3 of your card.
The full shell (sidebar, layout switcher, palette) is the next brief.

## What to build

1. **Package** `apps/ui/package.json`:
   - name `@pitcrew/ui`, private, `type: module`;
   - scripts `dev`, `build`, `preview`, `test`, `typecheck`, `lint`;
   - install with `corepack pnpm install` from the repo root, and commit the updated
     `pnpm-lock.yaml`;
   - respect the workspace's `minimumReleaseAge` and `onlyBuiltDependencies` in
     `pnpm-workspace.yaml`.
2. **Stack:**
   - Vite; React 19 with the React Compiler (`babel-plugin-react-compiler`); TypeScript strict;
   - Tailwind 4 whose theme maps to the `--pc-*` custom properties from `@pitcrew/tokens`
     (`workspace:*`);
   - Radix primitives, TanStack Router, Query and Virtual; zustand for UI-only state.
   - **Bundle Geist and Geist Mono** from an npm package (OFL). No font CDN, no remote origins.
   - The production `index.html` has **no inline scripts**.
3. **Data layer** (`src/data`):
   - An API client. The base URL comes from `VITE_PITCREW_API` (default
     `http://127.0.0.1:47317`), the token from `VITE_PITCREW_TOKEN` in development
     (`dev-device-token`), sent as `Authorization: Bearer`. It parses errors as `ApiError`.
   - Hand-written TypeScript types for what you use, mirroring the serde names, in
     `src/data/types.ts`, until generated types exist.
   - A stream client for `/v1/stream`:
     - auth by subprotocol;
     - resume with `since`, and drop the cache when `since` is ahead of `hello.rev`;
     - reconnect with back-off, and on 60 s of silence.
   - An **event → query-key invalidation map** covering every `EventBody` type (e.g.
     `task_moved` → that task, task lists, its workstream).
   - Query hooks for the workspace, projects, workstreams, tasks, sessions and asks. Features add
     their own hooks later.
4. **Minimal proof page:**
   - projects with their workstreams, open-ask count and live session status lines, from the
     mock hub;
   - a button that moves a task through the API, whose result arrives via the stream and
     re-renders without a reload;
   - styled with the tokens, light and dark.

## Acceptance

- `corepack pnpm --filter @pitcrew/ui dev` against `npm run mock-hub` shows the demo workspace.
  Moving a task updates the page through the stream. Include a screenshot or a Playwright trace
  in your report.
- Vitest tests:
  - the invalidation map has an entry for every event type;
  - the stream client resumes from the last `to_rev` and handles a reset;
  - the API client maps errors.
  - Where practical, run against the real mock hub by importing `startServer` from
    `apps/mock-hub/src/server.ts` on port 0.
- `typecheck` and `build` pass. Report the gzipped size of the initial JS (budget: under
  250 KB).
- `dist/index.html` contains no inline script.

## Out of scope

The sidebar, layout switcher, palette and Orchestrator panel (next brief), the design-system
components beyond what the proof page needs, and the feature folders. CI does not run UI jobs
yet. List the CI steps you need (install, typecheck, test, build) in your report, and the
integrator will add them.
