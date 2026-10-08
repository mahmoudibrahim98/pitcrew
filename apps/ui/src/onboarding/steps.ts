// The first-run wizard's steps, in order. A step whose calls have no backend yet
// (`OnboardingApi.unavailable`) is left out. So are installing the helper on a machine that needs
// none (the hub's own, which runs the hub: `needsHelper`) and the optional draft step where it does
// not apply (`draft-board.tsx`). Against the real hub the first run is Welcome, Workspace, Machine
// check, Sign in, Scan, Create, Import, Draft boards (when it applies), Hooks, Safety, Done; the
// other steps come back as their routes land. The fake has them all.

import type { MachineTarget, OnboardingApi, OnboardingCall } from './api.ts';

export type StepId =
  | 'welcome'
  | 'workspace'
  | 'machine-check'
  | 'install-helper'
  | 'sign-in'
  | 'integrations'
  | 'scan'
  | 'create'
  | 'import'
  | 'draft'
  | 'hooks'
  | 'safety'
  | 'done';

export interface StepMeta {
  id: StepId;
  title: string;
  /** Shown in the stepper and as the page heading while the wizard is on it. */
  heading: string;
  skippable: boolean;
}

const FIRST_RUN: StepMeta[] = [
  { id: 'welcome', title: 'Welcome', heading: 'Welcome to PitCrew', skippable: false },
  { id: 'workspace', title: 'Workspace', heading: 'Your first workspace', skippable: false },
  { id: 'machine-check', title: 'Machine check', heading: 'Checking the machine', skippable: false },
  { id: 'install-helper', title: 'Install helper', heading: 'Install the helper', skippable: false },
  { id: 'sign-in', title: 'Sign in', heading: 'Sign in to your agents', skippable: true },
  { id: 'integrations', title: 'Integrations', heading: 'Connect integrations', skippable: true },
  { id: 'scan', title: 'Scan', heading: 'Scanning for sessions', skippable: false },
  { id: 'create', title: 'Create', heading: 'Create projects and workstreams', skippable: true },
  { id: 'import', title: 'Import', heading: 'Import sessions', skippable: true },
  // Shown only where it applies (`draft-board.tsx`): new workstreams, and history imported.
  { id: 'draft', title: 'Draft boards', heading: 'Draft the boards from history', skippable: true },
  { id: 'hooks', title: 'Hooks', heading: 'Install hooks', skippable: true },
  { id: 'safety', title: 'Safety', heading: 'Safety settings', skippable: false },
  { id: 'done', title: 'Done', heading: "You're set up", skippable: false },
];

/** The calls each step makes; it shows only when all of them are available. */
const NEEDS: Record<StepId, readonly OnboardingCall[]> = {
  welcome: [],
  workspace: ['setupWorkspace'],
  'machine-check': ['checkMachine', 'fixMachineRow'],
  'install-helper': ['launcherOptions', 'streamInstallHelper'],
  'sign-in': ['agentAccounts', 'startSignIn', 'signInRunning'],
  integrations: ['integrationStatus'],
  scan: ['streamScan'],
  // Its suggestions come from the scan.
  create: ['streamScan', 'createFromScan'],
  import: ['importSessions', 'commitImport'],
  // Through the data layer, not `OnboardingApi`: `stepsFor`'s `draft` option says when it shows.
  draft: [],
  hooks: ['hooksDiff', 'installHooks'],
  safety: ['readSafety', 'saveSafety'],
  done: [],
};

/** Which steps to offer, besides what the API can serve. */
export interface StepOptions {
  /** The machine the machine steps are about: the hub's own by default. */
  target?: MachineTarget;
  /** Offer the optional draft step (`draft-board.tsx`: a hub that drafts boards, and something to draft). */
  draft?: boolean;
}

/** The steps for `api`: the install step only where `target` needs the helper, the draft step only with `draft`. */
export function stepsFor(
  api: Pick<OnboardingApi, 'unavailable' | 'needsHelper'>,
  { target = { kind: 'local' }, draft = false }: StepOptions = {},
): StepMeta[] {
  return FIRST_RUN.filter(
    (step) =>
      NEEDS[step.id].every((call) => !api.unavailable.has(call)) &&
      (step.id !== 'install-helper' || api.needsHelper(target)) &&
      (step.id !== 'draft' || draft),
  );
}
