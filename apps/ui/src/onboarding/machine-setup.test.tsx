// @vitest-environment happy-dom
//
// Machine setup in the real `OnboardingApi` (api-v1.md, "Machine setup"), against a stand-in hub:
// the check and its rows (only the contract's, cut to a line), a fix that only opens the tool's
// install page from this app's own table and asks again (nothing installed, never a URL from the
// hub), the accounts, and a sign-in whose terminal the console's view would show and which the
// panel stops when it closes; a machine that is checked as it is connected saying so; no install
// step in the first run (the connect wizard installs from a reviewed plan); and the first run
// through Machine check and Sign in.

import { act, cleanup, fireEvent, render, screen, within } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { RemoteGateway, Transport, TransportResponse } from '../data/index.ts';
import type { OnboardingApi } from './api.ts';
import { OnboardingApiProvider } from './api-context.tsx';
import { CHECKED_WHEN_CONNECTED, createHubOnboardingApi } from './hub-api.ts';
import { INSTALL_PAGES } from './install-pages.ts';
import { line, toAccounts, toCheckRows, toSignIn } from './machine-wire.ts';
import { SignInPanel } from './sign-in-panel.tsx';
import { SignInTerminalProvider } from './sign-in-terminal.tsx';
import { MachineCheckStep } from './steps/machine-check-step.tsx';
import { stepsFor } from './steps.ts';
import { renderWizard } from './test-support.tsx';
import { WizardProvider } from './wizard-context.tsx';

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

const LAPTOP = '01JB000000000000000MCH0001';
const TERMINAL = '01JB000000000000000SES0042';
const MACHINES = [
  { id: '01JB000000000000000MCH0002', name: 'a SLURM cluster', kind: 'ssh', liveness: 'live' },
  { id: LAPTOP, name: 'This laptop', kind: 'local', liveness: 'live' },
];

const json = (body: unknown, status = 200): TransportResponse => ({ status, body: JSON.stringify(body) });

/** A hub that answers machine setup's routes; `answers` overrides by `METHOD path`. */
function standInHub(answers: Record<string, () => TransportResponse> = {}, kind: Transport['kind'] = 'browser') {
  const sent: { method: string; path: string; body?: string | undefined }[] = [];
  const transport: Transport = {
    kind,
    label: 'a test hub',
    request: (method, path, body) => {
      sent.push({ method, path, body });
      const answer = answers[`${method} ${path}`];
      if (answer !== undefined) return Promise.resolve(answer());
      if (path === '/v1/machines') return Promise.resolve(json(MACHINES));
      return Promise.resolve(json({ code: 'not_found', message: `No route for ${method} ${path}.` }, 404));
    },
    openSocket: () => {
      throw new Error('no sockets in this test');
    },
  };
  return { transport, sent };
}

const CHECK_ROWS = [
  { id: 'cli_claude', status: 'ok', detail: '2.1.3 (Claude Code)', version: '2.1.3 (Claude Code)' },
  { id: 'cli_opencode', status: 'missing', detail: 'OpenCode (opencode) is not on PATH.', fix: 'install_page' },
  { id: 'disk', status: 'warn', detail: 'Low: 2.1 GB free where PitCrew keeps its state.' },
];

describe('machine setup on the wire', () => {
  it('reads only the contract’s rows, and cuts text to a line', () => {
    const rows = toCheckRows({
      rows: [
        ...CHECK_ROWS,
        { id: 'cli_gemini', status: 'ok', detail: 'unknown tool' },
        { id: 'constructor', status: 'ok', detail: 'not a row' },
        { id: 'git', status: 'ok', detail: 'x', fix: '__proto__' },
        { id: 'git', status: 'great', detail: 'unknown status' },
        { id: 'gh', status: 'missing', detail: 'x', fix: 'curl | sh' },
        { id: 'tmux', status: 'ok', detail: `tmux 3.4\n\u001b[31m${'x'.repeat(300)}` },
        { id: 'cli_claude', status: 'missing', detail: 'a second claude row' },
      ],
    });
    expect(rows.map((r) => r.id)).toEqual(['cli-claude', 'cli-opencode', 'disk', 'tmux']);
    expect(rows[1]).toEqual({
      id: 'cli-opencode',
      label: 'OpenCode CLI',
      status: 'missing',
      detail: 'OpenCode (opencode) is not on PATH.',
      fixable: true,
      fix: 'install-page',
    });
    expect(rows[2]?.fixable).toBe(false);
    const tmux = rows[3]?.detail ?? '';
    expect(tmux.startsWith('tmux 3.4 ')).toBe(true);
    expect([...tmux]).toHaveLength(200);
    expect([...tmux].some((c) => c.charCodeAt(0) < 0x20)).toBe(false);
    expect(() => toCheckRows({ rows: 'nope' })).toThrow();
  });

  it('drops hidden and direction-changing characters, as the hub’s own check does', () => {
    // U+202E would show "2.1.3" as something else; zero-width and tag characters hide text.
    expect(line('claude \u202Egnp.lave\u202C 2.1.3')).toBe('claude gnp.lave 2.1.3');
    expect(line('git\u200B version\uFEFF 2.43.0\u2066\u2069')).toBe('git version 2.43.0');
    expect(line('gh\u{E0041}\u{E0042} 2.45.0\u00AD')).toBe('gh 2.45.0');
    // A line separator is a space, not words run together.
    expect(line('one\u2028two\u2029three')).toBe('one two three');
    const [row] = toCheckRows({ rows: [{ id: 'cli_codex', status: 'ok', detail: 'codex-cli \u202E0.50.0' }] });
    expect(row?.detail).toBe('codex-cli 0.50.0');
    const [account] = toAccounts([{ engine: 'claude', installed: true, signed_in: true, account: 'sam\u200D@example.com' }]);
    expect(account?.account).toBe('sam@example.com');
  });

  it('reads accounts and sign-ins as the contract has them', () => {
    expect(
      toAccounts([
        { engine: 'claude', installed: true, signed_in: true, account: 'sam@example.com' },
        { engine: 'codex', installed: true },
        { engine: 'opencode', installed: 'yes' },
        { engine: 'gemini', installed: true },
      ]),
    ).toEqual([
      { engine: 'claude', installed: true, signedIn: true, account: 'sam@example.com' },
      { engine: 'codex', installed: true, signedIn: undefined },
    ]);
    const signIn = { engine: 'codex', terminal: TERMINAL, command: ['codex', 'login'], running: true, started: 1 };
    expect(toSignIn(signIn).terminal).toBe(TERMINAL);
    expect(() => toSignIn({ ...signIn, terminal: '../../etc' })).toThrow();
  });
});

describe('machine setup through the hub', () => {
  it('is there once the hub api has a transport, without an install step for the hub’s own machine', () => {
    const { transport } = standInHub();
    const api = createHubOnboardingApi({ transport });
    for (const call of ['checkMachine', 'fixMachineRow', 'agentAccounts', 'startSignIn', 'signInRunning', 'stopSignIn'] as const) {
      expect(api.unavailable.has(call)).toBe(false);
    }
    expect(api.needsHelper({ kind: 'local' })).toBe(false);
    expect(api.needsHelper({ kind: 'ssh', host: 'hpc-login' })).toBe(true);
    // The safety settings need only the transport too; the hooks need the data client.
    expect(stepsFor(api).map((s) => s.id)).toEqual(['welcome', 'machine-check', 'sign-in', 'import', 'safety', 'done']);
  });

  it('says a machine that is not the hub’s own is checked as it is connected, asking nothing', async () => {
    const hub = standInHub();
    const api = createHubOnboardingApi({ transport: hub.transport });
    for (const machine of [
      { kind: 'ssh', host: 'hpc-login' },
      { kind: 'wsl', distro: 'Ubuntu' },
    ] as const) {
      expect(await api.checkMachine(machine)).toEqual({ machine, rows: [], deferred: CHECKED_WHEN_CONNECTED });
    }
    expect(hub.sent).toEqual([]);
  });

  it('checks the hub’s own machine', async () => {
    const hub = standInHub({ [`GET /v1/machines/${LAPTOP}/check`]: () => json({ rows: CHECK_ROWS }) });
    const api = createHubOnboardingApi({ transport: hub.transport });
    const result = await api.checkMachine({ kind: 'local' });
    expect(result.rows.map((r) => [r.id, r.status, r.fix])).toEqual([
      ['cli-claude', 'ok', undefined],
      ['cli-opencode', 'missing', 'install-page'],
      ['disk', 'warn', undefined],
    ]);
  });

  it('fixes only by opening the tool’s install page, from the app’s own table, and asks again', async () => {
    const opened: string[] = [];
    const hub = standInHub({
      [`GET /v1/machines/${LAPTOP}/check?row=cli_opencode`]: () => json({ rows: [CHECK_ROWS[1]] }),
      [`GET /v1/machines/${LAPTOP}/check?row=disk`]: () => json({ rows: [CHECK_ROWS[2]] }),
      // A hostile hub naming a page of its own: it is not one the app knows.
      [`GET /v1/machines/${LAPTOP}/check?row=gh`]: () =>
        json({ rows: [{ id: 'gh', status: 'missing', detail: 'x', fix: 'install_page', url: 'https://example.com/evil' }] }),
    });
    const api = createHubOnboardingApi({ transport: hub.transport, openPage: (url) => opened.push(url) });
    const row = await api.fixMachineRow({ kind: 'local' }, 'cli-opencode');
    expect(row.status).toBe('missing');
    expect(opened).toEqual([INSTALL_PAGES['cli-opencode']]);
    await api.fixMachineRow({ kind: 'local' }, 'gh');
    expect(opened).toEqual([INSTALL_PAGES['cli-opencode'], INSTALL_PAGES.gh]);
    await expect(api.fixMachineRow({ kind: 'local' }, 'disk')).rejects.toThrow('cannot fix');
    // Only reads: nothing was installed or changed on the hub.
    expect(hub.sent.every((r) => r.method === 'GET')).toBe(true);
  });

  it('opens no window in the desktop app, where the step shows the page instead', async () => {
    const open = vi.spyOn(window, 'open').mockImplementation(() => null);
    const hub = standInHub(
      { [`GET /v1/machines/${LAPTOP}/check?row=cli_opencode`]: () => json({ rows: [CHECK_ROWS[1]] }) },
      'desktop',
    );
    await createHubOnboardingApi({ transport: hub.transport }).fixMachineRow({ kind: 'local' }, 'cli-opencode');
    expect(open).not.toHaveBeenCalled();
    const browser = standInHub({ [`GET /v1/machines/${LAPTOP}/check?row=cli_opencode`]: () => json({ rows: [CHECK_ROWS[1]] }) });
    await createHubOnboardingApi({ transport: browser.transport }).fixMachineRow({ kind: 'local' }, 'cli-opencode');
    expect(open).toHaveBeenCalledWith(INSTALL_PAGES['cli-opencode'], '_blank', 'noopener,noreferrer');
  });

  it('reads the accounts, starts a sign-in, and says when its login has ended', async () => {
    let running = true;
    const hub = standInHub({
      [`GET /v1/machines/${LAPTOP}/agents`]: () =>
        json([{ engine: 'claude', installed: true, signed_in: false }, { engine: 'opencode', installed: false, detail: 'not on PATH' }]),
      [`POST /v1/machines/${LAPTOP}/agents/codex/sign-in`]: () =>
        json({ engine: 'codex', terminal: TERMINAL, command: ['codex', 'login', '--device-auth'], running, started: 1 }, 201),
      [`GET /v1/machines/${LAPTOP}/agents/codex/sign-in`]: () =>
        json({ engine: 'codex', terminal: TERMINAL, command: ['codex', 'login'], running, started: 1 }),
    });
    const api = createHubOnboardingApi({ transport: hub.transport });
    expect(await api.agentAccounts()).toEqual([
      { engine: 'claude', installed: true, signedIn: false },
      { engine: 'opencode', installed: false, signedIn: undefined, detail: 'not on PATH' },
    ]);
    expect(await api.startSignIn('codex', { kind: 'local' }, 'device-code')).toEqual({
      terminalSessionId: TERMINAL,
      command: ['codex', 'login', '--device-auth'],
    });
    expect(hub.sent.find((r) => r.method === 'POST')?.body).toBe('{"method":"device_code"}');
    expect(await api.signInRunning('codex', { kind: 'local' })).toBe(true);
    running = false;
    expect(await api.signInRunning('codex', { kind: 'local' })).toBe(false);
    expect(await api.signInRunning('claude', { kind: 'local' })).toBe(false);
    await expect(api.startSignIn('claude', { kind: 'local' })).rejects.toThrow('No route');
  });

  it('stops a sign-in: gone, or none there, both resolve', async () => {
    let status = 204;
    const hub = standInHub({
      [`DELETE /v1/machines/${LAPTOP}/agents/codex/sign-in`]: () =>
        status === 204 ? { status, body: '' } : json({ code: 'forbidden', message: 'Only the person who set this hub up…' }, status),
    });
    const api = createHubOnboardingApi({ transport: hub.transport });
    await api.stopSignIn('codex', { kind: 'local' });
    expect(hub.sent.filter((r) => r.method === 'DELETE').map((r) => r.path)).toEqual([
      `/v1/machines/${LAPTOP}/agents/codex/sign-in`,
    ]);
    await api.stopSignIn('claude', { kind: 'local' });
    status = 403;
    await expect(api.stopSignIn('codex', { kind: 'local' })).rejects.toThrow('Only the person who set this hub up');
  });
});

/** An `OnboardingApi` for the sign-in panel alone: every sign-in it starts, and every stop. */
function panelApi() {
  const stopped: string[] = [];
  const started: string[] = [];
  const api = {
    agentAccounts: () =>
      Promise.resolve([
        { engine: 'claude' as const, installed: true, signedIn: false },
        { engine: 'codex' as const, installed: true, signedIn: false },
      ]),
    startSignIn: (engine: 'claude' | 'codex' | 'opencode') => {
      started.push(engine);
      return Promise.resolve({ terminalSessionId: `${TERMINAL.slice(0, -1)}${started.length}`, command: [engine, 'login'] });
    },
    signInRunning: () => Promise.resolve(true),
    stopSignIn: (engine: 'claude' | 'codex' | 'opencode') => {
      stopped.push(engine);
      return Promise.resolve();
    },
  } satisfies Pick<OnboardingApi, 'agentAccounts' | 'startSignIn' | 'signInRunning' | 'stopSignIn'>;
  return { api, stopped, started };
}

describe('the sign-in panel', () => {
  it('stops a login still running when it closes, and one another CLI’s replaces', async () => {
    const { api, stopped, started } = panelApi();
    const { unmount } = render(
      <SignInTerminalProvider render={({ terminal }) => <p>Terminal {terminal}</p>}>
        <SignInPanel api={api} target={{ kind: 'local' }} />
      </SignInTerminalProvider>,
    );
    fireEvent.click(await screen.findByRole('button', { name: 'Sign in to Claude Code' }));
    await screen.findByText(/Terminal .*1$/);
    expect(stopped).toEqual([]);
    // Codex's sign-in in its place: Claude Code's login, unfinished, stops.
    fireEvent.click(screen.getByRole('button', { name: 'Sign in to Codex' }));
    await screen.findByText(/Terminal .*2$/);
    expect(started).toEqual(['claude', 'codex']);
    expect(stopped).toEqual(['claude']);
    // The person goes on: Codex's login, still running, stops with the panel.
    act(() => unmount());
    expect(stopped).toEqual(['claude', 'codex']);
  });

  it('leaves a login that has ended alone', async () => {
    const { api, stopped } = panelApi();
    let running = true;
    const ended = { ...api, signInRunning: () => Promise.resolve(running) };
    const { unmount } = render(
      <SignInTerminalProvider render={({ terminal }) => <p>Terminal {terminal}</p>}>
        <SignInPanel api={ended} target={{ kind: 'local' }} />
      </SignInTerminalProvider>,
    );
    fireEvent.click(await screen.findByRole('button', { name: 'Sign in to Claude Code' }));
    await screen.findByText(/Terminal /);
    running = false;
    await screen.findByText(/the login has ended/, undefined, { timeout: 6000 });
    act(() => unmount());
    expect(stopped).toEqual([]);
  });
});

describe('the machine check step', () => {
  it('shows a machine checked as it is connected as a note, not an error', async () => {
    const api = {
      ...createHubOnboardingApi({ transport: standInHub().transport }),
      checkMachine: () => Promise.resolve({ machine: { kind: 'local' as const }, rows: [], deferred: CHECKED_WHEN_CONNECTED }),
    };
    render(
      <OnboardingApiProvider api={api}>
        <WizardProvider>
          <MachineCheckStep />
        </WizardProvider>
      </OnboardingApiProvider>,
    );
    expect(await screen.findByText(CHECKED_WHEN_CONNECTED)).toBeTruthy();
    expect(screen.queryByRole('alert')).toBeNull();
    expect(screen.queryByRole('button', { name: 'Check again' })).toBeNull();
    expect((screen.getByRole('button', { name: 'Continue' }) as HTMLButtonElement).disabled).toBe(false);
  });
});

/** A gateway whose remote commands this test answers. */
function gateway(overrides: Partial<RemoteGateway>): RemoteGateway {
  const no = () => Promise.reject(new Error('not in this test'));
  return {
    sshHosts: () => Promise.resolve([]),
    remoteProbe: no,
    remotePlan: no,
    remoteAdd: no,
    workspaceRemove: no,
    workspaceRetry: no,
    remoteCancel: no,
    onPrompt: no,
    onPromptClosed: no,
    replyPrompt: no,
    ...overrides,
  };
}

describe('installing the helper', () => {
  it('is no step of the first run: the connect wizard installs from a plan the person reviewed', async () => {
    const added: string[] = [];
    const remote = gateway({
      remoteAdd: (plan) => {
        added.push(plan);
        return Promise.reject(new Error('not in this test'));
      },
    });
    const api = createHubOnboardingApi({ transport: standInHub().transport, remote });
    expect(api.unavailable.has('launcherOptions')).toBe(true);
    expect(api.unavailable.has('streamInstallHelper')).toBe(true);
    // Not even for a machine that needs the helper.
    expect(stepsFor(api, { target: { kind: 'ssh', host: 'hpc-login' } }).map((s) => s.id)).not.toContain('install-helper');
    await expect(api.launcherOptions({ kind: 'ssh', host: 'hpc-login' })).rejects.toThrow('not available');
    expect(() =>
      api.streamInstallHelper({ machine: { kind: 'ssh', host: 'hpc-login' }, launcher: 'tmux', plan: 'plan-1' }, () => undefined),
    ).toThrow('not available');
    expect(added).toEqual([]);
  });
});

describe('the first run, through Machine check and Sign in', () => {
  it('checks, offers the install page, signs in with the CLI’s own login, and reads the account again', async () => {
    let signedIn = false;
    let running = true;
    const opened: string[] = [];
    const hub = standInHub({
      [`GET /v1/machines/${LAPTOP}/check`]: () => json({ rows: CHECK_ROWS }),
      [`GET /v1/machines/${LAPTOP}/check?row=cli_opencode`]: () => json({ rows: [CHECK_ROWS[1]] }),
      [`GET /v1/machines/${LAPTOP}/agents`]: () =>
        json([signedIn ? { engine: 'claude', installed: true, signed_in: true, account: 'sam@example.com' } : { engine: 'claude', installed: true, signed_in: false }]),
      [`POST /v1/machines/${LAPTOP}/agents/claude/sign-in`]: () =>
        json({ engine: 'claude', terminal: TERMINAL, command: ['claude', 'auth', 'login'], running: true, started: 1 }, 201),
      [`GET /v1/machines/${LAPTOP}/agents/claude/sign-in`]: () => {
        if (!running) signedIn = true;
        return json({ engine: 'claude', terminal: TERMINAL, command: ['claude', 'auth', 'login'], running, started: 1 });
      },
    });
    const api = createHubOnboardingApi({ transport: hub.transport, openPage: (url) => opened.push(url) });
    renderWizard(api, (wizard) => (
      <SignInTerminalProvider render={({ terminal }) => <p>Terminal {terminal}</p>}>{wizard}</SignInTerminalProvider>
    ));

    await screen.findByRole('heading', { level: 1, name: 'Welcome to PitCrew' });
    fireEvent.click(screen.getByRole('button', { name: 'Get started' }));
    await screen.findByRole('heading', { level: 1, name: 'Checking the machine' });
    const opencode = (await screen.findByText('OpenCode CLI')).closest('li');
    if (opencode === null) throw new Error('no row');
    fireEvent.click(within(opencode).getByRole('button', { name: 'Install OpenCode CLI…' }));
    await within(opencode).findByText(INSTALL_PAGES['cli-opencode'] ?? '');
    expect(opened).toEqual([INSTALL_PAGES['cli-opencode']]);
    expect(screen.queryByRole('button', { name: /Fix Disk space/ })).toBeNull();
    fireEvent.click(screen.getByRole('button', { name: 'Check again' }));
    await screen.findByText('2.1.3 (Claude Code)');
    fireEvent.click(screen.getByRole('button', { name: 'Continue' }));

    await screen.findByRole('heading', { level: 1, name: 'Sign in to your agents' });
    await screen.findByText('Not signed in');
    fireEvent.click(screen.getByRole('button', { name: 'Sign in to Claude Code' }));
    await screen.findByText(`Terminal ${TERMINAL}`);
    expect(screen.getByText('claude auth login')).toBeTruthy();
    running = false;
    await screen.findByText('sam@example.com', undefined, { timeout: 6000 });
    expect(screen.getByText('Signed in')).toBeTruthy();
    expect(screen.getByText(/the login has ended/)).toBeTruthy();
    // Nothing but the sign-in changed anything.
    expect(hub.sent.filter((r) => r.method !== 'GET').map((r) => r.path)).toEqual([
      `/v1/machines/${LAPTOP}/agents/claude/sign-in`,
    ]);
  });
});
