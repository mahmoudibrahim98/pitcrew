// @vitest-environment happy-dom
// @vitest-environment-options {"url": "http://localhost:5173/"}
//
// The projects feature wired into the real shell router (see shell/tests/shell.test.tsx for the
// pattern this copies): its routes replace the shell's placeholders, and a task page reproduces
// from its URL alone, with no dialog — see projects/README.md, "The task page".

import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import type { QueryClient } from '@tanstack/react-query';
import { createMemoryHistory, RouterProvider } from '@tanstack/react-router';
import { afterAll, afterEach, beforeAll, describe, expect, it, vi } from 'vitest';
import { freePort, spawnHub, type HubProcess } from '../../../tests/hub-process.ts';
import { createApi } from '../../data/api.ts';
import { createQueryClient, DataProvider } from '../../data/provider.tsx';
import { createAppRouter } from '../../shell/routes.tsx';
import { feature } from '../index.ts';
import { demo } from './harness.tsx';

const TOKEN = 'dev-device-token';
const WORKSPACE = '01JB000000000000000WSP0001';

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
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  await queryClient?.cancelQueries();
  queryClient?.clear();
  localStorage.clear();
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
  return router;
}

const heading = (name: string | RegExp) => screen.findByRole('heading', { level: 1, name });
const PLACEHOLDER = 'A placeholder until its feature fills this page.';

describe('the projects feature, wired into the shell', () => {
  it('asks before route navigation with a dirty file and preserves the draft on cancel', async () => {
    const path = `/w/${WORKSPACE}/projects/${demo.paper}/workstreams/${demo.submission}`;
    const router = renderApp(path);
    fireEvent.click(await screen.findByRole('radio', { name: 'Files' }));
    fireEvent.click(await screen.findByRole('button', { name: '▸ src' }));
    fireEvent.click(await screen.findByRole('button', { name: 'hello.txt' }));
    fireEvent.click(await screen.findByRole('button', { name: 'Edit' }));
    fireEvent.change(screen.getByLabelText('Edit file text'), { target: { value: 'unsaved draft' } });
    const confirm = vi.fn().mockReturnValue(false);
    vi.stubGlobal('confirm', confirm);
    fireEvent.click(screen.getByRole('button', { name: 'Paper · Diffusion study' }));
    await waitFor(() => expect(confirm).toHaveBeenCalledWith('Discard unsaved changes?'));
    expect(router.state.location.pathname).toBe(path);
    expect((screen.getByLabelText('Edit file text') as HTMLTextAreaElement).value).toBe('unsaved draft');
    void router.navigate({ to: `/w/${WORKSPACE}/home` });
    await waitFor(() => expect(confirm).toHaveBeenCalledTimes(2));
    expect(router.state.location.pathname).toBe(path);
    confirm.mockReturnValue(true);
    await router.navigate({ to: `/w/${WORKSPACE}/home` });
    await heading('Home');
  });
  it('serves Home with live panels where the shell had a placeholder', async () => {
    renderApp(`/w/${WORKSPACE}/home`);
    await heading('Home');
    await screen.findByRole('region', { name: 'Where things stand' });
    expect(screen.queryByText(PLACEHOLDER)).toBeNull();
  });

  it(
    'serves the Inbox, answerable in place',
    async () => {
      renderApp(`/w/${WORKSPACE}/inbox`);
      await heading('Inbox');
      // The question card is a lazy chunk (stream M's); the first load in this file can be slow.
      await screen.findByText('Merge the benchmark change into parsers?', {}, { timeout: 15_000 });
      fireEvent.click(screen.getByRole('button', { name: 'Merge it' }));
      await screen.findByText('Answered: Merge it');
    },
    20_000,
  );

  it('serves My tasks where the shell had a placeholder', async () => {
    renderApp(`/w/${WORKSPACE}/my-tasks`);
    await heading('My tasks');
    expect(screen.queryByText(PLACEHOLDER)).toBeNull();
  });

  it('serves the projects list', async () => {
    renderApp(`/w/${WORKSPACE}/projects`);
    await heading('Projects');
    // The sidebar's Projects tree names projects too; scope to the page's own list.
    const list = await screen.findByRole('list', { name: 'Projects' });
    await within(list).findByText('Paper · Diffusion study');
  });

  it('serves Members: people and agents in one table, owners shown', async () => {
    renderApp(`/w/${WORKSPACE}/members`);
    await heading('Members');
    await screen.findByText('Sam Rivera');
    await screen.findByText('Writer, agent of Sam Rivera');
    // Sam's own handle, plus the owner column of every agent row.
    expect((await screen.findAllByText('@sam')).length).toBeGreaterThan(1);
  });

  it('opens a project with tabs, then a workstream, then a task — each with its own URL', async () => {
    const router = renderApp(`/w/${WORKSPACE}/projects/${demo.paper}`);
    await heading('Paper · Diffusion study');
    expect(router.state.location.pathname).toBe(`/w/${WORKSPACE}/projects/${demo.paper}`);

    fireEvent.click(screen.getByRole('radio', { name: 'Board' }));
    await screen.findByText('Draft the method section');

    fireEvent.click(screen.getByRole('radio', { name: 'Workstreams' }));
    const table = await screen.findByRole('table', { name: 'Workstreams' });
    fireEvent.click(within(table).getByRole('button', { name: 'Seed runs' }));
    await heading('Seed runs');
    expect(router.state.location.pathname).toBe(
      `/w/${WORKSPACE}/projects/${demo.paper}/workstreams/${demo.seedRuns}`,
    );

    fireEvent.click(screen.getByRole('radio', { name: 'Tasks' }));
    fireEvent.click(await screen.findByRole('button', { name: /Run seeds/ }));
    await heading(/^PAP-4 · Run seeds/);
    // `openTask` takes the task's id (every component calls it that way); the route resolves by
    // either id or key — see `task-page.tsx` and `src/shell/paths.ts`, `task(ws, task)`.
    expect(router.state.location.pathname).toBe(`/w/${WORKSPACE}/tasks/${demo.pap4}`);
    // A page of its own, not a drawer over the board: see README.md, "The task page".
    expect(screen.queryByRole('dialog')).toBeNull();
  });

  it('reproduces a task directly from its URL (by key), with no dialog', async () => {
    renderApp(`/w/${WORKSPACE}/tasks/PAP-4`);
    await heading(/^PAP-4 · Run seeds/);
    expect(screen.queryByRole('dialog')).toBeNull();
    await screen.findByText('Submit seeds 1–5');
    expect((await screen.findAllByText('Agent plan · @runner')).length).toBe(3);
  });

  it('a workstream known only by id still resolves (the shell keeps that redirect)', async () => {
    renderApp(`/w/${WORKSPACE}/workstreams/${demo.seedRuns}`);
    await heading('Seed runs');
  });
});
