# onboarding (stream O)

The first-run wizard and the add-a-machine wizard. See `docs/build/streams/O.md`.

| File | What |
|---|---|
| `api.ts` | The proposed `OnboardingApi` contract (below): every call the wizards make, typed, none of it real yet. |
| `fake-api.ts` | `createFakeOnboardingApi()`: an in-memory implementation that behaves plausibly (streamed progress, a fixable row, synthetic scan suggestions). Built against `OnboardingApi`, so a real client drops in without touching a step. |
| `api-context.tsx` | `OnboardingApiProvider`, `useOnboardingApi()`. Defaults to a fresh fake; tests pass their own instance. |
| `wizard-state.ts` | `WizardState`: one plain object per run, never persisted (a reload starts over). |
| `wizard-context.tsx` | `WizardProvider`, `useWizard()`: the state, the current step, and `next`/`back`/`skip`/`goTo`. `patch` is stable (`useCallback`, no deps) — see the comment there for why that matters to any step whose effect both starts a stream and calls `patch`. |
| `steps.ts` | Which steps make up each wizard, in order (`stepsFor(mode)`). |
| `wizard-shell.tsx` | The stepper (a vertical Radix `Tabs.Root`, so arrow keys move between reached steps) and the current step's content. |
| `step-footer.tsx` | The Back / Skip / primary-action row every step ends with, inside a `<form onSubmit>` so Enter submits it. |
| `machine-target-picker.tsx` | This computer, a WSL distro, or an SSH host from `discoverHosts()` — never a free-typed host. |
| `steps/*.tsx` | One component per step. The add-a-machine wizard reuses `workspace-step.tsx`, `machine-check-step.tsx`, `install-helper-step.tsx`, `sign-in-step.tsx`, `scan-step.tsx`, `create-step.tsx` and `import-step.tsx`; each reads `useWizard().mode` where its copy or fields differ. |
| `first-run-page.tsx`, `add-machine-page.tsx` | The two routes' components (`routes.tsx`), lazy-loaded. |
| `*.test.ts(x)` | Vitest and Testing Library, against the fake. See "Running this stream's tests" below. |

## The `OnboardingApi` contract (proposed)

None of this exists on the hub yet (`docs/build/contracts/api-v1.md` has no onboarding routes: no
machine check, no helper install, no scan, no CLI sign-in, and no way to create a project or
workstream from the UI at all — `POST /v1/projects` doesn't exist either). Every method below is
this stream's proposal for what the real routes should look like; `fake-api.ts` is the only
implementation. Types are in `api.ts`, reusing `Engine`, `Project`, `Workstream` etc. from
`src/data` rather than redeclaring them, since the real routes would return the same objects.

| Method | Shape | Notes |
|---|---|---|
| `discoverHosts()` | `() → DiscoveredHost[]` | WSL distros (`wsl -l`) and SSH hosts (the user's own `~/.ssh/config`). Never a free-typed host (ADR-0009: deploy is over the user's own SSH connection). |
| `setupWorkspace(input)` | `{ name, primaryMachine } → { workspace: { id, name } }` | Names the workspace and its primary machine. Idempotent. **Not in the brief's list** — added because work package 1's "first workspace" step needs somewhere to land; see "Open question" below. |
| `checkMachine(target)` | `MachineTarget → MachineCheckResult` | CLI versions, tmux, git, gh, disk, and SLURM (SSH targets only). Each row: `status`, `detail`, `fixable`. |
| `fixMachineRow(target, row)` | `(MachineTarget, CheckRowId) → MachineCheckRow` | Rejects if the row is not `fixable`. |
| `launcherOptions(target)` | `MachineTarget → LauncherOption[]` | Which of `direct` / `tmux` / `systemd-user` / `slurm` this machine supports, and which is recommended (ADR-0009's "default detected"). |
| `streamInstallHelper(options, onEvent)` | `(InstallHelperOptions, cb) → Streamed` | Streams `log` lines; `slurm` first sends `script-preview` with the **exact** script, before anything is submitted. Ends with `done` or `error`. `Streamed.cancel()` stops it (a step unmounting mid-install). |
| `agentAccounts()` | `() → AgentAccount[]` | One row per engine: `signedIn`, and the account label if so. |
| `startSignIn(engine, machine)` | `(Engine, MachineTarget) → { terminalSessionId }` | Opens the CLI's own login **in a terminal on that machine** (ADR-0010: PitCrew never reads or copies its OAuth tokens). The id is meant to open in the Agent console (stream M); until that's registered, the wizard links to the shell's placeholder session page. |
| `integrationStatus()` | `() → IntegrationStatus[]` | GitHub, Jira, Linear, GitLab. Stream G owns the real connections. |
| `streamScan(target, onEvent)` | `(ScanTarget, cb) → Streamed` | Streams `progress`, ends with `done` carrying counts (by engine, folder, month) and suggested projects/workstreams. |
| `createFromScan(selection)` | `ProjectSelection[] → { projects: Project[], workstreams: Workstream[] }` | What the "Create" step submits after the user ticks, renames and regroups. **Not in `api-v1.md` at all**: there is no `POST /v1/projects` or `POST /v1/workstreams` today. |
| `importSessions(filter)` | `ImportFilter → { count }` | A dry run: counts what the filter would import without importing anything. |
| `commitImport(filter)` | `ImportFilter → { imported }` | Commits it. Sessions are read in place and never moved (ADR-0010); reversible (`link_basis: "imported"` can be unlinked later — that unlink route doesn't exist yet either). |
| `hooksDiff()` | `() → HooksDiff` | Every file the hooks would touch, before or after. Shown before `installHooks()` runs (never without the user seeing the diff first, per the stream's "Do not" rule). |
| `installHooks()` | `() → void` | Hooks themselves stay fire-and-forget and under 10 ms (ADR-0010); this only writes the CLI config that calls out to the daemon. |
| `saveSafety(settings)` | `SafetySettings → void` | `PermissionMode` is this stream's proposed enum for `StartSession.permission_mode`, which `api-v1.md` currently types as an untyped string. Suggest promoting it to `crates/protocol` so the real contract and the UI share one type. |

### Open question for the integrator: is there a workspace to create at all?

`GET /v1/workspace` (singular, existing contract) always returns exactly one `Workspace` — there is
no list and no creation route. That reads as: installing `pitcrewd` **is** creating the hub's one
workspace, and "first workspace" in the wizard is really just **naming** it and picking its primary
machine, not creating one from nothing. `setupWorkspace` above is written on that assumption. If
that's wrong — if a desktop can ever face more than one workspace, or none yet — the contract
needs an explicit create route and this wizard's step 2 needs to call it instead.

### Why the first-run wizard is not at a pre-workspace `/onboarding`

The brief asks for routes "`/w/$ws/onboarding/...` and `/onboarding` (first run, before a
workspace exists)". The shell's `Feature.routes(parent)` (`src/shell/feature.ts`) only ever
receives the **workspace** route as `parent` — there is no hook for a route outside `/w/$ws`, and
`OpenWorkspace` (`src/shell/pages/open.tsx`) waits on `GET /v1/workspace` with no "no workspace
yet" branch to redirect from. Given the point above (a workspace always exists once the hub is up),
a pre-workspace route may not even be needed in practice. Both wizards are registered at
`/w/$ws/onboarding` and `/w/$ws/onboarding/add-machine` instead. If product intent still wants a
true pre-workspace `/onboarding` (e.g. for a future multi-workspace desktop), the shell needs either:
- an optional `Feature.rootRoutes?: (root: RootRoute) => AnyRoute[]`, composed in `routes.tsx`
  alongside the workspace-scoped ones; or
- a "no workspace" branch in `OpenWorkspace` that redirects there instead of showing "Connecting…"
  forever.

That's a change to `src/shell/**`, outside this stream's paths — flagged here rather than made.

## Running this stream's tests

Vitest's `include` in `apps/ui/vitest.config.ts` (owned by stream L) is `['tests/**/*.test.{ts,tsx}']`,
so `pnpm test` does not pick up tests colocated under `src/onboarding/**`. Rather than touch a file
outside this stream's paths, there's a second, scoped config here:

```
corepack pnpm --filter @pitcrew/ui exec vitest run --config src/onboarding/vitest.config.ts
```

For CI to run these too, stream L (or the integrator) should add `'src/**/*.test.{ts,tsx}'` to the
root config's `include` — at which point this file's own config becomes redundant and can go.

The same gap exists for Playwright: `apps/ui/playwright.config.ts` (also L's) has `testDir: 'e2e'`,
and `apps/ui/e2e/**` is L's path too. The onboarding end-to-end spec lives at
`src/onboarding/e2e/onboarding.e2e.spec.ts`, with its own `src/onboarding/e2e/playwright.config.ts`
(same ports and webServer shape as the root one, so it is a drop-in once moved). Run it with:

```
corepack pnpm --filter @pitcrew/ui exec playwright test --config src/onboarding/e2e/playwright.config.ts
```

## Registering the feature

`index.ts` registers `onboarding`'s routes (`routes.tsx`) and the "Add a machine" palette command.
No nav entries: the wizards are flows you're dropped into, not pages you navigate to and from.
