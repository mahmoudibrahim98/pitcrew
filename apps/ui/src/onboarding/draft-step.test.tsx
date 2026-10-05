// @vitest-environment happy-dom
// @vitest-environment-options {"url": "http://localhost:5173/"}

// The optional "Draft boards" step: offered only where a hub drafts boards and the run created
// workstreams and imported history; per workstream, the cost first, then the person's start.

import { fireEvent, screen, within } from '@testing-library/react';
import { useEffect, type ReactNode } from 'react';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import type { Workstream } from '../data/index.ts';
import type { BoardDraft } from '../projects/board-drafts.ts';
import { demo, otherClient, renderWithHub, startHub, stopHub, type Hub } from '../projects/tests/harness.tsx';
import { OnboardingApiProvider } from './api-context.tsx';
import { DraftStepProvider, showDraftStep } from './draft-board.tsx';
import { createFakeOnboardingApi } from './fake-api.ts';
import { DraftStep } from './steps/draft-step.tsx';
import { useWizard, WizardProvider } from './wizard-context.tsx';

/** Puts the run where the step applies (or not), lists the steps, and shows the draft step. */
function Run({ workstreams, imported }: { workstreams: Workstream[]; imported: number }) {
  const { steps, patch } = useWizard();
  useEffect(() => {
    patch({ createResult: { projects: [], workstreams }, importResult: { imported } });
  }, [patch, workstreams, imported]);
  return (
    <>
      <ol aria-label="Steps">
        {steps.map((s) => (
          <li key={s.id}>{s.title}</li>
        ))}
      </ol>
      <DraftStep />
    </>
  );
}

function wizard(run: ReactNode, offered: boolean) {
  const inner = <WizardProvider>{run}</WizardProvider>;
  return (
    <OnboardingApiProvider api={createFakeOnboardingApi({ speed: 0 })}>
      {offered ? <DraftStepProvider>{inner}</DraftStepProvider> : inner}
    </OnboardingApiProvider>
  );
}

describe('the draft step', () => {
  let hub: Hub;

  beforeEach(async () => {
    hub = await startHub();
  });

  afterEach(async () => {
    await stopHub(hub);
  });

  it('applies only to new workstreams with imported history', () => {
    const ws = [{ id: demo.submission } as Workstream];
    expect(showDraftStep({ createResult: { projects: [], workstreams: ws }, importResult: { imported: 3 } })).toBe(true);
    expect(showDraftStep({ createResult: { projects: [], workstreams: ws } })).toBe(true);
    expect(showDraftStep({ createResult: { projects: [], workstreams: ws }, importResult: { imported: 0 } })).toBe(false);
    expect(showDraftStep({ createResult: { projects: [], workstreams: [] }, importResult: { imported: 3 } })).toBe(false);
    expect(showDraftStep({})).toBe(false);
  });

  it('is offered only by a hub that drafts, and starts a draft per workstream once confirmed', async () => {
    const person = otherClient(hub);
    const submission = await person.request<Workstream>('GET', `/v1/workstreams/${demo.submission}`);
    const { unmount } = renderWithHub(wizard(<Run workstreams={[submission]} imported={0} />, true), hub);
    // Nothing imported: nothing to draft.
    expect(within(await screen.findByRole('list', { name: 'Steps' })).queryByText('Draft boards')).toBeNull();
    unmount();

    const without = renderWithHub(wizard(<Run workstreams={[submission]} imported={4} />, false), hub);
    expect(within(await screen.findByRole('list', { name: 'Steps' })).queryByText('Draft boards')).toBeNull();
    without.unmount();

    renderWithHub(wizard(<Run workstreams={[submission]} imported={4} />, true), hub);
    const steps = await screen.findByRole('list', { name: 'Steps' });
    await within(steps).findByText('Draft boards');
    const titles = within(steps).getAllByRole('listitem').map((li) => li.textContent);
    expect(titles.indexOf('Draft boards')).toBe(titles.indexOf('Import') + 1);

    // Cost first, per workstream; nothing is sent until the person says so.
    const list = screen.getByRole('list', { name: 'Workstreams to draft' });
    await within(list).findByText(/^2 sessions, 4 existing tasks:/);
    expect(within(list).getByRole('button', { name: 'Show what will be sent' })).toBeTruthy();
    expect(await person.request<BoardDraft[]>('GET', '/v1/board-drafts')).toEqual([]);
    expect(screen.getByRole('button', { name: 'Continue without drafting' })).toBeTruthy();
    fireEvent.click(await within(list).findByRole('button', { name: 'Send and draft' }));
    await within(list).findByText(/^Drafting\. Review the proposal/);
    const drafts = await person.request<BoardDraft[]>('GET', '/v1/board-drafts');
    expect(drafts.map((d) => d.workstream)).toEqual([demo.submission]);
    // The mock's back office may already have answered.
    expect(['running', 'proposed']).toContain(drafts[0]?.state);
    expect(screen.getByRole('button', { name: 'Continue' })).toBeTruthy();
  }, 30_000);
});
