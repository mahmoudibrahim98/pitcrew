// @vitest-environment happy-dom
// @vitest-environment-options {"url": "http://localhost:5173/"}
//
// `gateway://navigate` (docs/build/contracts/desktop-gateway.md, "Navigation from outside the
// window"): the pure checks (`parseNavigateTarget`, `navigateHref`) and the wiring to the router.

import { createMemoryHistory, RouterProvider } from '@tanstack/react-router';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { FakeGateway } from '../../../tests/fake-gateway.ts';
import { WorkspacesProvider } from '../../data/desktop.tsx';
import { createGateway } from '../../data/gateway.ts';
import type { GatewayWorkspace } from '../../data/workspaces.tsx';
import { navigateHref, parseNavigateTarget, type NavigateTarget } from '../gateway-navigate.ts';
import { createAppRouter } from '../routes.tsx';
import { initialShellState, useShell } from '../store.ts';

/** A compliant, readable 26-character ULID (Crockford base32: no I, L, O or U). */
const ulid = (tag: string): string => `${tag}${'0'.repeat(26 - tag.length)}`;

const ALPHA = ulid('01JBWKSA');
const BETA = ulid('01JBWKSB');
const UNKNOWN = ulid('01JBWKSZ');

const alpha: GatewayWorkspace = { id: ALPHA, name: 'Alpha Lab', kind: 'local', state: 'ready' };
const beta: GatewayWorkspace = { id: BETA, name: 'Beta Lab', kind: 'remote', state: 'ready' };

describe('parseNavigateTarget', () => {
  it('accepts an inbox target without an id, and the other kinds with one', () => {
    expect(parseNavigateTarget({ workspace: ALPHA, kind: 'inbox' })).toEqual({ workspace: ALPHA, kind: 'inbox' });
    for (const kind of ['task', 'session', 'project', 'workstream'] as const) {
      expect(parseNavigateTarget({ workspace: ALPHA, kind, id: 'PAP-4' })).toEqual({ workspace: ALPHA, kind, id: 'PAP-4' });
    }
  });

  it('drops anything that is not that shape', () => {
    expect(parseNavigateTarget(null)).toBeUndefined();
    expect(parseNavigateTarget('pitcrew://w/x/inbox')).toBeUndefined();
    expect(parseNavigateTarget(['not', 'an', 'object'])).toBeUndefined();
    // Not a ULID: too short, lowercase, or a forbidden letter (I, L, O, U).
    expect(parseNavigateTarget({ workspace: 'short', kind: 'inbox' })).toBeUndefined();
    expect(parseNavigateTarget({ workspace: ALPHA.toLowerCase(), kind: 'inbox' })).toBeUndefined();
    expect(parseNavigateTarget({ workspace: ulid('01JBWKSI'), kind: 'inbox' })).toBeUndefined();
    // An unknown kind.
    expect(parseNavigateTarget({ workspace: ALPHA, kind: 'settings', id: 'x' })).toBeUndefined();
    // A kind that is not inbox, without an id (or an empty one).
    expect(parseNavigateTarget({ workspace: ALPHA, kind: 'task' })).toBeUndefined();
    expect(parseNavigateTarget({ workspace: ALPHA, kind: 'task', id: '' })).toBeUndefined();
    // id of the wrong type.
    expect(parseNavigateTarget({ workspace: ALPHA, kind: 'task', id: 4 })).toBeUndefined();
  });
});

describe('navigateHref', () => {
  const workspaces = [alpha, beta];

  it('maps each kind to its path with the shell’s own helpers', () => {
    const target = (kind: NavigateTarget['kind'], id?: string): NavigateTarget =>
      id === undefined ? { workspace: ALPHA, kind } : { workspace: ALPHA, kind, id };
    expect(navigateHref(target('inbox'), workspaces)).toBe(`/w/${ALPHA}/inbox`);
    expect(navigateHref(target('task', 'PAP-4'), workspaces)).toBe(`/w/${ALPHA}/tasks/PAP-4`);
    expect(navigateHref(target('session', 'S1'), workspaces)).toBe(`/w/${ALPHA}/console/S1`);
    expect(navigateHref(target('project', 'PRJ'), workspaces)).toBe(`/w/${ALPHA}/projects/PRJ`);
    expect(navigateHref(target('workstream', 'WK1'), workspaces)).toBe(`/w/${ALPHA}/workstreams/WK1`);
  });

  it('never treats a field as a URL or a path: every id is percent-encoded', () => {
    const href = navigateHref({ workspace: ALPHA, kind: 'task', id: '../etc/passwd' }, workspaces);
    expect(href).toBe(`/w/${ALPHA}/tasks/..%2Fetc%2Fpasswd`);
  });

  it('is undefined for a workspace the gateway does not list', () => {
    expect(navigateHref({ workspace: UNKNOWN, kind: 'inbox' }, workspaces)).toBeUndefined();
    expect(navigateHref({ workspace: UNKNOWN, kind: 'inbox' }, [])).toBeUndefined();
  });
});

describe('wired to the router', () => {
  let gateway: FakeGateway;

  beforeEach(() => {
    gateway = new FakeGateway([alpha, beta]).install();
  });

  afterEach(() => {
    cleanup();
    gateway.uninstall();
    localStorage.clear();
    useShell.setState(initialShellState);
  });

  function renderDesktop(path: string) {
    const router = createAppRouter([], { history: createMemoryHistory({ initialEntries: [path] }) });
    render(
      <WorkspacesProvider gateway={createGateway()}>
        <RouterProvider router={router} />
      </WorkspacesProvider>,
    );
    return router;
  }

  it('navigates to the mapped path for a known workspace', async () => {
    const router = renderDesktop(`/w/${ALPHA}/home`);
    await screen.findByRole('heading', { level: 1, name: 'Home' });
    await gateway.navigate({ workspace: BETA, kind: 'task', id: 'PAP-4' });
    await vi.waitFor(() => expect(router.state.location.pathname).toBe(`/w/${BETA}/tasks/PAP-4`));
  });

  it('holds a target that arrives before the workspace list is first known, and delivers it once the list arrives', async () => {
    // The gateway holds a launch-time deep link until the webview's first `gateway_workspaces()`
    // call, which can race the very read that would answer "is this workspace known?" (reported
    // by stream K): hold that read open, as a slow first answer would.
    gateway.holdWorkspacesRead = true;
    const router = renderDesktop(`/w/${ALPHA}/home`);
    await screen.findByText('Loading workspaces…');
    await gateway.navigate({ workspace: BETA, kind: 'task', id: 'PAP-4' });
    // Still nothing: held, not dropped and not acted on with a guess while the list is unknown.
    await new Promise((done) => setTimeout(done, 20));
    expect(router.state.location.pathname).toBe(`/w/${ALPHA}/home`);

    gateway.resolveWorkspaces();
    await vi.waitFor(() => expect(router.state.location.pathname).toBe(`/w/${BETA}/tasks/PAP-4`));
  });

  it('goes to / with a notice for a workspace the gateway no longer lists', async () => {
    renderDesktop(`/w/${ALPHA}/home`);
    await screen.findByRole('heading', { level: 1, name: 'Home' });
    await gateway.navigate({ workspace: UNKNOWN, kind: 'inbox' });
    const notice = await screen.findByText('That link’s workspace isn’t one this app can reach right now.');
    fireEvent.click(screen.getByRole('button', { name: 'Dismiss' }));
    expect(screen.queryByText('That link’s workspace isn’t one this app can reach right now.')).toBeNull();
    expect(notice).toBeTruthy();
  });

  it('drops and logs anything invalid, without navigating', async () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
    const router = renderDesktop(`/w/${ALPHA}/home`);
    // Waits for the app (and the navigate listener) to settle, not just the route.
    await screen.findByRole('heading', { level: 1, name: 'Home' });
    await gateway.navigate({ workspace: ALPHA, kind: 'not-a-real-kind' });
    await gateway.navigate('pitcrew://w/x/inbox');
    await new Promise((done) => setTimeout(done, 20));
    expect(router.state.location.pathname).toBe(`/w/${ALPHA}/home`);
    expect(warn).toHaveBeenCalledTimes(2);
    for (const call of warn.mock.calls) {
      expect(String(call[0])).toContain('dropped an invalid gateway://navigate target');
    }
    warn.mockRestore();
  });
});
