// @vitest-environment happy-dom
// @vitest-environment-options {"url": "http://localhost:5173/"}
// The first-run route itself, in the desktop app over a fake gateway: a remote hub reaching it
// through the redirect starts from the gateway's name for its machine, and checks and signs in on
// that machine through its hub; Done replaces the wizard with Home; and a workspace already set up goes Home at once (unless the development flag asks
// for the fake wizard).

import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { createMemoryHistory, RouterProvider } from '@tanstack/react-router';
import { StrictMode } from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { createApi } from '../data/api.ts';
import { WorkspacesProvider } from '../data/desktop.tsx';
import { createGateway, gatewayTransport } from '../data/gateway.ts';
import { FakeDesktop, freshDaemon } from '../data/tests/fake-desktop.ts';
import type { GatewayWorkspace } from '../data/workspaces.tsx';
import { paths } from '../shell/paths.ts';
import { createAppRouter } from '../shell/routes.tsx';
import { initialShellState, useShell } from '../shell/store.ts';
import { feature as onboarding } from './index.ts';

const WS = '01JB000000000000000WSPFRSH';
const REMOTE: GatewayWorkspace = { id: WS, name: 'hpc-login', kind: 'remote', state: 'ready' };
const PATIENCE = { timeout: 8_000 };

let desktop: FakeDesktop;
let fresh = freshDaemon(WS);

beforeEach(() => {
  desktop = new FakeDesktop({ workspaces: [REMOTE] }).install();
  fresh = freshDaemon(WS);
  desktop.daemons.set(WS, fresh.daemon);
});

afterEach(() => {
  cleanup();
  desktop.uninstall();
  localStorage.clear();
  useShell.setState(initialShellState);
});

function renderApp(path: string) {
  const router = createAppRouter([onboarding], { history: createMemoryHistory({ initialEntries: [path] }) });
  render(
    <StrictMode>
      <WorkspacesProvider gateway={createGateway()}>
        <RouterProvider router={router} />
      </WorkspacesProvider>
    </StrictMode>,
  );
  return router;
}

const heading = (name: string | RegExp) => screen.findByRole('heading', { level: 1, name }, PATIENCE);

describe('the first-run route', () => {
  it("starts a remote hub's machine name from the gateway's name, and replaces itself with Home", async () => {
    const own = '01JB000000000000000MAC0001';
    desktop.daemons.set(WS, (req) => {
      // Machine setup on the remote hub's own machine (the remote one), through the gateway.
      if (req.method === 'GET' && req.path === `/v1/machines/${own}/check`) {
        return { status: 200, body: JSON.stringify({ rows: [{ id: 'git', status: 'ok', detail: 'git version 2.43.0' }] }) };
      }
      if (req.method === 'GET' && req.path === `/v1/machines/${own}/agents`) {
        return { status: 200, body: JSON.stringify([{ engine: 'codex', installed: true, signed_in: true, account: 'ChatGPT' }]) };
      }
      if (req.method === 'POST' && req.path === '/v1/import/dry-run') {
        return { status: 200, body: '{"count":0}' };
      }
      if (req.method === 'PUT' && req.path === '/v1/import') {
        return { status: 200, body: '{"imported":0}' };
      }
      return fresh.daemon(req);
    });
    const router = renderApp('/');
    await heading('Welcome to PitCrew');
    expect(router.state.location.pathname).toBe(paths.setup(WS));
    fireEvent.click(screen.getByRole('button', { name: 'Get started' }));
    await heading('Your first workspace');
    const machine = screen.getByLabelText('The remote machine’s name') as HTMLInputElement;
    expect(machine.value).toBe('hpc-login');
    fireEvent.change(screen.getByLabelText('Workspace name'), { target: { value: 'Cluster Lab' } });
    fireEvent.change(screen.getByLabelText('Your name'), { target: { value: 'Sam Rivera' } });
    fireEvent.click(screen.getByRole('button', { name: 'Continue' }));

    // Its machine check and sign-in are the remote machine's, through its hub.
    await heading('Checking the machine');
    await screen.findByText('git version 2.43.0', undefined, PATIENCE);
    fireEvent.click(screen.getByRole('button', { name: 'Continue' }));
    await heading('Sign in to your agents');
    await screen.findByText('ChatGPT', undefined, PATIENCE);
    expect(screen.getByText(/in a terminal on hpc-login/)).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: 'Skip for now' }));

    await heading('Import sessions');
    await screen.findByText('This will import 0 sessions.');
    fireEvent.click(screen.getByRole('button', { name: 'Continue' }));
    await heading("You're set up");
    const requests = desktop.commands('gateway_request').map((args) => args.req as { workspace: string; method: string; path: string; body?: string });
    const previews = requests.filter((req) => req.path === '/v1/import/dry-run');
    expect(previews.length).toBeGreaterThan(0);
    for (const req of previews) {
      expect([req.workspace, req.method, JSON.parse(req.body ?? '{}')]).toEqual([WS, 'POST', { mode: 'all' }]);
    }
    expect(requests.filter((req) => req.path === '/v1/import').map((req) =>
      [req.workspace, req.method, JSON.parse(req.body ?? '{}')])).toEqual([[WS, 'PUT', { mode: 'all' }]]);
    expect(requests.some((req) => req.path.endsWith('/scan') || req.method === 'POST' && req.path === '/v1/projects')).toBe(false);
    expect(fresh.setups).toEqual([
      { workspace_name: 'Cluster Lab', person: { name: 'Sam Rivera', handle: '@sam' }, machine_name: 'hpc-login' },
    ]);
    fireEvent.click(screen.getByRole('button', { name: 'Go to Home' }));
    await heading('Home');
    // Every step on the way replaced the one before: Back cannot reopen the finished wizard.
    expect(router.history.canGoBack()).toBe(false);
  });

  it('sends a workspace that is set up already Home', async () => {
    await createApi({ transport: gatewayTransport(WS) }).setup({
      workspace_name: 'Cluster Lab',
      person: { name: 'Sam Rivera', handle: '@sam' },
      machine_name: 'hpc-login',
    });
    const router = renderApp(paths.setup(WS));
    await heading('Home');
    expect(router.state.location.pathname).toBe(paths.home(WS));
    expect(fresh.setups).toHaveLength(1);
  });

  it('shows the fake wizard on a set-up workspace when the development flag asks for it', async () => {
    await createApi({ transport: gatewayTransport(WS) }).setup({
      workspace_name: 'Cluster Lab',
      person: { name: 'Sam Rivera', handle: '@sam' },
      machine_name: 'hpc-login',
    });
    const router = renderApp(`${paths.setup(WS)}?onboarding=fake`);
    await heading('Welcome to PitCrew');
    await vi.waitFor(() => expect(screen.getByRole('note').textContent).toContain('against a fake'));
    expect(router.state.location.pathname).toBe(paths.setup(WS));
  });
});
