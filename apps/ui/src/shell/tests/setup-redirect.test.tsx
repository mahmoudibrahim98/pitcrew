// @vitest-environment happy-dom
// @vitest-environment-options {"url": "http://localhost:5173/"}
// A workspace that needs setup opens the first-run route, in a browser (a fresh mock hub) and in
// the desktop app (a fake gateway), without a loop: the setup route itself is exempt, and a
// finished setup lets the person stay on Home.

import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import type { QueryClient } from '@tanstack/react-query';
import { createMemoryHistory, createRoute, RouterProvider, useRouter, type AnyRouter } from '@tanstack/react-router';
import { afterAll, afterEach, beforeAll, describe, expect, it, vi } from 'vitest';
import { freePort, spawnHub, type HubProcess } from '../../../tests/hub-process.ts';
import { createApi } from '../../data/api.ts';
import { WorkspacesProvider } from '../../data/desktop.tsx';
import { createGateway, gatewayTransport } from '../../data/gateway.ts';
import { createQueryClient, DataProvider } from '../../data/provider.tsx';
import { useSetup } from '../../data/setup.ts';
import { FakeDesktop, freshDaemon } from '../../data/tests/fake-desktop.ts';
import { defineFeature } from '../feature.ts';
import { useWorkspaceId } from '../layout.ts';
import { paths } from '../paths.ts';
import { createAppRouter } from '../routes.tsx';
import { initialShellState, useShell } from '../store.ts';

const TOKEN = 'dev-device-token';
const PATIENCE = { timeout: 8_000 };

/** A stand-in first-run wizard at `paths.setup`: one button that sets the workspace up. */
const setupStub = defineFeature({
  id: 'setup-stub',
  layout: 'both',
  routes: (parent) => [
    createRoute({
      getParentRoute: () => parent,
      path: 'onboarding',
      staticData: { setup: true },
      component: function SetupStub() {
        const ws = useWorkspaceId();
        const router = useRouter();
        const setup = useSetup();
        return (
          <>
            <h1>Set up stub</h1>
            <button
              type="button"
              onClick={() =>
                void setup
                  .mutateAsync({
                    workspace_name: 'Demo Lab',
                    person: { name: 'Sam Rivera', handle: '@sam' },
                    machine_name: 'This laptop',
                  })
                  .then(() => router.navigate({ href: paths.home(ws) }))
              }
            >
              Finish setup
            </button>
          </>
        );
      },
    }),
  ],
});

/** Every location the router resolves, in order. */
function track(router: AnyRouter): string[] {
  const seen: string[] = [];
  router.subscribe('onResolved', (event) => seen.push(event.toLocation.pathname));
  return seen;
}

const settle = () => new Promise((done) => setTimeout(done, 300));

afterEach(() => {
  cleanup();
  localStorage.clear();
  useShell.setState(initialShellState);
});

describe('in a browser, against a fresh hub', () => {
  let hub: HubProcess;
  let queryClient: QueryClient | undefined;

  beforeAll(async () => {
    process.env.PITCREW_MOCK_FRESH = '1';
    try {
      hub = await spawnHub(await freePort());
    } finally {
      delete process.env.PITCREW_MOCK_FRESH;
    }
  });

  afterAll(async () => {
    await queryClient?.cancelQueries();
    queryClient?.clear();
    await hub.close();
  });

  it('sends every page to setup until it is done, then stays on Home', async () => {
    const api = createApi({ baseUrl: hub.url, token: TOKEN });
    const ws = (await api.workspace()).workspace.id;
    const router = createAppRouter([setupStub], { history: createMemoryHistory({ initialEntries: ['/'] }) });
    const seen = track(router);
    queryClient = createQueryClient();
    render(
      <DataProvider api={api} queryClient={queryClient} token={TOKEN}>
        <RouterProvider router={router} />
      </DataProvider>,
    );

    await screen.findByRole('heading', { level: 1, name: 'Set up stub' }, PATIENCE);
    expect(router.state.location.pathname).toBe(paths.setup(ws));
    // Shown bare: no sidebar, no top bar.
    expect(screen.queryByRole('complementary', { name: 'Sidebar' })).toBeNull();
    // The setup route is exempt: it stays put.
    await settle();
    expect(seen.filter((p) => p === paths.setup(ws))).toHaveLength(1);

    // Any other page goes back to setup.
    await router.navigate({ href: paths.inbox(ws) });
    await vi.waitFor(() => expect(router.state.location.pathname).toBe(paths.setup(ws)));

    fireEvent.click(screen.getByRole('button', { name: 'Finish setup' }));
    await screen.findByRole('heading', { level: 1, name: 'Home' }, PATIENCE);
    const after = seen.length;
    await settle();
    expect(router.state.location.pathname).toBe(paths.home(ws));
    expect(seen.slice(after)).toEqual([]);
    expect((await api.me()).name).toBe('Sam Rivera');
    expect((await api.workspace()).setup_needed).toBeUndefined();
  });
});

describe('in the desktop app', () => {
  const WS = '01JB000000000000000WSPFRSH';
  let desktop: FakeDesktop;

  afterEach(() => {
    // Unmount first: the registry stops listening while Tauri's mocks are still there.
    cleanup();
    desktop.uninstall();
  });

  it('sends a fresh workspace to setup, and lets it go once set up', async () => {
    desktop = new FakeDesktop({ workspaces: [{ id: WS, name: 'hpc-login', kind: 'remote', state: 'ready' }] }).install();
    const fresh = freshDaemon(WS);
    desktop.daemons.set(WS, fresh.daemon);
    const router = createAppRouter([setupStub], { history: createMemoryHistory({ initialEntries: ['/'] }) });
    const seen = track(router);
    render(
      <WorkspacesProvider gateway={createGateway()}>
        <RouterProvider router={router} />
      </WorkspacesProvider>,
    );

    await screen.findByRole('heading', { level: 1, name: 'Set up stub' }, PATIENCE);
    expect(router.state.location.pathname).toBe(paths.setup(WS));
    await settle();
    expect(seen.filter((p) => p === paths.setup(WS))).toHaveLength(1);
    // Not a dead end: the switcher stays, with its connect and remove actions.
    expect(screen.getByRole('button', { name: 'Workspace: hpc-login' })).toBeTruthy();
    // And `/` will not reopen a workspace still waiting for setup.
    expect(useShell.getState().lastWorkspace).toBeNull();

    fireEvent.click(screen.getByRole('button', { name: 'Finish setup' }));
    await screen.findByRole('heading', { level: 1, name: 'Home' }, PATIENCE);
    const after = seen.length;
    await settle();
    expect(router.state.location.pathname).toBe(paths.home(WS));
    expect(seen.slice(after)).toEqual([]);
    expect(fresh.me?.handle).toBe('@sam');
    expect(useShell.getState().lastWorkspace).toBe(WS);
    // The first-run route is never remembered as a place to come back to.
    expect(JSON.stringify(useShell.getState().workspaces)).not.toContain('onboarding');
  });

  it('does not send a set-up workspace anywhere', async () => {
    desktop = new FakeDesktop({ workspaces: [{ id: WS, name: 'hpc-login', kind: 'remote', state: 'ready' }] }).install();
    const fresh = freshDaemon(WS);
    desktop.daemons.set(WS, fresh.daemon);
    await createApi({ transport: gatewayTransport(WS) }).setup({
      workspace_name: 'Demo Lab',
      person: { name: 'Sam Rivera', handle: '@sam' },
      machine_name: 'This laptop',
    });
    const router = createAppRouter([setupStub], { history: createMemoryHistory({ initialEntries: [paths.inbox(WS)] }) });
    render(
      <WorkspacesProvider gateway={createGateway()}>
        <RouterProvider router={router} />
      </WorkspacesProvider>,
    );
    await screen.findByRole('heading', { level: 1, name: 'Inbox' }, PATIENCE);
    await settle();
    expect(router.state.location.pathname).toBe(paths.inbox(WS));
  });
});
