# onboarding (stream O)

The first-run wizard, and connecting a remote machine in the desktop app. See
`docs/build/streams/O.md`, `docs/build/contracts/api-v1.md` ("The first run: `POST /v1/setup`", "Machine scan") and
`docs/build/contracts/desktop-gateway.md` ("Remote workspaces", "Prompts").

| File | What |
|---|---|
| `api.ts` | The `OnboardingApi` contract (below): every call the first-run wizard makes, typed, with `unavailable` (the calls with no backend yet) and `SetupRefused` (why setup was refused, by field). |
| `hub-api.ts` | `createHubOnboardingApi({ setUp, remote, data })`: the real one. `setupWorkspace` is `POST /v1/setup` (the data layer's `setUp`, through the workspace's own transport); `discoverHosts` is the gateway's `sshHosts`; `streamScan` and `createFromScan` are the scan and creating from it, through the workspace's client (`data`, `useApi()`; without it, unavailable); hooks use server-held previews and revision confirmation; safety reads/saves authored workspace preferences. Other unfinished calls are unavailable. |
| `scan-wire.ts` | The scan on the wire (`ScanFrame`, `ScanReport`, snake_case, as `pitcrew_protocol::scan` has them), `parseScanFrames` for its newline-delimited answer, and `toScanResult`, the mapping to this feature's `ScanResult` (camelCase, `byEngine` as a record). |
| `project-key.ts` | `projectKeyFor(name, taken)`: a new project's key from its name, unique in the workspace (see "Creating from the scan"). |
| `fake-api.ts` | `createFakeOnboardingApi()`: an in-memory implementation of every call that behaves plausibly (streamed progress, a fixable row, synthetic scan suggestions), refusing a bad setup as the hub would. For tests and a development flag only (below). |
| `validation.ts` | The forms' checks, mirroring the contracts: setup (trimmed names counted in code points, the handle's shape, no control characters), `suggestHandle`, `fieldOfMessage` (which field a hub `400` names), a typed SSH host, SLURM job options. |
| `setup-form.tsx` | `SetupForm`: the workspace's name, your name and handle (suggested from your name until you edit it), and the machine's name. Used by the first run and by the connect wizard. |
| `api-context.tsx` | `OnboardingApiProvider`, `useOnboardingApi()`. No default: nothing falls back to the fake. |
| `wizard-state.ts` | `WizardState`: one plain object per run, never persisted (a reload starts over). |
| `wizard-context.tsx` | `WizardProvider`, `useWizard()`: the state, the steps (`stepsFor(api)`), the current step, and `next`/`back`/`skip`/`goTo`. `patch` is stable (`useCallback`, no deps) — see the comment there for why that matters to any step whose effect both starts a stream and calls `patch`. |
| `steps.ts` | The first-run steps, in order, and the calls each needs: `stepsFor(api, draft)` leaves out a step whose calls are unavailable, and the optional draft step unless `draft` (below). |
| `wizard-shell.tsx` | The stepper (a vertical Radix `Tabs.Root`, so arrow keys move between reached steps) and the current step's content. |
| `step-footer.tsx` | The Back / Skip / primary-action row every step ends with, inside a `<form onSubmit>` so Enter submits it. |
| `steps/*.tsx` | One component per step. `workspace-step.tsx` is the setup form; once the hub has taken it, going Back only shows what was set, and never sends it again. |
| `first-run-page.tsx`, `fake-first-run.tsx` | The first-run route's component (lazy): the real API, or in a development build with `?onboarding=fake`, the fake. |
| `connect/connect-page.tsx`, `connect/connect-wizard.tsx` | `/connect`: connecting a remote machine (desktop only; a browser is told it cannot). |
| `routes.tsx` | `/w/$ws/onboarding` (`paths.setup`, `staticData.setup`) and the root route `/connect` (`Feature.rootRoutes`). Both lazy. |
| `*.test.ts(x)`, `connect/*.test.tsx` | Vitest and Testing Library: the fake wizard, the real first run against a stand-in `setUp` and data client (`scan-fixture.ts`'s synthetic report), the scan's mapping and the key rule, the checks, and the connect wizard in the whole app against a mocked gateway (`src/data/tests/fake-desktop.ts`). |

## The first run

A fresh hub answers `GET /v1/workspace` with `setup_needed: true`, and the shell sends the
workspace to `paths.setup(ws)`, this feature's first-run wizard, from any page (see
`src/shell/README.md`, "The first run"). Against the real hub the wizard is **Welcome, Workspace,
Scan, Create, Import, Hooks, Safety, Done**, then Home (with **Draft boards** after Import when the
run has something to draft; see "Drafting the boards from history"):

- **Workspace** is `POST /v1/setup`: the workspace's name, your name, your handle (suggested from
  the first word of your name, `Sam Rivera` → `@sam`, until you type one), and this machine's name
  (`This computer` until you give another: the webview cannot read the host name). A remote hub
  sent here by the redirect is a remote machine: the field says so, and starts from the gateway's
  name for it.
- **Validation mirrors the contract** before anything is sent: names are trimmed (as JavaScript's
  `trim` does, which the hub matches) and counted in Unicode code points (1–80, 1–80, 1–60), the
  handle is `@` and 1–32 of `a-z 0-9 _ -` (not trimmed, and never `@office`, the back office's),
  and nothing may hold a control character. Each problem shows by its field, which gets focus.
- **The hub's refusals** show by the right field too: a `400` by the field its message names
  (otherwise above the buttons), a `409` for a taken handle by the handle. A `409` because the
  workspace was set up meanwhile goes Home: the data layer has already read the workspace again,
  so the shell does not send it back.
- **Scan** is `POST /v1/machines/{id}/scan` on the hub's own machine (the first `local` one in
  `GET /v1/machines`): what agent sessions it has, counted by engine and folder, and the projects
  and workstreams they suggest. A refusal (a scan already running, the hub out of reach) says why,
  with **Try again**. A finished scan is shown again, not repeated, when you come back to the
  step (it would reset Create's choices); **Scan again** asks for one. Today's transports hand
  over the whole answer at once, so the step says it is scanning until the report arrives; the
  answer's progress frames show as they come once the data layer and the gateway can stream a
  request (see `hub-api.ts`). The hub's walk cannot be stopped part-way, so `streamScan` sends
  nothing for a scan cancelled before its request went out (StrictMode's first mount).
- **Create** is `POST /v1/projects` and `POST /v1/workstreams` for what stayed ticked (see
  "Creating from the scan"). A failure says why and keeps your choices; pressing Create again
  creates only what is still missing. Skipping creates nothing.
- **Done** goes Home, replacing the wizard in the history. The data layer turned `setup_needed`
  off in the cache when setup succeeded, so there is no loop.
- A workspace that is set up already, opened at `/onboarding`, goes Home at once, unless this
  visit is the one that set it up (its Done step is still to come) or the development flag asks
  for the fake wizard.

The other steps (machine check, helper install, sign-in, integrations, import, hooks, safety) need
routes that do not exist yet, so `createHubOnboardingApi` lists their calls in `unavailable` and
`stepsFor` leaves them out. They come back, unchanged, as their routes land. A remote workspace
(the desktop's gateway says `kind: 'remote'`) gets no data client, so its first run, like the
connect wizard's setup of one, stays Welcome, Workspace, Done: its hub's own machine is the remote
one, and scanning a remote machine is a later step.

### Creating from the scan

`ProjectSelection` names suggestions, so `createFromScan` looks each one up by `suggestionId` in
the last scan that finished (a selection the scan did not have is refused: scan again):

- **A project** is `POST /v1/projects` with the name you gave (the suggestion's if you cleared it),
  its `root` at the suggestion's `path` on the scanned machine, and a **key** derived from the name
  (`project-key.ts`): the initials of its words, at most four, or the first three characters of a
  one-word name (`Diffusion study` → `DS`, `paper` → `PAP`), upper-case ASCII with accents
  dropped and leading digits skipped (`PRJ` when nothing is left). It is made unique against the
  workspace's keys (`GET /v1/projects`) and the batch's own, with the smallest free number from 2
  (`PAP2`); a `409` from the hub (another client took it meanwhile) tries the next.
- **A workstream** is `POST /v1/workstreams` in its (possibly moved) project, with one location: a
  sub-folder suggestion's own folder (its `id` is its path), or a branch suggestion's project root
  on that branch. A moved workstream keeps its place.
- The template (Research, Software, Blank) is not sent: the contract has no templates yet.
- What one run created is remembered, so pressing Create again after a failure part-way creates
  only the rest.

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
   is checked first against an allow-list: a name of letters, digits, `.`, `_` and `-`, optionally
   after `user@`, or an IPv6 address in brackets, with no leading `-` in the user or the host (ssh
   would read it as an option). Nothing else: no spaces, quotes, `%` tokens, shell characters, or
   invisible and direction-changing characters.
2. **Probe** (`remoteProbe`): the OS and architecture, whether PitCrew is there and running,
   SLURM's version and default partition, and tmux's version when the gateway gives it.
3. **Launcher**: `direct`, `tmux` or `slurm`. SLURM only where the probe found it; tmux only from
   3.2 (an older one is off, with the reason; an unknown one is offered, with a note that the plan
   says so if not). For SLURM: the site recipe (generic by default), partition (the default to
   start from), account, QoS, time (`08:00:00`, `2-00:00:00`), CPUs, memory (`16G`) and GPUs
   (`2`, `a100:2`).
4. **Review** (`remotePlan`): the plan's steps and, for SLURM, the exact `jobScript`, verbatim, in a
   monospace block, with a warning when it holds direction-changing or zero-width characters or
   carriage returns (what you read may not be what runs). "Nothing changes on the remote until you
   press Connect." A plan the gateway refuses shows its message.
5. **Connect** (`remoteAdd`): every step of the plan, each with its latest state and detail ("40%
   sent", "job 4242 pending (Priority)"); the last message, `add`, is the whole add's outcome. A
   SLURM job can wait in the queue for minutes: the wizard waits too, with no time limit of its
   own. Meanwhile:
   - **Leave it running** goes back to PitCrew; the add goes on in the gateway, and its workspace
     shows up in the switcher when it is ready;
   - **Stop connecting…** asks first, then calls `gateway_remote_cancel({ plan })`, which undoes
     what the add started; a gateway without the command says so, and the add goes on.
   A failure shows its step and detail (worked out from the progress, else the whole add's), and
   "Back to review" gets a fresh plan (a plan is used once). SSH's questions arrive meanwhile
   through the shell's prompt dialog.
6. **Setup**: if the new workspace has `setup_needed`, the same setup form, against that workspace
   through its own gateway transport (in its data scope). **Set it up later** leaves it: opening
   the workspace later leads to its first run.
7. **Done**: opens the new workspace.

**A plan is never submitted without being shown.** Connect sends the plan on screen. When the
gateway refuses it as `invalid` before anything started (expired after 10 minutes, or already
used), the wizard goes back to Review with a fresh plan and says why, and only another Connect
sends it. A launch refused once the add is under way is a failure like any other: its step and
detail, and "Back to review".

Every remote call starts from a button press, never from an effect, so React's development
double-mount cannot make one twice (two password prompts, or one plan submitted twice). A late
answer for a step already left is dropped.

## The `OnboardingApi` contract

`setupWorkspace`, `discoverHosts`, `streamScan` and `createFromScan` are real. Everything else is
this stream's proposal for what the real routes should look like (`api-v1.md` has no machine
check, helper install, CLI sign-in, hooks or safety routes yet); `fake-api.ts` is their only
implementation. Types are in `api.ts`, reusing `Engine`, `Project`, `Workstream` etc. from
`src/data`; the scan's wire types, which the data layer does not declare, are in `scan-wire.ts`.

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
| `streamScan(target, onEvent)` | `(ScanTarget, cb) → Streamed` | **Real**: `POST /v1/machines/{id}/scan` on the hub's own machine. Streams `progress`, ends with `done` carrying counts and suggested projects/workstreams, or `error` with why. |
| `createFromScan(selection)` | `ProjectSelection[] → { projects, workstreams }` | **Real**: `POST /v1/projects` and `POST /v1/workstreams` from the last scan's suggestions (see "Creating from the scan"). |
| `importSessions(filter)` / `commitImport(filter)` | `ImportFilter → { count }` / `{ imported }` | Proposed. A dry run, then the import (sessions read in place, never moved). |
| `hooksDiff()` / `installHooks()` | `() → HooksDiff` / `() → void` | Proposed. The diff is always shown before installing. |
| `saveSafety(settings)` | `SafetySettings → void` | Proposed. `PermissionMode` should be promoted to `crates/protocol`. |

**Still proposed, for the integrator:** the machine check, CLI sign-in and the hooks
APIs (and the helper install, integrations, import and safety calls with them). The old
"Add a machine" wizard and its palette command are retired: its machine-picker-led flow ran only
against the fake, and connecting a remote hub is now the connect wizard above. Adding a machine to
an existing hub (a runner reporting to it) comes back when those routes land.

## Drafting the boards from history (optional)

After Import, the real first run can offer **Draft boards** (`steps/draft-step.tsx`,
`draft-board.tsx`; brief 0-draft-board, api-v1.md "Board drafts"): for each workstream created in
this run, what would be sent to an agent the person already uses (no API key: its own CLI), its
size and the agent's estimated usage, and a start the person confirms per workstream. Proposals
are reviewed later on each workstream's page ("Draft board"); nothing is created until then.

The step goes through the data layer (`../projects/board-drafts.ts`), not `OnboardingApi`, so
`api.ts` and `hub-api.ts` are unchanged. It is in the stepper (`stepsFor(api, draft)`, from
`WizardProvider`) only when `DraftStepProvider` is around the wizard (the hub's first run puts it
there; the fake and the other tests do not) and the run has something to draft (`showDraftStep`:
workstreams were created, and the import kept some sessions, or was not made). A first run that
imports nothing, as against a fresh hub, has no such step, so the sequence stays Welcome,
Workspace, Scan, Create, Import, Hooks, Safety, Done; with it, Draft boards comes between Import
and Hooks, as `docs/build/streams/O.md` orders them.

## Running this stream's tests

```
corepack pnpm --filter @pitcrew/ui test
```

End to end, from `apps/ui`: the demo-mode suite (`corepack pnpm --filter @pitcrew/ui e2e`)
includes `e2e/onboarding-fake.spec.ts`, the whole fake wizard with axe, light and dark. The first
run against a fresh hub, and the connect wizard and the prompt dialog in a simulated desktop, need
fresh mock hubs (`PITCREW_MOCK_FRESH=1`) and run with their own config. The first run there is
Welcome, Workspace, Scan (the mock's synthetic report), Create and Done, with axe in both
themes:

```
corepack pnpm --filter @pitcrew/ui exec playwright test --config e2e/fresh.config.ts
```

The real first run now includes Import after Create. It previews the indexed-session count for all sessions, a UTC date/engine/folder filter, or start fresh, then stores the choice with PUT. Requests that fail display an error and keep confirmation disabled until a successful count. Sessions stay in place; widening the hub choice is reversible via `/v1/import`.

Hooks are previewed file by file on the hub’s own machine. Only **Install hooks** confirms the displayed revision; concurrent edits are refused and **Refresh diff** fetches a new preview. Skipping writes nothing. Remote machines within that workspace remain unsupported. Safety loads saved preferences before editing, warns for **Skip permissions**, and saves permission defaults and the back-office hourly acceptance budget. Read/save failures stay on the step.

Onboarding review: hook previews detect supported CLIs on PATH or through their
homes, skip conflicting engines while applying other changes, and report the
skipped engines. No-change previews cannot set the wizard's installed flag.
Desktop packages include the hook CLI beside the daemon. Safety uses snake_case
wire fields and the shared PermissionMode enum; bypass defaults are currently
refused. Unsaved safety reports `saved: false` for legacy per-task acceptance.
