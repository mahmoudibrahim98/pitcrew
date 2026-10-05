// @vitest-environment happy-dom
// @vitest-environment-options {"url": "http://localhost:5173/"}

import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import type { QueryClient } from '@tanstack/react-query';
import { createMemoryHistory, RouterProvider } from '@tanstack/react-router';
import { afterAll, afterEach, beforeAll, describe, expect, it } from 'vitest';
import { freePort, spawnHub, type HubProcess } from '../../../tests/hub-process.ts';
import { createApi } from '../../data/api.ts';
import { createQueryClient, DataProvider } from '../../data/provider.tsx';
import { defineFeature, type Feature } from '../feature.ts';
import { createAppRouter } from '../routes.tsx';
import { initialShellState, useShell } from '../store.ts';

const TOKEN = 'dev-device-token';
const REASON = 'Starting a session is not available yet.';

let hub: HubProcess;

beforeAll(async () => {
  hub = await spawnHub(await freePort());
});

afterAll(async () => {
  await hub.close();
});

let queryClient: QueryClient | undefined;

afterEach(async () => {
  cleanup();
  // Abort fetches still in flight, so none is cut off when the hub stops.
  await queryClient?.cancelQueries();
  queryClient?.clear();
  localStorage.clear();
  useShell.setState(initialShellState);
});

function renderApp(features: Feature[]) {
  const router = createAppRouter(features, { history: createMemoryHistory({ initialEntries: ['/'] }) });
  const api = createApi({ baseUrl: hub.url, token: TOKEN });
  queryClient = createQueryClient();
  render(
    <DataProvider api={api} queryClient={queryClient} token={TOKEN}>
      <RouterProvider router={router} />
    </DataProvider>,
  );
}

function sessionFeature(): Feature {
  return defineFeature({
    id: 'console',
    layout: 'console',
    create: [
      { id: 'task', label: 'Task', order: 10, dialog: () => <p>Task form</p> },
      { id: 'session', label: 'Session', dialog: () => <p>Starting…</p>, disabled: REASON },
    ],
  });
}

describe('a disabled "+ New" item', () => {
  it('shows disabled with its reason as an accessible description, and never opens a dialog', async () => {
    renderApp([sessionFeature()]);

    await screen.findByRole('heading', { level: 1, name: 'Home' }, { timeout: 8_000 });
    // The Radix trigger opens on a real keyboard activation; a synthetic click alone does not.
    fireEvent.keyDown(screen.getByRole('button', { name: 'New' }), { key: 'Enter' });

    const item = await screen.findByRole('menuitem', { name: 'Session' });
    expect(item.getAttribute('aria-disabled')).toBe('true');
    const describedBy = item.getAttribute('aria-describedby');
    expect(describedBy).not.toBeNull();
    expect(document.getElementById(describedBy ?? '')?.textContent).toBe(REASON);

    fireEvent.click(item);
    expect(screen.queryByRole('dialog')).toBeNull();
    // The menu stays open: a disabled item does not act as a selection.
    expect(screen.getByRole('menuitem', { name: 'Session' })).toBeTruthy();

    // An enabled item next to it still opens its dialog.
    fireEvent.click(screen.getByRole('menuitem', { name: 'Task' }));
    expect(await screen.findByRole('dialog')).toBeTruthy();
  });

  it("is left out of the palette's results", async () => {
    renderApp([sessionFeature()]);
    await screen.findByRole('heading', { level: 1, name: 'Home' }, { timeout: 8_000 });

    fireEvent.keyDown(window, { key: 'k', ctrlKey: true });
    const input = await screen.findByRole('combobox', { name: 'Search' });
    fireEvent.change(input, { target: { value: 'session' } });

    expect(screen.queryByText('New session')).toBeNull();
  });
});
