// The first-run wizard's steps, in order. A step whose calls have no backend yet
// (`OnboardingApi.unavailable`) is left out, so against the real hub the first run is Welcome,
// Workspace, Done; the other steps come back as their routes land. The fake has them all.

import type { OnboardingApi, OnboardingCall } from './api.ts';

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
  'sign-in': ['agentAccounts', 'startSignIn'],
  integrations: ['integrationStatus'],
  scan: ['streamScan'],
  // Its suggestions come from the scan.
  create: ['streamScan', 'createFromScan'],
  import: ['importSessions', 'commitImport'],
  // Through the data layer, not `OnboardingApi`: `DraftStepProvider` offers it.
  draft: [],
  hooks: ['hooksDiff', 'installHooks'],
  safety: ['saveSafety'],
  done: [],
};

export function stepsFor(api: Pick<OnboardingApi, 'unavailable'>): StepMeta[] {
  return FIRST_RUN.filter((step) => NEEDS[step.id].every((call) => !api.unavailable.has(call)));
}
