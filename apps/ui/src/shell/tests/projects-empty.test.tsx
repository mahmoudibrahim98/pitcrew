// @vitest-environment happy-dom
// @vitest-environment-options {"url": "http://localhost:5173/"}
// A workspace with no projects yet (a fresh mock hub, set up): the projects tree says so, and its
// "New project" opens the Projects feature's own dialog, never a placeholder, and gives focus back
// when it closes. Without that dialog registered, it offers nothing.

import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import type { QueryClient } from '@tanstack/react-query';
import { createMemoryHistory, RouterProvider } from '@tanstack/react-router';
import { afterAll, afterEach, beforeAll, expect, it } from 'vitest';
import { freePort, spawnHub, type HubProcess } from '../../../tests/hub-process.ts';
import { createApi } from '../../data/api.ts';
import { createQueryClient, DataProvider } from '../../data/provider.tsx';
import { feature as projects } from '../../projects/index.ts';
import type { Feature } from '../feature.ts';
import { createAppRouter } from '../routes.tsx';
import { initialShellState, useShell } from '../store.ts';

const TOKEN = 'dev-device-token';
const PATIENCE = { timeout: 8_000 };

let hub: HubProcess;
let queryClient: QueryClient | undefined;
let ws = '';

beforeAll(async () => {
  process.env.PITCREW_MOCK_FRESH = '1';
  try {
    hub = await spawnHub(await freePort());
  } finally {
    delete process.env.PITCREW_MOCK_FRESH;
  }
  const api = createApi({ baseUrl: hub.url, token: TOKEN });
  await api.setup({ workspace_name: 'Demo Lab', person: { name: 'Sam Rivera', handle: '@sam' }, machine_name: 'This laptop' });
  ws = (await api.workspace()).workspace.id;
});

afterAll(async () => {
  await hub.close();
});

afterEach(async () => {
  cleanup();
  await queryClient?.cancelQueries();
  queryClient?.clear();
  localStorage.clear();
  useShell.setState(initialShellState);
});

function renderApp(features: Feature[]) {
  const router = createAppRouter(features, { history: createMemoryHistory({ initialEntries: [`/w/${ws}/home`] }) });
  queryClient = createQueryClient();
  render(
    <DataProvider api={createApi({ baseUrl: hub.url, token: TOKEN })} queryClient={queryClient} token={TOKEN}>
      <RouterProvider router={router} />
    </DataProvider>,
  );
}

const sidebar = () => screen.getByRole('complementary', { name: 'Sidebar' });

it('offers "New project", which opens the real dialog and gives focus back', async () => {
  renderApp([projects]);
  await screen.findByRole('heading', { level: 1, name: 'Home' }, PATIENCE);
  await within(sidebar()).findByText('No projects yet.', undefined, PATIENCE);
  const button = within(sidebar()).getByRole('button', { name: 'New project' });
  fireEvent.click(button);

  const dialog = await screen.findByRole('dialog', { name: 'New project' });
  // The Projects feature's form, not a placeholder.
  await within(dialog).findByLabelText('Name', undefined, PATIENCE);
  expect(dialog.textContent).not.toMatch(/placeholder|not available/i);

  fireEvent.keyDown(document.activeElement ?? dialog, { key: 'Escape' });
  await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
  await waitFor(() => expect(document.activeElement).toBe(button));
}, 20_000);

it('offers no "New project" while no feature registers one', async () => {
  renderApp([]);
  await screen.findByRole('heading', { level: 1, name: 'Home' }, PATIENCE);
  await within(sidebar()).findByText('No projects yet.', undefined, PATIENCE);
  expect(within(sidebar()).queryByRole('button', { name: 'New project' })).toBeNull();
});
