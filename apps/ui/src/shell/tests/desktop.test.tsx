// @vitest-environment happy-dom
// @vitest-environment-options {"url": "http://localhost:5173/"}
// The shell in the desktop app: workspaces from a fake gateway (Tauri's IPC mocked, following
// docs/build/contracts/desktop-gateway.md), each with its own daemon and its own data.

import { cleanup, fireEvent, render, screen, within } from '@testing-library/react';
import { createMemoryHistory, RouterProvider } from '@tanstack/react-router';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { FakeGateway, helloOnStream, tinyDaemon, type Daemon } from '../../../tests/fake-gateway.ts';
import { WorkspacesProvider } from '../../data/desktop.tsx';
import { createGateway } from '../../data/gateway.ts';
import type { GatewayWorkspace } from '../../data/workspaces.tsx';
import { createAppRouter } from '../routes.tsx';
import { initialShellState, useShell } from '../store.ts';

const ALPHA = '01JB000000000000000WSPALPH';
const BETA = '01JB000000000000000WSPBETA';
const GAMMA = '01JB000000000000000WSPGAMM';

const alpha: GatewayWorkspace = { id: ALPHA, name: 'Alpha Lab', kind: 'local', state: 'ready' };
const beta: GatewayWorkspace = { id: BETA, name: 'Beta Lab', kind: 'remote', state: 'ready' };

const PATIENCE = { timeout: 8_000 };

let gateway: FakeGateway;

beforeEach(() => {
  gateway = new FakeGateway([alpha, beta]).install();
  gateway.daemons.set(ALPHA, tinyDaemon({ id: ALPHA, name: 'Alpha Lab' }, [{ id: 'PRJA', key: 'ALP', name: 'Alpha project' }]));
  gateway.daemons.set(BETA, tinyDaemon({ id: BETA, name: 'Beta Lab' }, [{ id: 'PRJB', key: 'BET', name: 'Beta project' }]));
  helloOnStream(gateway);
});

afterEach(() => {
  cleanup();
  gateway.uninstall();
  localStorage.clear();
  useShell.setState(initialShellState);
});

function renderDesktop(path = '/') {
  const router = createAppRouter([], { history: createMemoryHistory({ initialEntries: [path] }) });
  render(
    <WorkspacesProvider gateway={createGateway()}>
      <RouterProvider router={router} />
    </WorkspacesProvider>,
  );
  return router;
}

const sidebar = () => screen.getByRole('complementary', { name: 'Sidebar' });

async function openSwitcher(name: string): Promise<HTMLElement> {
  // The Radix trigger opens on a real keyboard activation; a synthetic click alone does not.
  fireEvent.keyDown(within(sidebar()).getByRole('button', { name: `Workspace: ${name}` }), { key: 'Enter' });
  return screen.findByRole('menu');
}

/** Holds a daemon's answers until `release()`. */
function held(daemon: Daemon): { daemon: Daemon; release(): void } {
  let release = () => {};
  const gate = new Promise<void>((done) => (release = done));
  return {
    daemon: async (req) => {
      await gate;
      return daemon(req);
    },
    release,
  };
}

describe('the shell in the desktop app', () => {
  it("opens a workspace from the gateway's list, and the switcher follows gateway://workspaces", async () => {
    const router = renderDesktop('/');
    await screen.findByRole('heading', { level: 1, name: 'Home' }, PATIENCE);
    expect(router.state.location.pathname).toBe(`/w/${ALPHA}/home`);
    await screen.findByRole('link', { name: 'Alpha project' });

    let menu = await openSwitcher('Alpha Lab');
    expect(within(menu).getAllByRole('menuitemradio').map((i) => i.textContent)).toEqual(['Alpha Lab', 'Beta Lab']);
    fireEvent.keyDown(menu, { key: 'Escape' });

    await gateway.setWorkspaces([
      alpha,
      { ...beta, state: 'unreachable', detail: 'SSH timed out.' },
      { id: GAMMA, name: 'Gamma Lab', kind: 'remote', state: 'needs_pairing' },
    ]);
    menu = await openSwitcher('Alpha Lab');
    await vi.waitFor(() =>
      expect(within(menu).getAllByRole('menuitemradio').map((i) => i.textContent)).toEqual([
        'Alpha Lab',
        'Beta Lab · Unreachable',
        'Gamma Lab · Needs pairing',
      ]),
    );
    expect(within(menu).getByRole('menuitemradio', { name: 'Alpha Lab' }).getAttribute('aria-checked')).toBe('true');
  });

  it("keeps each workspace's data apart when switching", async () => {
    const slow = held(tinyDaemon({ id: BETA, name: 'Beta Lab' }, [{ id: 'PRJB', key: 'BET', name: 'Beta project' }]));
    gateway.daemons.set(BETA, slow.daemon);
    const router = renderDesktop(`/w/${ALPHA}/home`);
    await screen.findByRole('link', { name: 'Alpha project' }, PATIENCE);

    const menu = await openSwitcher('Alpha Lab');
    fireEvent.click(within(menu).getByRole('menuitemradio', { name: 'Beta Lab' }));
    await vi.waitFor(() => expect(router.state.location.pathname).toBe(`/w/${BETA}/home`));
    // While Beta's daemon has not answered, nothing of Alpha's shows under Beta.
    await screen.findByRole('heading', { level: 1, name: 'Home' });
    expect(within(sidebar()).getByRole('button', { name: 'Workspace: Beta Lab' })).toBeTruthy();
    expect(screen.queryByText('Alpha project')).toBeNull();
    const breadcrumb = screen.getByRole('navigation', { name: 'Breadcrumb' });
    expect(within(breadcrumb).getByText('Beta Lab')).toBeTruthy();
    expect(within(breadcrumb).queryByText('Alpha Lab')).toBeNull();

    slow.release();
    await screen.findByRole('link', { name: 'Beta project' }, PATIENCE);
    expect(screen.queryByText('Alpha project')).toBeNull();
    // Every request for Beta went to Beta.
    const betaRequests = gateway.calls.filter(
      (c) => c.cmd === 'gateway_request' && (c.args.req as { path: string }).path === '/v1/projects',
    );
    expect(betaRequests.map((c) => (c.args.req as { workspace: string }).workspace)).toEqual([ALPHA, BETA]);

    // Back to Alpha: its own data, from its own cache; Beta's is gone from the screen.
    await router.navigate({ href: `/w/${ALPHA}/home` });
    await screen.findByRole('link', { name: 'Alpha project' }, PATIENCE);
    expect(screen.queryByText('Beta project')).toBeNull();
    // Each workspace has its own stream.
    expect(gateway.socketsFor(ALPHA).map((s) => s.path)).toEqual(['/v1/stream']);
    expect(gateway.socketsFor(BETA).map((s) => s.path)).toEqual(['/v1/stream']);
  });

  it('shows why a workspace is unreachable or needs pairing, and its pages once it is back', async () => {
    gateway.workspaces = [alpha, { ...beta, state: 'unreachable', detail: 'SSH to hpc-login timed out.' }];
    renderDesktop(`/w/${BETA}/home`);

    await screen.findByRole('heading', { level: 1, name: 'Cannot reach Beta Lab' }, PATIENCE);
    expect(screen.getByTestId('workspace-detail').textContent).toBe('SSH to hpc-login timed out.');
    expect(screen.getByTestId('stream-status').textContent).toBe('Unreachable');
    // The frame stays, so another workspace is one click away.
    expect(within(sidebar()).getByRole('button', { name: 'Workspace: Beta Lab' })).toBeTruthy();
    expect(screen.queryByText('Loading…')).toBeNull();

    await gateway.setWorkspaces([alpha, { ...beta, state: 'needs_pairing', detail: 'The token was revoked.' }]);
    await screen.findByRole('heading', { level: 1, name: 'Beta Lab needs pairing' });
    expect(screen.getByTestId('stream-status').textContent).toBe('Needs pairing');

    // Paired again: the stream reconnects at once and the page comes back.
    await gateway.setWorkspaces([alpha, beta]);
    await screen.findByRole('link', { name: 'Beta project' }, PATIENCE);
    expect(screen.queryByTestId('workspace-unavailable')).toBeNull();
  });

  it('opens the workspace last opened, and says when a workspace is not in the list', async () => {
    useShell.setState({ lastWorkspace: BETA });
    const router = renderDesktop('/');
    await screen.findByRole('link', { name: 'Beta project' }, PATIENCE);
    expect(router.state.location.pathname).toBe(`/w/${BETA}/home`);

    await router.navigate({ href: '/w/01JB000000000000000WSPNONE/home' });
    await screen.findByRole('heading', { level: 1, name: 'Page not found' });
  });

  it('says so when there are no workspaces yet', async () => {
    gateway.workspaces = [];
    renderDesktop('/');
    await screen.findByText('No workspaces yet.', undefined, PATIENCE);
  });
});
