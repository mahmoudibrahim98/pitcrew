// Which steps make up each wizard, in order. The add-a-machine wizard reuses the same step
// components (the map in `wizard-shell.tsx`) for 2-5 and 7-9 of the first-run wizard
// (`docs/build/streams/O.md` work package 2); each component reads `useWizard().mode` where its
// copy or fields differ.

import type { WizardMode } from './wizard-state.ts';

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
  { id: 'hooks', title: 'Hooks', heading: 'Install hooks', skippable: true },
  { id: 'safety', title: 'Safety', heading: 'Safety settings', skippable: false },
  { id: 'done', title: 'Done', heading: "You're set up", skippable: false },
];

const ADD_MACHINE: StepMeta[] = [
  { id: 'workspace', title: 'Machine', heading: 'Add a machine', skippable: false },
  { id: 'machine-check', title: 'Machine check', heading: 'Checking the machine', skippable: false },
  { id: 'install-helper', title: 'Install helper', heading: 'Install the helper', skippable: false },
  { id: 'sign-in', title: 'Sign in', heading: 'Sign in to your agents', skippable: true },
  { id: 'scan', title: 'Scan', heading: 'Scanning for sessions', skippable: false },
  { id: 'create', title: 'Create', heading: 'Create projects and workstreams', skippable: true },
  { id: 'import', title: 'Import', heading: 'Import sessions', skippable: true },
  { id: 'done', title: 'Done', heading: 'Machine added', skippable: false },
];

export function stepsFor(mode: WizardMode): StepMeta[] {
  return mode === 'first-run' ? FIRST_RUN : ADD_MACHINE;
}
