# onboarding (stream O)

The first-run wizard, and connecting a remote machine in the desktop app. See
`docs/build/streams/O.md`, `docs/build/contracts/api-v1.md` ("The first run: `POST /v1/setup`") and
`docs/build/contracts/desktop-gateway.md` ("Remote workspaces", "Prompts").

| File | What |
|---|---|
| `api.ts` | The `OnboardingApi` contract (below): every call the first-run wizard makes, typed, with `unavailable` (the calls with no backend yet) and `SetupRefused` (why setup was refused, by field). |
| `hub-api.ts` | `createHubOnboardingApi({ setUp, remote })`: the real one. `setupWorkspace` is `POST /v1/setup` (the data layer's `setUp`, through the workspace's own transport); `discoverHosts` is the gateway's `sshHosts`; every other call is unavailable. |
| `fake-api.ts` | `createFakeOnboardingApi()`: an in-memory implementation of every call that behaves plausibly (streamed progress, a fixable row, synthetic scan suggestions), refusing a bad setup as the hub would. For tests and a development flag only (below). |
| `validation.ts` | The forms' checks, mirroring the contracts: setup (trimmed names counted in code points, the handle's shape, no control characters), `suggestHandle`, `fieldOfMessage` (which field a hub `400` names), a typed SSH host, SLURM job options. |
| `setup-form.tsx` | `SetupForm`: the workspace's name, your name and handle (suggested from your name until you edit it), and the machine's name. Used by the first run and by the connect wizard. |
| `api-context.tsx` | `OnboardingApiProvider`, `useOnboardingApi()`. No default: nothing falls back to the fake. |
| `wizard-state.ts` | `WizardState`: one plain object per run, never persisted (a reload starts over). |
| `wizard-context.tsx` | `WizardProvider`, `useWizard()`: the state, the steps (`stepsFor(api)`), the current step, and `next`/`back`/`skip`/`goTo`. `patch` is stable (`useCallback`, no deps) — see the comment there for why that matters to any step whose effect both starts a stream and calls `patch`. |
| `steps.ts` | The first-run steps, in order, and the calls each needs: `stepsFor(api)` leaves out a step whose calls are unavailable. |
| `wizard-shell.tsx` | The stepper (a vertical Radix `Tabs.Root`, so arrow keys move between reached steps) and the current step's content. |
| `step-footer.tsx` | The Back / Skip / primary-action row every step ends with, inside a `<form onSubmit>` so Enter submits it. |
| `steps/*.tsx` | One component per step. `workspace-step.tsx` is the setup form; once the hub has taken it, going Back only shows what was set, and never sends it again. |
| `first-run-page.tsx`, `fake-first-run.tsx` | The first-run route's component (lazy): the real API, or in a development build with `?onboarding=fake`, the fake. |
| `connect/connect-page.tsx`, `connect/connect-wizard.tsx` | `/connect`: connecting a remote machine (desktop only; a browser is told it cannot). |
| `routes.tsx` | `/w/$ws/onboarding` (`paths.setup`, `staticData.setup`) and the root route `/connect` (`Feature.rootRoutes`). Both lazy. |
| `*.test.ts(x)`, `connect/*.test.tsx` | Vitest and Testing Library: the fake wizard, the real first run against a stand-in `setUp`, the checks, and the connect wizard in the whole app against a mocked gateway (`src/data/tests/fake-desktop.ts`). |

## The first run

A fresh hub answers `GET /v1/workspace` with `setup_needed: true`, and the shell sends the
workspace to `paths.setup(ws)`, this feature's first-run wizard, from any page (see
`src/shell/README.md`, "The first run"). Against the real hub the wizard is **Welcome, Workspace,
Done**, then Home:

- **Workspace** is `POST /v1/setup`: the workspace's name, your name, your handle (suggested from
  the first word of your name, `Sam Rivera` → `@sam`, until you type one), and this machine's name
  (`This computer` until you give another: the webview cannot read the host name).
- **Validation mirrors the contract** before anything is sent: names are trimmed and counted in
  Unicode code points (1–80, 1–80, 1–60), the handle is `@` and 1–32 of `a-z 0-9 _ -` (not
  trimmed), and nothing may hold a control character. Each problem shows by its field, which gets
  focus.
- **The hub's refusals** show by the right field too: a `400` by the field its message names
  (otherwise above the buttons), a `409` for a taken handle by the handle. A `409` because the
  workspace was set up meanwhile goes Home: the data layer has already read the workspace again,
  so the shell does not send it back.
- **Done** goes Home. The data layer turned `setup_needed` off in the cache when setup succeeded,
  so there is no loop.

The other steps (machine check, helper install, sign-in, integrations, scan, create, import, hooks,
safety) need routes that do not exist yet, so `createHubOnboardingApi` lists their calls in
`unavailable` and `stepsFor` leaves them out. They come back, unchanged, as their routes land.

### The fake: tests, and a development flag

The fake implements every call. Tests use it directly. In a **development build**,
`/w/$ws/onboarding?onboarding=fake` runs the whole twelve-step wizard against it (its setup reaches
no hub, so a fresh hub stays fresh). A **production build never uses it**: `first-run-page.tsx`
loads it only behind `import.meta.env.DEV`, which the bundler folds to `false`, dropping the branch
and the fake's chunk.

## Connecting a remote machine (desktop only)

`/connect` (`paths.connect()`), opened from the switcher's "Connect a remote machine…" or the empty
desktop's "No workspaces yet" screen. It is a root route (`Feature.rootRoutes`), since it also runs
before any workspace exists. It drives the gateway's remote commands:

1. **Host**: one from your ssh config (`discoverHosts`, the gateway's `sshHosts`), or typed. A host
   is checked first: no leading `-` (ssh would read it as an option), no whitespace or control
   characters.
2. **Probe** (`remoteProbe`): the OS and architecture, whether PitCrew is there and running, and
   SLURM's version and default partition.
3. **Launcher**: `direct`, `tmux` or `slurm` (only where the probe found SLURM). For SLURM: the site
   recipe, partition (the default to start from), account, QoS, time, CPUs, memory and GPUs.
4. **Review** (`remotePlan`): the plan's steps and, for SLURM, the exact `jobScript`, verbatim, in a
   monospace block. "Nothing changes on the remote until you press Connect."
5. **Connect** (`remoteAdd`): progress, step by step. A failure shows its step and detail, and
   "Back to review" gets a fresh plan (a plan is used once). SSH's questions arrive meanwhile
   through the shell's prompt dialog.
6. **Setup**: if the new workspace has `setup_needed`, the same setup form, against that workspace
   through its own gateway transport (in its data scope).
7. **Done**: opens the new workspace.

**A plan is never submitted without being shown.** Connect sends the plan on screen. When the
gateway refuses it as `invalid` (expired after 10 minutes, already used, or a launch it refused),
the wizard goes back to Review with a fresh plan and says why, and only another Connect sends it.

Every remote call starts from a button press, never from an effect, so React's development
double-mount cannot make one twice (two password prompts, or one plan submitted twice). A late
answer for a step already left is dropped.

## The `OnboardingApi` contract

`setupWorkspace` and `discoverHosts` are real. Everything else is this stream's proposal for what
the real routes should look like (`api-v1.md` has no machine check, helper install, scan, CLI
sign-in, hooks or safety routes yet); `fake-api.ts` is their only implementation. Types are in
`api.ts`, reusing `Engine`, `Project`, `Workstream` etc. from `src/data`.

| Method | Shape | Notes |
|---|---|---|
| `unavailable` | `ReadonlySet<OnboardingCall>` | The calls with no backend: they reject, and their steps are left out. Empty in the fake. |
| `discoverHosts()` | `() → DiscoveredHost[]` | **Real** in the desktop: the gateway's `sshHosts`, as `{ kind: 'ssh', id }`. The fake adds WSL distros. |
| `setupWorkspace(input)` | `{ workspaceName, person: { name, handle }, machineName } → { workspace, me }` | **Real**: `POST /v1/setup`. Rejects with `SetupRefused` (`field`, or `alreadySetUp`). |
| `checkMachine(target)` | `MachineTarget → MachineCheckResult` | Proposed. CLI versions, tmux, git, gh, disk, and SLURM (SSH targets only). Each row: `status`, `detail`, `fixable`. |
| `fixMachineRow(target, row)` | `(MachineTarget, CheckRowId) → MachineCheckRow` | Proposed. Rejects if the row is not `fixable`. |
| `launcherOptions(target)` | `MachineTarget → LauncherOption[]` | Proposed. Which of `direct` / `tmux` / `systemd-user` / `slurm` this machine supports, and which is recommended. |
| `streamInstallHelper(options, onEvent)` | `(InstallHelperOptions, cb) → Streamed` | Proposed. Streams `log` lines; `slurm` first sends `script-preview` with the **exact** script. Ends with `done` or `error`. `Streamed.cancel()` must stop the **server-side** deploy. |
| `agentAccounts()` | `() → AgentAccount[]` | Proposed. One row per engine. |
| `startSignIn(engine, machine)` | `(Engine, MachineTarget) → { terminalSessionId }` | Proposed. Opens the CLI's own login in a terminal on that machine (ADR-0010: PitCrew never reads its tokens). |
| `integrationStatus()` | `() → IntegrationStatus[]` | Proposed. Stream G owns the real connections. |
| `streamScan(target, onEvent)` | `(ScanTarget, cb) → Streamed` | Proposed. Streams `progress`, ends with `done` carrying counts and suggested projects/workstreams. |
| `createFromScan(selection)` | `ProjectSelection[] → { projects, workstreams }` | Proposed. Could now be built on `POST /v1/projects` and `POST /v1/workstreams`. |
| `importSessions(filter)` / `commitImport(filter)` | `ImportFilter → { count }` / `{ imported }` | Proposed. A dry run, then the import (sessions read in place, never moved). |
| `hooksDiff()` / `installHooks()` | `() → HooksDiff` / `() → void` | Proposed. The diff is always shown before installing. |
| `saveSafety(settings)` | `SafetySettings → void` | Proposed. `PermissionMode` should be promoted to `crates/protocol`. |

**Still proposed, for the integrator:** the machine check, the scan, CLI sign-in and the hooks
APIs (and the helper install, integrations, import and safety calls with them). The old
"Add a machine" wizard and its palette command are retired: its machine-picker-led flow ran only
against the fake, and connecting a remote hub is now the connect wizard above. Adding a machine to
an existing hub (a runner reporting to it) comes back when those routes land.

## Running this stream's tests

```
corepack pnpm --filter @pitcrew/ui test
```

End to end, from `apps/ui`: the demo-mode suite (`corepack pnpm --filter @pitcrew/ui e2e`)
includes `e2e/onboarding-fake.spec.ts`, the whole fake wizard with axe, light and dark. The first
run against a fresh hub, and the connect wizard and the prompt dialog in a simulated desktop, need
fresh mock hubs (`PITCREW_MOCK_FRESH=1`) and run with their own config:

```
corepack pnpm --filter @pitcrew/ui exec playwright test --config e2e/fresh.config.ts
```
