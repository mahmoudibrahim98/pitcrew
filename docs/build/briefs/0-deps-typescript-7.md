# Brief 0 · TypeScript 7, or a reasoned "not yet"

- **Stream:** 0 · Contracts (the JS toolchain across the workspace). **Branch:**
  `integrator/deps-typescript-7`.
  **Paths:**
  - the root `package.json` and lockfile;
  - `apps/ui/package.json`, `apps/ui/tsconfig*.json`, `apps/ui/eslint.config.*`, and the UI's source
    and tests, only where the new compiler reports real errors;
  - `apps/mock-hub/**` and `packages/tokens/**`, if they compile TypeScript;
  - `.github/dependabot.yml`, only for item 4.
- **First read:** [README.md](README.md), the root `AGENTS.md` (or `CLAUDE.md`), `apps/ui/README.md`,
  and TypeScript 7's release notes (what changed from 6, and which JS API it ships).
- **Suggested agent:** Codex, or any coding agent.

## Goal

Dependabot's PR #5 bumps `typescript` from 6.0.3 to 7.0.2. TypeScript 7 is the new native compiler,
so the question is whether the tools around it work with it, not just whether the code compiles.
Either move the workspace to 7 with everything green, or show why it can't move yet and keep
Dependabot from re-opening the PR.

## What to do

1. **Check the tools first.** Each must support TypeScript 7 in a released version:
   - `typescript-eslint` (8.71 today; it uses the compiler's API);
   - `@vitejs/plugin-react` and `vite` (they only strip types);
   - `vitest`;
   - `@playwright/test`;
   - anything else that imports `typescript`. Find these with `npm ls typescript`.

   Note each one's supported TypeScript range.
2. **If they all do:**
   - bump `typescript` to 7.0.2 and the tools to versions that support it;
   - make `npm run typecheck`, `npm run lint`, `npm test`, `npm run build` and the end-to-end tests
     pass in `apps/ui`, plus whatever `apps/mock-hub` and `packages/tokens` run;
   - fix real type errors in the code; don't loosen `tsconfig` (`strict` and the other flags stay);
   - if a flag was removed in 7, say what replaced it.
3. **Report what changed:** typecheck time before and after (`tsc -b` cold), and anything that
   behaves differently.
4. **If a tool doesn't support 7 yet:**
   - keep 6.0.3;
   - add an `ignore` for `typescript`'s major 7 to the npm entry in `.github/dependabot.yml`, with a
     comment naming the blocking tool and the release to watch for;
   - say in the report what would unblock it.

## Acceptance

- Either every UI check and every CI job passes on 7, or the report shows the blocking tool's
  supported range and the Dependabot ignore is in place.
- No `// @ts-ignore`, `any` casts, or eslint disables were added to get it through.

## Out of scope

Other dependency upgrades, and UI changes.
