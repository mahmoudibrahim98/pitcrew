// @vitest-environment happy-dom
// @vitest-environment-options {"url": "http://localhost:5173/"}

import { cleanup, fireEvent, render, screen, within } from '@testing-library/react';
import { createMemoryHistory, createRoute, RouterProvider } from '@tanstack/react-router';
import { afterAll, afterEach, beforeAll, describe, expect, it, vi } from 'vitest';
import { createApi } from '../src/data/api.ts';
import { createQueryClient, DataProvider } from '../src/data/provider.tsx';
import { defineFeature, type Feature } from '../src/shell/feature.ts';
import { createAppRouter } from '../src/shell/routes.tsx';
import { initialShellState, useShell } from '../src/shell/store.ts';
import { freePort, spawnHub, type HubProcess } from './hub-process.ts';

const TOKEN = 'dev-device-token';
const WORKSPACE = '01JB000000000000000WSP0001';

let hub: HubProcess;

beforeAll(async () => {
  hub = await spawnHub(await freePort());
});

afterAll(async () => {
  await hub.close();
});

afterEach(() => {
  cleanup();
  localStorage.clear();
  useShell.setState(initialShellState);
});

function renderApp(features: Feature[], path = '/') {
  const router = createAppRouter(features, { history: createMemoryHistory({ initialEntries: [path] }) });
  const api = createApi({ baseUrl: hub.url, token: TOKEN });
  render(
    <DataProvider api={api} queryClient={createQueryClient()} token={TOKEN}>
      <RouterProvider router={router} />
    </DataProvider>,
  );
  return router;
}

const sidebar = () => screen.getByRole('complementary', { name: 'Sidebar' });

describe('feature registration', () => {
  it("shows a stub feature's nav entry in the sidebar and routes to its page", async () => {
    const stub = defineFeature({
      id: 'stub',
      layout: 'projects',
      routes: (parent) => [
        createRoute({
          getParentRoute: () => parent,
          path: 'stub',
          component: function StubPage() {
            return <h1>Hello from the stub</h1>;
          },
        }),
      ],
      nav: [
        {
          id: 'stub',
          label: 'Stub page',
          to: 'stub',
          badge: function StubBadge() {
            return <span>7</span>;
          },
        },
      ],
    });
    const router = renderApp([stub]);

    // `/` opens the workspace's default layout.
    await screen.findByRole('heading', { level: 1, name: 'Home' }, { timeout: 8_000 });
    expect(router.state.location.pathname).toBe(`/w/${WORKSPACE}/home`);

    const link = within(sidebar()).getByRole('link', { name: /Stub page/ });
    expect(link.getAttribute('href')).toBe(`/w/${WORKSPACE}/stub`);
    expect(link.textContent).toContain('7');
    fireEvent.click(link);

    await screen.findByRole('heading', { level: 1, name: 'Hello from the stub' });
    expect(router.state.location.pathname).toBe(`/w/${WORKSPACE}/stub`);
    expect(link.getAttribute('aria-current')).toBe('page');
  });

  it("replaces the shell's placeholder when a feature serves the same path", async () => {
    const inbox = defineFeature({
      id: 'inbox',
      layout: 'both',
      routes: (parent) => [
        createRoute({
          getParentRoute: () => parent,
          path: 'inbox',
          component: function Inbox() {
            return <h1>The real Inbox</h1>;
          },
        }),
      ],
    });
    renderApp([inbox], `/w/${WORKSPACE}/inbox`);
    await screen.findByRole('heading', { level: 1, name: 'The real Inbox' }, { timeout: 8_000 });
  });

  it('shows entries only in their layout, and a route of one layout switches to it', async () => {
    const consoleFeature = defineFeature({
      id: 'console',
      layout: 'console',
      routes: (parent) => [
        createRoute({
          getParentRoute: () => parent,
          path: 'console/machines',
          component: function Machines() {
            return <h1>Machines</h1>;
          },
        }),
      ],
      nav: [{ id: 'machines', label: 'Machines', to: 'console/machines' }],
    });
    const router = renderApp([consoleFeature], `/w/${WORKSPACE}/home`);
    await screen.findByRole('heading', { level: 1, name: 'Home' }, { timeout: 8_000 });
    expect(within(sidebar()).queryByRole('link', { name: 'Machines' })).toBeNull();
    expect(screen.getByRole('radio', { name: 'Projects' }).getAttribute('aria-checked')).toBe('true');

    await router.navigate({ href: `/w/${WORKSPACE}/console/machines` });
    await screen.findByRole('heading', { level: 1, name: 'Machines' });
    expect(within(sidebar()).getByRole('link', { name: 'Machines' })).toBeTruthy();
    expect(within(sidebar()).queryByRole('link', { name: 'Home' })).toBeNull();
    expect(screen.getByRole('radio', { name: 'Agent console' }).getAttribute('aria-checked')).toBe('true');
    await vi.waitFor(() => expect(useShell.getState().workspaces[WORKSPACE]?.layout).toBe('console'));
  });

  it('sends a workstream known only by id to its path under its project', async () => {
    const router = renderApp([], `/w/${WORKSPACE}/workstreams/01JB000000000000000WST0002`);
    await screen.findByRole('heading', { level: 1, name: 'Seed runs' }, { timeout: 8_000 });
    expect(router.state.location.pathname).toBe(
      `/w/${WORKSPACE}/projects/01JB000000000000000PRJ0001/workstreams/01JB000000000000000WST0002`,
    );
  });

  it('shows a not-found page inside the frame for unknown paths', async () => {
    renderApp([], `/w/${WORKSPACE}/nowhere`);
    await screen.findByRole('heading', { level: 1, name: 'Page not found' }, { timeout: 8_000 });
    expect(screen.getByRole('complementary', { name: 'Sidebar' })).toBeTruthy();
  });
});
