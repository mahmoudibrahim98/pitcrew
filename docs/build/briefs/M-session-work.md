# Brief M · A session's work in the console; a sturdier test

- **Stream:** M · UI: Agent console. **Branch:** `s/M/session-work`. **Paths:** `apps/ui/src/console/**`.
- **First read:** [README.md](README.md), the `src/console` README, `apps/ui/src/projects/index.ts`
  and `recaps.tsx` (`SessionWork`, exported for you), and the `src/data` README (the recap
  hooks).

## Goal

The console's session view shows what the session did, in recap form, beside its chat and
terminal.

## What to build

1. **Mount `SessionWork`** (lazy, from `src/projects/index.ts`) in the session pane:
   - either as a third view in the Chat | Terminal switch ("Work"), kept in `?view=work`, or as a
     collapsible section under the header; pick one, and say why;
   - wrap it in `<Suspense>` and an error boundary;
   - on the narrow layout it follows the same rules as the terminal.
   - Tests use `tz=0` (the Projects tests' `RecapTzProvider`, or the hooks' override).
2. **The flaky test:** `console-page.test.tsx` "says so when the session does not exist" failed
   once under load. Make its wait robust: wait for the 404 response, or use `findBy` with a
   timeout that matches the rest of the suite. Then run the file 10 times in a row and report.
3. **Playwright:** the session's Work view shows its blocks with their lines, and axe passes
   with it open, in both themes and both layouts.

## Acceptance

- Typecheck, lint, the tests, the build and `pnpm size` all pass. `SessionWork` stays out of the
  initial JS.
- The console Playwright suite passes, including the new case.

## Out of scope

Changes to `src/projects` (if `SessionWork` needs a change, describe it), and the shared popover
(stream L).
