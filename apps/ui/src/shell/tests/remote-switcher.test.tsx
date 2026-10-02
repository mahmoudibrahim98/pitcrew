// @vitest-environment happy-dom
// @vitest-environment-options {"url": "http://localhost:5173/"}
// Remote workspaces in the switcher (desktop only): "Connect a remote machine…" opens the connect
// route, "Remove workspace…" (remote ones only) asks first, with the option to stop the remote's
// helper; the "No workspaces yet" screen offers to connect; a browser says it cannot.

import { cleanup, fireEvent, render, screen, within } from '@testing-library/react';
import type { QueryClient } from '@tanstack/react-query';
import { createMemoryHistory, createRoute, RouterProvider } from '@tanstack/react-router';
import { afterAll, afterEach, beforeAll, beforeEach, describe, expect, it, vi } from 'vitest';
import { tinyDaemon } from '../../../tests/fake-gateway.ts';
import { freePort, spawnHub, type HubProcess } from '../../../tests/hub-process.ts';
import { createApi } from '../../data/api.ts';
import { WorkspacesProvider } from '../../data/desktop.tsx';
import { createGateway } from '../../data/gateway.ts';
import { createQueryClient, DataProvider } from '../../data/provider.tsx';
import { FakeDesktop, refuse, remoteWorkspace } from '../../data/tests/fake-desktop.ts';
import type { GatewayWorkspace } from '../../data/workspaces.tsx';
import { defineFeature } from '../feature.ts';
import { paths } from '../paths.ts';
import { createAppRouter } from '../routes.tsx';
import { initialShellState, useShell } from '../store.ts';

const ALPHA = '01JB000000000000000WSPALPH';
const BETA = '01JB000000000000000WSPBETA';
const alpha: GatewayWorkspace = { id: ALPHA, name: 'Alpha Lab', kind: 'local', state: 'ready' };
const beta = remoteWorkspace(BETA, 'hpc-login', 'login.example.org');
const PATIENCE = { timeout: 8_000 };

/** A stand-in for the onboarding feature's connect wizard at `paths.connect()`. */
const connectStub = defineFeature({
  id: 'connect-stub',
  layout: 'both',
  rootRoutes: (root) => [
    createRoute({
      getParentRoute: () => root,
      path: 'connect',
      component: function ConnectStub() {
        return <h1>Connect stub</h1>;
      },
    }),
  ],
});

const sidebar = () => screen.getByRole('complementary', { name: 'Sidebar' });

async function openSwitcher(name: string): Promise<HTMLElement> {
  fireEvent.keyDown(within(sidebar()).getByRole('button', { name: `Workspace: ${name}` }), { key: 'Enter' });
  return screen.findByRole('menu');
}

afterEach(() => {
  localStorage.clear();
  useShell.setState(initialShellState);
});

describe('in the desktop app', () => {
  let desktop: FakeDesktop;

  beforeEach(() => {
    desktop = new FakeDesktop({ workspaces: [alpha, beta] }).install();
    desktop.daemons.set(ALPHA, tinyDaemon({ id: ALPHA, name: 'Alpha Lab' }, [{ id: 'PRJA', key: 'ALP', name: 'Alpha project' }]));
    desktop.daemons.set(BETA, tinyDaemon({ id: BETA, name: 'hpc-login' }, [{ id: 'PRJB', key: 'BET', name: 'Beta project' }]));
  });

  afterEach(() => {
    cleanup();
    desktop.uninstall();
  });

  function renderDesktop(path: string) {
    const router = createAppRouter([connectStub], { history: createMemoryHistory({ initialEntries: [path] }) });
    render(
      <WorkspacesProvider gateway={createGateway()}>
        <RouterProvider router={router} />
      </WorkspacesProvider>,
    );
    return router;
  }

  it('distinguishes a remote hub impersonating the local name by its trusted host', async () => {
    desktop.workspaces = [alpha, { ...beta, name: alpha.name }];
    desktop.daemons.set(BETA, tinyDaemon({ id: BETA, name: alpha.name }, []));
    renderDesktop(paths.home(BETA));
    await screen.findByRole('heading', { level: 1, name: 'Home' }, PATIENCE);
    expect(screen.getByRole('navigation', { name: 'Breadcrumb' }).textContent).toContain('Alpha Lab · login.example.org');
    const menu = await openSwitcher('Alpha Lab · login.example.org');
    expect(within(menu).getByRole('menuitemradio', { name: 'Alpha Lab · login.example.org' })).toBeTruthy();
    expect(within(menu).getByRole('menuitemradio', { name: /^Alpha Lab$/ }).textContent).not.toContain('login.example.org');
  });

  it('keeps the host and state in an unreachable remote menu name', async () => {
    desktop.workspaces = [alpha, { ...beta, state: 'unreachable' }];
    renderDesktop(paths.home(ALPHA));
    await screen.findByRole('link', { name: 'Alpha project' }, PATIENCE);
    const menu = await openSwitcher('Alpha Lab');
    expect(within(menu).getByRole('menuitemradio', { name: 'hpc-login · login.example.org · Unreachable' })).toBeTruthy();
  });

  it('opens the connect wizard from the switcher', async () => {
    const router = renderDesktop(paths.home(ALPHA));
    await screen.findByRole('link', { name: 'Alpha project' }, PATIENCE);
    const menu = await openSwitcher('Alpha Lab');
    // The local workspace is this machine's own: it cannot be removed.
    expect(within(menu).queryByRole('menuitem', { name: 'Remove workspace…' })).toBeNull();
    fireEvent.click(within(menu).getByRole('menuitem', { name: 'Connect a remote machine…' }));
    await screen.findByRole('heading', { level: 1, name: 'Connect stub' });
    expect(router.state.location.pathname).toBe('/connect');
  });

  it('removes a remote workspace after asking, stopping its helper if asked, then opens another', async () => {
    desktop.remove = async () => {
      await desktop.setWorkspaces([alpha]);
      return null;
    };
    const router = renderDesktop(paths.home(BETA));
    await screen.findByRole('link', { name: 'Beta project' }, PATIENCE);

    // Cancel first: nothing is removed.
    let menu = await openSwitcher('hpc-login · login.example.org');
    fireEvent.click(within(menu).getByRole('menuitem', { name: 'Remove workspace…' }));
    let dialog = await screen.findByRole('dialog', { name: 'Remove hpc-login · login.example.org?' });
    fireEvent.click(within(dialog).getByRole('button', { name: 'Cancel' }));
    await vi.waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
    expect(desktop.commands('gateway_workspace_remove')).toEqual([]);
    // Focus goes back to the switcher, not to the page's body.
    await vi.waitFor(() =>
      expect(document.activeElement).toBe(within(sidebar()).getByRole('button', { name: 'Workspace: hpc-login · login.example.org' })),
    );

    menu = await openSwitcher('hpc-login · login.example.org');
    fireEvent.click(within(menu).getByRole('menuitem', { name: 'Remove workspace…' }));
    dialog = await screen.findByRole('dialog', { name: 'Remove hpc-login · login.example.org?' });
    const stop = within(dialog).getByRole('checkbox', { name: 'Also stop PitCrew on the remote (cancels its SLURM job)' });
    expect((stop as HTMLInputElement).checked).toBe(false);
    fireEvent.click(stop);
    fireEvent.click(within(dialog).getByRole('button', { name: 'Remove' }));

    await vi.waitFor(() => expect(router.state.location.pathname).toBe(paths.home(ALPHA)), PATIENCE);
    expect(desktop.commands('gateway_workspace_remove')).toEqual([{ workspace: BETA, stopHelper: true }]);
    await screen.findByRole('link', { name: 'Alpha project' }, PATIENCE);
    expect(screen.queryByRole('dialog')).toBeNull();
  });

  it('keeps the dialog open with the reason when the gateway cannot remove it', async () => {
    desktop.remove = () => refuse('unreachable', 'ssh: hpc-login: connection timed out');
    const router = renderDesktop(paths.home(BETA));
    await screen.findByRole('link', { name: 'Beta project' }, PATIENCE);
    const menu = await openSwitcher('hpc-login · login.example.org');
    fireEvent.click(within(menu).getByRole('menuitem', { name: 'Remove workspace…' }));
    const dialog = await screen.findByRole('dialog', { name: 'Remove hpc-login · login.example.org?' });
    fireEvent.click(within(dialog).getByRole('button', { name: 'Remove' }));
    expect((await within(dialog).findByRole('alert')).textContent).toBe('ssh: hpc-login: connection timed out');
    expect(desktop.commands('gateway_workspace_remove')).toEqual([{ workspace: BETA, stopHelper: false }]);
    expect(router.state.location.pathname).toBe(paths.home(BETA));
  });

  it('offers Retry on an unreachable remote workspace', async () => {
    desktop.workspaces = [alpha, { ...beta, state: 'unreachable', detail: 'The sign-in was cancelled.' }];
    desktop.retry = async () => {
      await desktop.setWorkspaces([alpha, beta]);
      return null;
    };
    renderDesktop(paths.home(BETA));
    await screen.findByRole('heading', { level: 1, name: 'Cannot reach hpc-login · login.example.org' }, PATIENCE);
    fireEvent.click(screen.getByRole('button', { name: 'Retry' }));
    await screen.findByRole('link', { name: 'Beta project' }, PATIENCE);
    expect(desktop.commands('gateway_workspace_retry')).toEqual([{ workspace: BETA }]);
  });

  it('leaves a workspace the gateway drops while it is on screen: for the next one, or `/`', async () => {
    const router = renderDesktop(paths.home(BETA));
    await screen.findByRole('link', { name: 'Beta project' }, PATIENCE);
    await desktop.setWorkspaces([alpha]);
    await vi.waitFor(() => expect(router.state.location.pathname).toBe(paths.home(ALPHA)), PATIENCE);
    await screen.findByRole('link', { name: 'Alpha project' }, PATIENCE);

    await desktop.setWorkspaces([]);
    await screen.findByRole('heading', { level: 1, name: 'No workspaces yet.' }, PATIENCE);
    expect(router.state.location.pathname).toBe('/');
  });

  it('offers to connect a machine when there are no workspaces yet', async () => {
    desktop.workspaces = [];
    const router = renderDesktop('/');
    await screen.findByText('No workspaces yet.', undefined, PATIENCE);
    fireEvent.click(screen.getByRole('button', { name: 'Connect a remote machine…' }));
    await screen.findByRole('heading', { level: 1, name: 'Connect stub' });
    expect(router.state.location.pathname).toBe('/connect');
  });
});

describe('in a browser', () => {
  let hub: HubProcess;
  let queryClient: QueryClient | undefined;

  beforeAll(async () => {
    hub = await spawnHub(await freePort());
  });

  afterAll(async () => {
    await hub.close();
  });

  afterEach(async () => {
    cleanup();
    await queryClient?.cancelQueries();
    queryClient?.clear();
  });

  it('says connecting a machine needs the desktop app, and offers no removal', async () => {
    const router = createAppRouter([connectStub], { history: createMemoryHistory({ initialEntries: ['/'] }) });
    queryClient = createQueryClient();
    render(
      <DataProvider api={createApi({ baseUrl: hub.url, token: 'dev-device-token' })} queryClient={queryClient} token="dev-device-token">
        <RouterProvider router={router} />
      </DataProvider>,
    );
    await screen.findByRole('heading', { level: 1, name: 'Home' }, PATIENCE);
    const menu = await openSwitcher('Demo Lab');
    const item = within(menu).getByRole('menuitem', { name: 'Connect a remote machine (desktop app only)' });
    expect(item.getAttribute('aria-disabled')).toBe('true');
    expect(within(menu).queryByRole('menuitem', { name: 'Remove workspace…' })).toBeNull();
  });
});
