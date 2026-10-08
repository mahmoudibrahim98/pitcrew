// The optional "Draft the board from history" step (brief 0-draft-board; O.md item 1): after the
// import, an agent the person already uses can draft each new workstream's board from its
// sessions, cost shown first, nothing created until the person reviews it.
//
// The step talks to the hub through the data layer (`../projects/board-drafts.ts`), not through
// `OnboardingApi`: it is offered only where a hub answers it, which `DraftStepProvider` says (the
// real first run puts it around the wizard; the fake, and tests, do not). And only when there is
// something to draft: workstreams created in this run, and sessions imported (or no import choice
// made, which keeps every session).

import { createContext, use, type ReactNode } from 'react';
import type { WizardState } from './wizard-state.ts';

const DraftStepContext = createContext(false);

/** Offers the draft step to the wizard inside it. */
export function DraftStepProvider({ children }: { children: ReactNode }) {
  return <DraftStepContext value>{children}</DraftStepContext>;
}

/** Whether a hub that drafts boards is behind this wizard. */
export function useDraftStepAvailable(): boolean {
  return use(DraftStepContext);
}

/** Whether this run has something to draft: new workstreams, and history imported. */
export function showDraftStep(state: Pick<WizardState, 'createResult' | 'importResult'>): boolean {
  const workstreams = state.createResult?.workstreams.length ?? 0;
  const imported = state.importResult === undefined || state.importResult.imported > 0;
  return workstreams > 0 && imported;
}
