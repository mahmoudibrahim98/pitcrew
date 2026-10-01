// The wizards' shared, in-memory state: one object per run, held in `WizardProvider`
// (`wizard-context.tsx`) and read or patched by step components. Never persisted: reloading the
// page starts the wizard over, same as abandoning it.

import type { Engine } from '../data/index.ts';
import type {
  AgentAccount,
  CreateFromScanResult,
  DiscoveredHost,
  HooksDiff,
  ImportDryRunResult,
  ImportMode,
  ImportResult,
  IntegrationStatus,
  Launcher,
  LauncherOption,
  MachineCheckResult,
  MachineTarget,
  ProjectTemplate,
  SafetySettings,
  ScanResult,
} from './api.ts';

export type WizardMode = 'first-run' | 'add-machine';

export type Density = 'comfortable' | 'compact';
export type WizardTheme = 'light' | 'dark' | 'system';

/** A suggested project, as the "Create" step's checklist edits it. */
export interface CreateProjectDraft {
  suggestionId: string;
  checked: boolean;
  name: string;
  template: ProjectTemplate;
}

/** A suggested workstream; `projectId` is the owning draft's `suggestionId` and can be moved. */
export interface CreateWorkstreamDraft {
  suggestionId: string;
  checked: boolean;
  name: string;
  projectId: string;
  sessionCount: number;
}

export type StepStatus = 'idle' | 'running' | 'done' | 'error';

export interface WizardState {
  mode: WizardMode;

  // Welcome
  theme: WizardTheme;
  density: Density;

  // Workspace / add-machine target
  workspaceName: string;
  primaryMachine: MachineTarget;
  /** Cached (never refetched once present), so Back/Forward doesn't re-query the host list. */
  discoveredHosts: DiscoveredHost[];
  hostsStatus: StepStatus;

  // Machine check. Keyed by `targetKey()` so revisiting after Back/Forward reuses the result
  // instead of re-running the check (ADR-0009: fixes are server-side truth anyway, but refetching
  // is still wasted work every time the step remounts).
  machineCheckByTarget: Record<string, MachineCheckResult>;

  // Install helper. Both keyed by `targetKey()`: `launcherOptionsByTarget` so the options aren't
  // refetched, and `launcherChoiceByTarget` so a launcher the person explicitly picked for a
  // target is never overwritten by the recommended default on a later visit to the same target
  // (review r1, item 1) — only a target with no entry yet gets seeded from `recommended`.
  launcherOptionsByTarget: Record<string, LauncherOption[]>;
  launcherChoiceByTarget: Record<string, Launcher>;
  /** `| undefined` (not just optional) so a step can explicitly clear it when a new install starts. */
  slurmScript?: string | undefined;
  installLog: string[];
  installStatus: StepStatus;
  installError?: string | undefined;

  // Sign in. Not per-target: the fake (and any real client) reports every engine's account
  // regardless of which machine is current.
  accounts: AgentAccount[];
  accountsStatus: StepStatus;

  // Integrations
  integrations: IntegrationStatus[];
  integrationsStatus: StepStatus;

  // Scan
  scanStatus: StepStatus;
  scanProgress?: { scanned: number; total?: number } | undefined;
  scanResult?: ScanResult;

  // Create projects and workstreams
  createProjects: CreateProjectDraft[];
  createWorkstreams: CreateWorkstreamDraft[];
  createResult?: CreateFromScanResult;

  // Import
  importMode: ImportMode;
  importSince: string;
  importEngines: Engine[];
  importDryRun?: ImportDryRunResult;
  importResult?: ImportResult;

  // Hooks
  hooksDiff?: HooksDiff;
  hooksInstalled: boolean;

  // Safety
  safety: SafetySettings;
}

export function initialWizardState(mode: WizardMode): WizardState {
  return {
    mode,
    theme: 'system',
    density: 'comfortable',
    workspaceName: '',
    primaryMachine: { kind: 'local' },
    discoveredHosts: [],
    hostsStatus: 'idle',
    machineCheckByTarget: {},
    launcherOptionsByTarget: {},
    launcherChoiceByTarget: {},
    installLog: [],
    installStatus: 'idle',
    accounts: [],
    accountsStatus: 'idle',
    integrations: [],
    integrationsStatus: 'idle',
    scanStatus: 'idle',
    createProjects: [],
    createWorkstreams: [],
    importMode: 'all',
    importSince: '',
    importEngines: [],
    hooksInstalled: false,
    safety: {
      permissionMode: 'default',
      backOfficeEnabled: false,
      backOfficeCaps: { maxAutoAcceptPerHour: 5 },
    },
  };
}

/** Seeds the "Create" step's drafts from a scan result; every suggestion starts ticked. */
export function draftsFromScan(result: ScanResult): {
  projects: CreateProjectDraft[];
  workstreams: CreateWorkstreamDraft[];
} {
  return {
    projects: result.suggestedProjects.map((p) => ({
      suggestionId: p.id,
      checked: true,
      name: p.name,
      template: 'software',
    })),
    workstreams: result.suggestedProjects.flatMap((p) =>
      p.workstreams.map((w) => ({
        suggestionId: w.id,
        checked: true,
        name: w.name,
        projectId: p.id,
        sessionCount: w.sessionCount,
      })),
    ),
  };
}
