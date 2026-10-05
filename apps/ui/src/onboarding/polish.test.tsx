// @vitest-environment happy-dom
import { useEffect, type ReactNode } from 'react';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { OnboardingApiProvider } from './api-context.tsx';
import { createFakeOnboardingApi } from './fake-api.ts';
import { CreateStep } from './steps/create-step.tsx';
import { ImportStep } from './steps/import-step.tsx';
import { useWizard, WizardProvider } from './wizard-context.tsx';
import { draftsFromScan } from './wizard-state.ts';
import type { ScanResult } from './api.ts';

afterEach(cleanup);
const scan: ScanResult = {
  counts: { byEngine: { opencode: 1 }, byFolder: [{ path: '/home/sam/work/paper', count: 1 }], byMonth: [] },
  suggestedProjects: [
    { id: 'paper', name: 'Paper', path: '/home/sam/work/paper', workstreams: [{ id: 'draft', name: 'Draft', sessionCount: 1 }] },
    { id: 'runs', name: 'Runs', path: '/home/sam/work/runs', workstreams: [{ id: 'sweep', name: 'Sweep', sessionCount: 2 }] },
  ],
};

function Seed({ children }: { children: ReactNode }) {
  const { patch } = useWizard();
  useEffect(() => {
    const drafts = draftsFromScan(scan);
    patch({ scanResult: scan, createProjects: drafts.projects, createWorkstreams: drafts.workstreams });
  }, [patch]);
  return children;
}

it('merges two suggestions into one project without dropping their workstreams', async () => {
  const api = createFakeOnboardingApi({ speed: 0 });
  const create = vi.spyOn(api, 'createFromScan');
  render(<OnboardingApiProvider api={api}><WizardProvider><Seed><CreateStep /></Seed></WizardProvider></OnboardingApiProvider>);
  expect(await screen.findByText('/home/sam/work/paper')).toBeTruthy();
  expect(screen.getByText('1 session')).toBeTruthy();
  fireEvent.change(screen.getByLabelText('Merge Runs into project'), { target: { value: 'paper' } });
  expect(screen.getByLabelText('Include Runs')).toHaveProperty('checked', false);
  fireEvent.click(screen.getByRole('button', { name: 'Create' }));
  await waitFor(() => expect(create).toHaveBeenCalledWith([{ suggestionId: 'paper', name: 'Paper', template: 'software', workstreams: [{ suggestionId: 'draft', name: 'Draft' }, { suggestionId: 'sweep', name: 'Sweep' }] }]));
});

it('uses checked scanned folders and named engines in the import preview and commit', async () => {
  const api = createFakeOnboardingApi({ speed: 0 });
  const preview = vi.spyOn(api, 'importSessions');
  const commit = vi.spyOn(api, 'commitImport');
  render(<OnboardingApiProvider api={api}><WizardProvider><Seed><ImportStep /></Seed></WizardProvider></OnboardingApiProvider>);
  fireEvent.click(screen.getByLabelText('Import a filtered set'));
  fireEvent.click(screen.getByLabelText(/\/home\/sam\/work\/paper/));
  fireEvent.click(screen.getByLabelText('OpenCode'));
  const filter = { mode: 'filtered', engines: ['opencode'], folders: ['/home/sam/work/paper'] };
  await waitFor(() => expect(preview).toHaveBeenLastCalledWith(filter));
  await waitFor(() => expect(screen.getByRole('button', { name: 'Continue' })).toHaveProperty('disabled', false));
  fireEvent.click(screen.getByRole('button', { name: 'Continue' }));
  await waitFor(() => expect(commit).toHaveBeenCalledWith(filter));
});
