// @vitest-environment happy-dom
import { useEffect, type ReactNode } from 'react';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { createMemoryHistory, createRootRoute, createRoute, createRouter, RouterProvider } from '@tanstack/react-router';
import { afterEach, expect, it, vi } from 'vitest';
import { RegistryContext } from '../shell/context.ts';
import { defineFeature } from '../shell/feature.ts';
import { composeFeatures } from '../shell/registry.ts';
import { initialShellState, useShell } from '../shell/store.ts';
import { OnboardingApiProvider } from './api-context.tsx';
import { createFakeOnboardingApi } from './fake-api.ts';
import { CreateStep } from './steps/create-step.tsx';
import { DoneStep } from './steps/done-step.tsx';
import { ImportStep } from './steps/import-step.tsx';
import { useWizard, WizardProvider } from './wizard-context.tsx';
import { draftsFromScan } from './wizard-state.ts';
import type { ScanResult } from './api.ts';

afterEach(() => {
  cleanup();
  useShell.setState(initialShellState);
});
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

it('adds folders by hand when there is no scan (a remote or HPC first run), and unticks them', async () => {
  const api = createFakeOnboardingApi({ speed: 0 });
  const preview = vi.spyOn(api, 'importSessions');
  render(<OnboardingApiProvider api={api}><WizardProvider><ImportStep /></WizardProvider></OnboardingApiProvider>);
  fireEvent.click(screen.getByLabelText('Import a filtered set'));
  expect(screen.getByText(/There is no scan of this machine/)).toBeTruthy();
  const field = screen.getByLabelText('Add a folder');
  // Enter adds the folder, rather than submitting the step.
  fireEvent.change(field, { target: { value: '  /scratch/sam/runs  ' } });
  expect(fireEvent.keyDown(field, { key: 'Enter' })).toBe(false);
  expect(screen.getByLabelText('/scratch/sam/runs')).toHaveProperty('checked', true);
  expect((field as HTMLInputElement).value).toBe('');
  fireEvent.change(field, { target: { value: '/scratch/sam/paper' } });
  fireEvent.click(screen.getByRole('button', { name: 'Add folder' }));
  const filter = (folders: string[]) => ({ mode: 'filtered', engines: [], folders });
  await waitFor(() => expect(preview).toHaveBeenLastCalledWith(filter(['/scratch/sam/runs', '/scratch/sam/paper'])));
  // The same folder twice is still one.
  fireEvent.change(field, { target: { value: '/scratch/sam/runs' } });
  fireEvent.click(screen.getByRole('button', { name: 'Add folder' }));
  expect(screen.getAllByLabelText('/scratch/sam/runs')).toHaveLength(1);
  fireEvent.click(screen.getByLabelText('/scratch/sam/runs'));
  await waitFor(() => expect(preview).toHaveBeenLastCalledWith(filter(['/scratch/sam/paper'])));
  // Unticked, it stays listed to tick again.
  expect(screen.getByLabelText('/scratch/sam/runs')).toHaveProperty('checked', false);
});

/** The Done step behind a minimal router, with these "+ New" ids registered. */
function renderDone(ids: string[]) {
  const stub = defineFeature({
    id: 'stub',
    layout: 'both',
    create: ids.map((id) => ({ id, label: id, dialog: () => null })),
  });
  const registry = composeFeatures(defineFeature({ id: 'shell', layout: 'both' }), [stub]);
  const root = createRootRoute();
  const done = createRoute({
    getParentRoute: () => root,
    path: 'w/$ws/onboarding',
    component: () => (
      <OnboardingApiProvider api={createFakeOnboardingApi({ speed: 0 })}>
        <WizardProvider><DoneStep /></WizardProvider>
      </OnboardingApiProvider>
    ),
  });
  const page = (path: string, name: string) => createRoute({
    getParentRoute: () => root,
    path,
    component: () => <main id="main" tabIndex={-1}><h1>{name}</h1></main>,
  });
  const router = createRouter({
    routeTree: root.addChildren([done, page('w/$ws/home', 'Home'), page('w/$ws/console', 'Agent console')]),
    history: createMemoryHistory({ initialEntries: ['/w/ws-test/onboarding'] }),
  });
  render(<RegistryContext value={registry}><RouterProvider router={router} /></RegistryContext>);
  return router;
}

it('offers only the first steps that exist, and never "Invite someone" before invites do', async () => {
  renderDone(['task', 'session', 'human', 'member', 'project']);
  await screen.findByRole('button', { name: 'Create a task' });
  expect(screen.getByRole('button', { name: 'Start a session' })).toBeTruthy();
  expect(screen.queryByRole('button', { name: /Invite/ })).toBeNull();
  cleanup();
  renderDone([]);
  await screen.findByRole('button', { name: 'Open agent sessions' });
  expect(screen.queryByRole('button', { name: 'Create a task' })).toBeNull();
  expect(screen.queryByRole('button', { name: 'Start a session' })).toBeNull();
});

it('opens a first step on its page, with focus to give back to that page', async () => {
  const router = renderDone(['task', 'session']);
  fireEvent.click(await screen.findByRole('button', { name: 'Start a session' }));
  await screen.findByRole('heading', { name: 'Agent console' });
  await waitFor(() => expect(useShell.getState().creating).toBe('session'));
  expect(router.state.location.pathname).toBe('/w/ws-test/console');
  const from = useShell.getState().creatingFrom;
  expect(from).toBe(document.getElementById('main'));
  expect(from?.textContent).toBe('Agent console');
});
