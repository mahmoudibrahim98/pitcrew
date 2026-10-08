// @vitest-environment happy-dom
// @vitest-environment-options {"url": "http://localhost:5173/"}

import { cleanup, fireEvent, render, screen, within, waitFor } from '@testing-library/react';
import type { QueryClient } from '@tanstack/react-query';
import { createMemoryHistory, createRoute, RouterProvider } from '@tanstack/react-router';
import { afterAll, afterEach, beforeAll, describe, expect, it, vi } from 'vitest';
import { freePort, spawnHub, type HubProcess } from '../../../tests/hub-process.ts';
import { createApi } from '../../data/api.ts';
import { createQueryClient, DataProvider } from '../../data/provider.tsx';
import type { SocketFactory } from '../../data/stream.ts';
import { defineFeature, type Feature } from '../feature.ts';
import { createAppRouter } from '../routes.tsx';
import { initialShellState, useShell } from '../store.ts';

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
  // Abort fetches still in flight, so none is cut off when the hub stops.
  await queryClient?.cancelQueries();
  queryClient?.clear();
  localStorage.clear();
  useShell.setState(initialShellState);
  vi.restoreAllMocks();
});

function renderApp(features: Feature[], path = '/', socket?: SocketFactory) {
  const router = createAppRouter(features, { history: createMemoryHistory({ initialEntries: [path] }) });
  const api = createApi({ baseUrl: hub.url, token: TOKEN });
  queryClient = createQueryClient();
  render(
    <DataProvider
      api={api}
      queryClient={queryClient}
      token={TOKEN}
      {...(socket === undefined ? {} : { socket })}
    >
      <RouterProvider router={router} />
    </DataProvider>,
  );
  return router;
}

/** A stream that never says hello, so every live query stays loading. */
const silentStream: SocketFactory = () => ({ onmessage: null, onclose: null, onerror: null, close: () => undefined });

const sidebar = () => screen.getByRole('complementary', { name: 'Sidebar' });

describe('feature registration', () => {
  it('puts recently visited live items first in the palette and opens one with Enter', async () => {
    vi.spyOn(HTMLElement.prototype, 'offsetHeight', 'get').mockReturnValue(432);
    vi.spyOn(HTMLElement.prototype, 'offsetWidth', 'get').mockReturnValue(640);
    const router = renderApp([]);
    await screen.findByRole('heading', { level: 1, name: 'Home' }, { timeout: 8_000 });
    await router.navigate({ href: `/w/${WORKSPACE}/projects/01JB000000000000000PRJ0001` });
    await screen.findByRole('heading', { level: 1, name: 'Paper · Diffusion study' });
    await router.navigate({ href: `/w/${WORKSPACE}/home` });
    await screen.findByRole('heading', { level: 1, name: 'Home' });
    fireEvent.keyDown(window, { key: 'k', ctrlKey: true });
    const search = await screen.findByRole('combobox', { name: 'Search' });
    await waitFor(() => expect(screen.getAllByRole('option')[0]?.textContent).toContain('Recent · Project'));
    fireEvent.keyDown(search, { key: 'Enter' });
    await screen.findByRole('heading', { level: 1, name: 'Paper · Diffusion study' });
    expect(router.state.location.pathname).toBe(`/w/${WORKSPACE}/projects/01JB000000000000000PRJ0001`);
    expect(screen.queryByRole('combobox')).toBeNull();
  });
  it('leaves Home and End to the search text: they move its caret, not the chosen result', async () => {
    vi.spyOn(HTMLElement.prototype, 'offsetHeight', 'get').mockReturnValue(432);
    vi.spyOn(HTMLElement.prototype, 'offsetWidth', 'get').mockReturnValue(640);
    renderApp([]);
    await screen.findByRole('heading', { level: 1, name: 'Home' }, { timeout: 8_000 });
    fireEvent.keyDown(window, { key: 'k', ctrlKey: true });
    const search = await screen.findByRole('combobox', { name: 'Search' });
    await waitFor(() => expect(screen.getAllByRole('option').length).toBeGreaterThan(2));
    const chosen = () => screen.getAllByRole('option').findIndex((o) => o.getAttribute('aria-selected') === 'true');
    fireEvent.keyDown(search, { key: 'ArrowDown' });
    expect(chosen()).toBe(1);
    // Not prevented, so the browser moves the caret; the chosen result stays.
    expect(fireEvent.keyDown(search, { key: 'End' })).toBe(true);
    expect(fireEvent.keyDown(search, { key: 'Home' })).toBe(true);
    expect(chosen()).toBe(1);
    // The arrows still move it, and are taken from the text field.
    expect(fireEvent.keyDown(search, { key: 'ArrowDown' })).toBe(false);
    expect(chosen()).toBe(2);
  });
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

  it('shows the loading page while a workstream known only by id loads', async () => {
    renderApp([], `/w/${WORKSPACE}/workstreams/01JB000000000000000WST0002`, silentStream);
    await screen.findByRole('heading', { level: 1, name: 'Loading…' }, { timeout: 8_000 });
  });

  it('shows a not-found page inside the frame for unknown paths', async () => {
    renderApp([], `/w/${WORKSPACE}/nowhere`);
    await screen.findByRole('heading', { level: 1, name: 'Page not found' }, { timeout: 8_000 });
    expect(screen.getByRole('complementary', { name: 'Sidebar' })).toBeTruthy();
  });
});
