// The onboarding wizards' backend contract. Setup (`POST /v1/setup`), the host list
// (`gateway_ssh_hosts`), the machine check, the agents' accounts and sign-in (api-v1.md, "Machine
// setup"), installing the helper on a remote machine (the gateway's plan and add), the scan
// (`POST /v1/machines/{id}/scan`), creating from it (`POST /v1/projects`, `POST /v1/workstreams`)
// and the import are real (`hub-api.ts`); the other routes do not exist on the hub yet (see
// README.md for the proposed shapes), and `createFakeOnboardingApi` in `fake-api.ts` is their only
// implementation. Every step is built against the `OnboardingApi`
// interface below, so a real call replaces a fake one without touching a step component.
//
// Reuses wire types from `../data` (`Engine`, `Machine`, `Project`, `Workstream`, …) rather than
// redeclaring them, since the real routes below would return the same objects.

import type { Engine, MachineKind, Project, Workstream } from '../data/index.ts';
import type { SetupField } from './validation.ts';

/** A machine to check, install on, sign into or scan: not yet a registered `Machine`. */
export type MachineTarget =
  | { kind: 'local' }
  | { kind: 'wsl'; distro: string }
  | { kind: 'ssh'; host: string; user?: string };

export function machineTargetLabel(target: MachineTarget): string {
  if (target.kind === 'local') return 'This computer';
  if (target.kind === 'wsl') return `WSL: ${target.distro}`;
  return target.user === undefined ? target.host : `${target.user}@${target.host}`;
}

/**
 * A stable string key for caching per-machine data (a check, launcher options, …) in wizard
 * state, keyed by target so revisiting a step after Back/Forward reuses what it already fetched
 * instead of refetching and silently overwriting a choice the person already made.
 */
export function targetKey(target: MachineTarget): string {
  if (target.kind === 'local') return 'local';
  if (target.kind === 'wsl') return `wsl:${target.distro}`;
  return `ssh:${target.user ?? ''}@${target.host}`;
}

/** An SSH host offered from the user's own `~/.ssh/config`, or a WSL distro from `wsl -l`. */
export interface DiscoveredHost {
  kind: Extract<MachineKind, 'wsl' | 'ssh'>;
  /** The distro name (`wsl`) or the `Host` alias (`ssh`). */
  id: string;
  detail?: string;
}

export type CheckRowId =
  | 'cli-claude'
  | 'cli-codex'
  | 'cli-opencode'
  | 'tmux'
  | 'git'
  | 'gh'
  | 'disk'
  | 'slurm'
  /** PitCrew's helper, in a check made over SSH before it is installed. */
  | 'helper';

export type CheckRowStatus = 'ok' | 'warn' | 'missing' | 'checking';

/**
 * What "Fix" does. Never installing a system package: `install-page` opens the tool's install page
 * (from `install-pages.ts`, this app's own table: no URL comes from the machine), and
 * `install-helper` is installing PitCrew's own helper, the connect wizard's next steps.
 */
export type CheckFix = 'install-page' | 'install-helper';

export interface MachineCheckRow {
  id: CheckRowId;
  label: string;
  status: CheckRowStatus;
  /** A version string, free space, or an error, shown under the label. */
  detail?: string;
  /** Whether "Fix" can do something about it (install a CLI, free disk space is not). */
  fixable: boolean;
  /** What "Fix" does, when `fixable`. */
  fix?: CheckFix | undefined;
}

export interface MachineCheckResult {
  machine: MachineTarget;
  rows: MachineCheckRow[];
  /**
   * Set, with no rows, when this machine is not checked here but later, as it is connected (an
   * SSH host, a WSL distro or an HPC login node, which the connect wizard checks over SSH): what to
   * tell the person instead of rows. Not an error.
   */
  deferred?: string | undefined;
}

export type Launcher = 'direct' | 'tmux' | 'systemd-user' | 'slurm';

export interface LauncherOption {
  launcher: Launcher;
  /** Whether setup detected this launcher would work here (ADR-0009's "default detected"). */
  recommended: boolean;
  /** Why it is unavailable, when it is. */
  unavailable?: string;
}

export interface InstallHelperOptions {
  machine: MachineTarget;
  launcher: Launcher;
  /**
   * The plan the person reviewed (the gateway's `remotePlan`): the real install carries out
   * exactly that plan, and refuses to start without one, so nothing is installed or submitted
   * unseen. The fake ignores it.
   */
  plan?: string | undefined;
}

/** One line of a live install log, or the terminal event that ends the stream. */
export type InstallProgressEvent =
  | { type: 'log'; line: string }
  /** The exact script to be submitted (`launcher: 'slurm'` only), shown before anything runs. */
  | { type: 'script-preview'; script: string }
  | { type: 'done' }
  | { type: 'error'; message: string };

/**
 * A cancellable streamed call: every `stream*` method on `OnboardingApi` returns one of these.
 *
 * `cancel()` must stop the **server-side** work (the scan walk, the helper deploy), not just
 * detach the listener — a real implementation that only stops delivering events but leaves the
 * scan or install running server-side will double the work and can double-submit a SLURM job.
 * React StrictMode's dev-only double-mount relies on this: it cancels the first call's stream
 * before starting the second, and the fake's `cancel()` clears its timers accordingly (see
 * `fake-api.ts`, `streamSteps`).
 *
 * The hub's scan is the exception: its walk cannot be stopped part-way, so it never runs twice
 * instead. A second scan of a machine while one runs is refused (an `error` event), and the real
 * `streamScan` sends nothing when cancelled before its request went out, as StrictMode's first
 * mount is.
 */
export interface Streamed {
  cancel(): void;
}

/** One agent CLI's account, as the CLI's own status command reports it (never from its files). */
export interface AgentAccount {
  engine: Engine;
  /** Whether the CLI is on the machine. */
  installed: boolean;
  /** What the CLI says; `undefined` when it could not tell (`detail` says why). */
  signedIn: boolean | undefined;
  /** The signed-in account's label (an e-mail address, `ChatGPT`, `API key`), when it says. */
  account?: string | undefined;
  /** Why `signedIn` is unknown, or why the CLI is not there. */
  detail?: string | undefined;
}

/** How a CLI's login proves who the person is: `device-code` is Codex's, for remote machines. */
export type SignInMethod = 'browser' | 'device-code';

export interface StartSignInResult {
  /** The sign-in terminal, which the console's terminal view shows (`/v1/sessions/{id}/terminal`). */
  terminalSessionId: string;
  /** The command it runs, for people (`claude auth login`). */
  command: string[];
}

export type IntegrationId = 'github' | 'jira' | 'linear' | 'gitlab';

export interface IntegrationStatus {
  id: IntegrationId;
  connected: boolean;
  detail?: string;
}

export interface ScanTarget {
  machine: MachineTarget;
}

export interface ScanCounts {
  byEngine: Partial<Record<Engine, number>>;
  byFolder: { path: string; count: number }[];
  /** `YYYY-MM`, most recent first. */
  byMonth: { month: string; count: number }[];
}

export interface SuggestedWorkstream {
  id: string;
  name: string;
  branch?: string;
  sessionCount: number;
}

export interface SuggestedProject {
  id: string;
  name: string;
  path: string;
  workstreams: SuggestedWorkstream[];
}

export interface ScanResult {
  counts: ScanCounts;
  suggestedProjects: SuggestedProject[];
}

export type ScanProgressEvent =
  | { type: 'progress'; scanned: number; total?: number; path?: string }
  | { type: 'done'; result: ScanResult }
  /** The scan could not run or failed (one already running, the hub unreachable…): the last event. */
  | { type: 'error'; message: string };

export type ProjectTemplate = 'research' | 'software' | 'blank';

/** What the "Create projects and workstreams" step submits after the user ticks and edits suggestions. */
export interface ProjectSelection {
  suggestionId: string;
  name: string;
  template: ProjectTemplate;
  workstreams: { suggestionId: string; name: string }[];
}

export interface CreateFromScanResult {
  projects: Project[];
  workstreams: Workstream[];
}

export type ImportMode = 'all' | 'filtered' | 'none';

export interface ImportFilter {
  mode: ImportMode;
  /** `YYYY-MM-DD`; sessions started before this are excluded. */
  since?: string;
  engines?: Engine[];
  folders?: string[];
}

export interface ImportDryRunResult {
  /** How many sessions this filter would import; read in place, never moved (ADR-0010). */
  count: number;
}

export interface ImportResult {
  imported: number;
}

export interface HooksDiffFile {
  path: string;
  /** `null` for a new file. */
  before: string | null;
  after: string;
}

export interface HooksDiff {
  revision: string;
  engines: { engine: string; status: string; detail: string }[];
  files: HooksDiffFile[];
}

/**
 * The CLI's own default until the user opts in to skipping permissions (ADR-0010). Not yet in
 * `docs/build/contracts/api-v1.md`'s `StartSession.permission_mode`, which is an untyped string
 * today; this is the proposed enum for it.
 */
export type PermissionMode = 'default' | 'plan' | 'accept-edits' | 'bypass-permissions';

export interface BackOfficeCaps {
  /** How many low-risk actions the back office may accept per hour before it asks a person. */
  maxAutoAcceptPerHour: number;
}

export interface SafetySettings {
  permissionMode: PermissionMode;
  backOfficeEnabled: boolean;
  backOfficeCaps: BackOfficeCaps;
}

/** `POST /v1/setup`'s body, as the form holds it (names already trimmed: `trimmedSetup`). */
export interface SetupWorkspaceInput {
  workspaceName: string;
  person: { name: string; handle: string };
  machineName: string;
}

export interface SetupWorkspaceResult {
  workspace: { id: string; name: string };
  me: { name: string; handle: string };
}

/**
 * Why setup was refused, for the form to show by the right field: a `400` names a field, a `409`
 * is either the handle (taken) or the whole workspace (`alreadySetUp`: someone finished first, so
 * there is nothing left to do but go Home).
 */
export class SetupRefused extends Error {
  readonly field: SetupField | undefined;
  readonly alreadySetUp: boolean;

  constructor(message: string, options: { field?: SetupField | undefined; alreadySetUp?: boolean } = {}) {
    super(message);
    this.name = 'SetupRefused';
    this.field = options.field;
    this.alreadySetUp = options.alreadySetUp ?? false;
  }
}

/** Every call; `unavailable` lists those with no backend yet. */
export type OnboardingCall = Exclude<keyof OnboardingApi, 'unavailable' | 'needsHelper'>;

/**
 * The onboarding wizards' backend contract (see README.md). Every method is called from a step
 * component through `useOnboardingApi()` (`api-context.tsx`). `hub-api.ts` is the real one (setup
 * and the host list so far); `fake-api.ts` implements all of it, for tests and a development flag.
 */
export interface OnboardingApi {
  /**
   * The calls with no backend yet: each rejects, and `stepsFor` leaves out the steps that need
   * one. They come back as their routes land.
   */
  readonly unavailable: ReadonlySet<OnboardingCall>;
  /** SSH hosts from the person's ssh config (and, in the fake, WSL distros), for a host picker. */
  discoverHosts(): Promise<DiscoveredHost[]>;
  /**
   * The first run (`POST /v1/setup`): names the workspace, its person and this machine. Once
   * only; rejects with a `SetupRefused` when the hub says no.
   */
  setupWorkspace(input: SetupWorkspaceInput): Promise<SetupWorkspaceResult>;

  checkMachine(target: MachineTarget): Promise<MachineCheckResult>;
  /** Re-checks one row after "Fix" runs. Rejects if `fixable` was false. */
  fixMachineRow(target: MachineTarget, row: CheckRowId): Promise<MachineCheckRow>;
  launcherOptions(target: MachineTarget): Promise<LauncherOption[]>;

  /** Streams progress; for `launcher: 'slurm'` the first event is the script preview. */
  streamInstallHelper(options: InstallHelperOptions, onEvent: (event: InstallProgressEvent) => void): Streamed;

  agentAccounts(): Promise<AgentAccount[]>;
  /** Opens the CLI's own login in a terminal on the target machine (never reads its tokens). */
  startSignIn(engine: Engine, machine: MachineTarget, method?: SignInMethod): Promise<StartSignInResult>;
  /** Whether that CLI's login still runs: once it has ended, `agentAccounts` asks the CLI again. */
  signInRunning(engine: Engine, machine: MachineTarget): Promise<boolean>;
  /**
   * Stops that CLI's sign-in and removes its terminal, running or ended (the person left it or
   * skipped it): no login, and no Codex callback listener, is left running. Resolves when there is
   * none.
   */
  stopSignIn(engine: Engine, machine: MachineTarget): Promise<void>;

  /**
   * Whether PitCrew's helper must be installed on `target` before it can be used. Not on the hub's
   * own machine (`local`), which runs the hub itself; `stepsFor` leaves the install step out then.
   */
  needsHelper(target: MachineTarget): boolean;

  integrationStatus(): Promise<IntegrationStatus[]>;

  streamScan(target: ScanTarget, onEvent: (event: ScanProgressEvent) => void): Streamed;

  createFromScan(selection: ProjectSelection[]): Promise<CreateFromScanResult>;

  /** A dry run: counts what the filter would import without importing anything. */
  importSessions(filter: ImportFilter): Promise<ImportDryRunResult>;
  /** Stores reversible inclusion rules; no session or transcript is deleted. */
  commitImport(filter: ImportFilter): Promise<ImportResult>;

  hooksDiff(): Promise<HooksDiff>;
  installHooks(preview: HooksDiff): Promise<void>;
  readSafety(): Promise<SafetySettings>;

  saveSafety(settings: SafetySettings): Promise<void>;
}
