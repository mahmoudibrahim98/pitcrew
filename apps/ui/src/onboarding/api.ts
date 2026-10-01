// The onboarding wizards' backend contract. None of these routes exist on the hub yet (see
// README.md for the proposed shapes); `createFakeOnboardingApi` in `fake-api.ts` is the only
// implementation today, and every step is built against the `OnboardingApi` interface below so a
// real client can replace the fake without touching a step component.
//
// Reuses wire types from `../data` (`Engine`, `Machine`, `Project`, `Workstream`, …) rather than
// redeclaring them, since the real routes below would return the same objects.

import type { Engine, MachineKind, Project, Workstream } from '../data/index.ts';

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

/** An SSH host offered from the user's own `~/.ssh/config`, or a WSL distro from `wsl -l`. */
export interface DiscoveredHost {
  kind: Extract<MachineKind, 'wsl' | 'ssh'>;
  /** The distro name (`wsl`) or the `Host` alias (`ssh`). */
  id: string;
  detail?: string;
}

export type CheckRowId = 'cli-claude' | 'cli-codex' | 'cli-opencode' | 'tmux' | 'git' | 'gh' | 'disk' | 'slurm';

export type CheckRowStatus = 'ok' | 'warn' | 'missing' | 'checking';

export interface MachineCheckRow {
  id: CheckRowId;
  label: string;
  status: CheckRowStatus;
  /** A version string, free space, or an error, shown under the label. */
  detail?: string;
  /** Whether "Fix" can do something about it (install a CLI, free disk space is not). */
  fixable: boolean;
}

export interface MachineCheckResult {
  machine: MachineTarget;
  rows: MachineCheckRow[];
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
}

/** One line of a live install log, or the terminal event that ends the stream. */
export type InstallProgressEvent =
  | { type: 'log'; line: string }
  /** The exact script to be submitted (`launcher: 'slurm'` only), shown before anything runs. */
  | { type: 'script-preview'; script: string }
  | { type: 'done' }
  | { type: 'error'; message: string };

/** A cancellable streamed call: every `stream*` method on `OnboardingApi` returns one of these. */
export interface Streamed {
  cancel(): void;
}

export interface AgentAccount {
  engine: Engine;
  /** The signed-in account's label (email or handle), absent when not signed in. */
  account?: string;
  signedIn: boolean;
}

export interface StartSignInResult {
  /** Opens in the console's terminal view (a placeholder link until the console exists). */
  terminalSessionId: string;
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
  | { type: 'done'; result: ScanResult };

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

export interface SetupWorkspaceInput {
  name: string;
  primaryMachine: MachineTarget;
}

export interface SetupWorkspaceResult {
  workspace: { id: string; name: string };
}

/**
 * The onboarding wizards' backend contract (proposed; see README.md). Every method is called from
 * a step component through `useOnboardingApi()` (`api-context.tsx`); `fake-api.ts` is the only
 * implementation so far.
 */
export interface OnboardingApi {
  /** SSH hosts from the user's `~/.ssh/config` and WSL distros, for the machine picker. */
  discoverHosts(): Promise<DiscoveredHost[]>;
  /** Names and remembers the workspace's primary machine. Idempotent: safe to call again. */
  setupWorkspace(input: SetupWorkspaceInput): Promise<SetupWorkspaceResult>;

  checkMachine(target: MachineTarget): Promise<MachineCheckResult>;
  /** Re-checks one row after "Fix" runs. Rejects if `fixable` was false. */
  fixMachineRow(target: MachineTarget, row: CheckRowId): Promise<MachineCheckRow>;
  launcherOptions(target: MachineTarget): Promise<LauncherOption[]>;

  /** Streams progress; for `launcher: 'slurm'` the first event is the script preview. */
  streamInstallHelper(options: InstallHelperOptions, onEvent: (event: InstallProgressEvent) => void): Streamed;

  agentAccounts(): Promise<AgentAccount[]>;
  /** Opens the CLI's own login in a terminal on the target machine (never reads its tokens). */
  startSignIn(engine: Engine, machine: MachineTarget): Promise<StartSignInResult>;

  integrationStatus(): Promise<IntegrationStatus[]>;

  streamScan(target: ScanTarget, onEvent: (event: ScanProgressEvent) => void): Streamed;

  createFromScan(selection: ProjectSelection[]): Promise<CreateFromScanResult>;

  /** A dry run: counts what the filter would import without importing anything. */
  importSessions(filter: ImportFilter): Promise<ImportDryRunResult>;
  /** Commits the import (reversible: `link_basis: "imported"` sessions can be unlinked later). */
  commitImport(filter: ImportFilter): Promise<ImportResult>;

  hooksDiff(): Promise<HooksDiff>;
  installHooks(): Promise<void>;

  saveSafety(settings: SafetySettings): Promise<void>;
}
