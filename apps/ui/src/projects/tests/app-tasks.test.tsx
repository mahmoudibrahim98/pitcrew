// @vitest-environment happy-dom
// @vitest-environment-options {"url": "http://localhost:5173/"}
//
// Task flows through the whole app (the shell's router and frame with the projects feature):
// creating from "+ New" and from a board column, the create notification's Open, Copy link from
// the layout's drawer, and archived tasks left out of the sidebar's counts. One hub per test, as
// these tests write.

import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import type { QueryClient } from '@tanstack/react-query';
import { createMemoryHistory, RouterProvider } from '@tanstack/react-router';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { freePort, spawnHub, type HubProcess } from '../../../tests/hub-process.ts';
import { createApi } from '../../data/api.ts';
import { createQueryClient, DataProvider } from '../../data/provider.tsx';
import { clearToasts } from '../../design/index.ts';
import { createAppRouter } from '../../shell/routes.tsx';
import { initialShellState, useShell } from '../../shell/store.ts';
import { feature } from '../index.ts';
import { demo } from './harness.tsx';

const TOKEN = 'dev-device-token';
const WORKSPACE = '01JB000000000000000WSP0001';

let hub: HubProcess;
let queryClient: QueryClient | undefined;

beforeEach(async () => {
  clearToasts();
  hub = await spawnHub(await freePort());
});

afterEach(async () => {
  cleanup();
  await queryClient?.cancelQueries();
  queryClient?.clear();
  localStorage.clear();
  useShell.setState(initialShellState);
  delete (window as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__;
  await new Promise((done) => setTimeout(done, 25));
  await hub.close();
});

function renderApp(path: string) {
  const router = createAppRouter([feature], { history: createMemoryHistory({ initialEntries: [path] }) });
  const api = createApi({ baseUrl: hub.url, token: TOKEN });
  queryClient = createQueryClient();
  render(
    <DataProvider api={api} queryClient={queryClient} token={TOKEN}>
      <RouterProvider router={router} />
    </DataProvider>,
  );
  return { router, api };
}

const heading = (name: string | RegExp) => screen.findByRole('heading', { level: 1, name }, { timeout: 8_000 });
const select = (scope: HTMLElement, label: string) => within(scope).getByLabelText(label) as HTMLSelectElement;

describe('tasks in the app', () => {
  it('opens a task created from "+ New" with the router, without reloading the document', async () => {
    const { router } = renderApp(`/w/${WORKSPACE}/projects/${demo.paper}`);
    await heading('Paper · Diffusion study');
    // The Radix trigger opens on a real keyboard activation; a synthetic click alone does not.
    fireEvent.keyDown(screen.getByRole('button', { name: 'New' }), { key: 'Enter' });
    fireEvent.click(await screen.findByRole('menuitem', { name: 'Task' }));
    const dialog = await screen.findByRole('dialog', { name: 'New task' });
    fireEvent.change(await within(dialog).findByLabelText('Title'), { target: { value: 'Synthetic created task' } });
    await waitFor(() => expect(select(dialog, 'Project').value).toBe(demo.paper));
    fireEvent.click(within(dialog).getByRole('button', { name: 'Create' }));
    const open = await screen.findByRole('button', { name: 'Open' });
    const href = window.location.href;
    fireEvent.click(open);
    await heading(/Synthetic created task/);
    expect(router.state.location.pathname).toMatch(new RegExp(`^/w/${WORKSPACE}/tasks/[0-9A-Z]{26}$`));
    expect(window.location.href).toBe(href);
  });

  it('opens the shell’s New task dialog from a board column with its project and status, and only from there', async () => {
    renderApp(`/w/${WORKSPACE}/projects/${demo.paper}`);
    await heading('Paper · Diffusion study');
    fireEvent.click(screen.getByRole('radio', { name: 'Board' }));
    const add = await screen.findByRole('button', { name: 'Add a task to In progress' });
    expect(screen.queryByRole('button', { name: /^New task in/ })).toBeNull();
    fireEvent.click(add);
    const dialog = await screen.findByRole('dialog', { name: 'New task' });
    await within(dialog).findByLabelText('Title');
    await waitFor(() => expect(select(dialog, 'Project').value).toBe(demo.paper));
    expect(select(dialog, 'Status').value).toBe('in_progress');
    fireEvent.keyDown(within(dialog).getByLabelText('Title'), { key: 'Escape' });
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
    await waitFor(() => expect(document.activeElement).toBe(add));

    // "+ New" afterwards starts afresh: the column's status is not carried over.
    fireEvent.keyDown(screen.getByRole('button', { name: 'New' }), { key: 'Enter' });
    fireEvent.click(await screen.findByRole('menuitem', { name: 'Task' }));
    const fresh = await screen.findByRole('dialog', { name: 'New task' });
    await within(fresh).findByLabelText('Title');
    expect(select(fresh, 'Status').value).toBe('todo');
  });

  it('copies the pitcrew:// deep link from the layout’s drawer in the desktop app', async () => {
    const copied: string[] = [];
    Object.defineProperty(navigator, 'clipboard', {
      configurable: true,
      value: { writeText: (text: string) => { copied.push(text); return Promise.resolve(); } },
    });
    renderApp(`/w/${WORKSPACE}/projects/${demo.paper}/workstreams/${demo.seedRuns}`);
    await heading('Seed runs');
    fireEvent.click(screen.getByRole('radio', { name: 'Tasks' }));
    fireEvent.click(await screen.findByRole('button', { name: /Run seeds/ }));
    const drawer = await screen.findByRole('dialog', { name: /^Run seeds/ });
    (window as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__ = {};
    fireEvent.click(within(drawer).getByRole('button', { name: 'Copy link' }));
    await waitFor(() => expect(copied).toEqual([`pitcrew://w/${WORKSPACE}/task/${demo.pap4}`]));
  });

  it('leaves archived tasks out of the sidebar’s open-task counts', async () => {
    const { api } = renderApp(`/w/${WORKSPACE}/home`);
    await heading('Home');
    // The badge reads "<n> open tasks" (its label is for screen readers).
    const count = () => Number.parseInt(screen.getByTestId(`open-${demo.paper}`).textContent, 10);
    await screen.findByTestId(`open-${demo.paper}`);
    await waitFor(() => expect(count()).toBeGreaterThan(0));
    const before = count();
    await api.patchTask(demo.pap5, { archived: true });
    await waitFor(() => expect(count()).toBe(before - 1));
    await api.patchTask(demo.pap5, { archived: false });
    await waitFor(() => expect(count()).toBe(before));
  });
});
