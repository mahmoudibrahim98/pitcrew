// @vitest-environment happy-dom
//
// Machine setup in the real `OnboardingApi` (api-v1.md, "Machine setup"), against a stand-in hub:
// the check and its rows (only the contract's, cut to a line), a fix that only opens the tool's
// install page from this app's own table and asks again (nothing installed, never a URL from the
// hub), the accounts, and a sign-in whose terminal the console's view would show; installing the
// helper on a remote machine through the gateway, only a plan the person reviewed; and the first
// run through Machine check and Sign in.

import { cleanup, fireEvent, screen, within } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { RemoteGateway, RemoteProgress, Transport, TransportResponse } from '../data/index.ts';
import type { InstallProgressEvent } from './api.ts';
import { createHubOnboardingApi } from './hub-api.ts';
import { INSTALL_PAGES } from './install-pages.ts';
import { installLine } from './install-log.ts';
import { toAccounts, toCheckRows, toSignIn } from './machine-wire.ts';
import { SignInTerminalProvider } from './sign-in-terminal.tsx';
import { stepsFor } from './steps.ts';
import { renderWizard } from './test-support.tsx';

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
    for (const call of ['checkMachine', 'fixMachineRow', 'agentAccounts', 'startSignIn', 'signInRunning'] as const) {
      expect(api.unavailable.has(call)).toBe(false);
    }
    expect(api.needsHelper({ kind: 'local' })).toBe(false);
    expect(api.needsHelper({ kind: 'ssh', host: 'hpc-login' })).toBe(true);
    expect(stepsFor(api).map((s) => s.id)).toEqual(['welcome', 'machine-check', 'sign-in', 'import', 'done']);
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
    await expect(api.checkMachine({ kind: 'ssh', host: 'hpc-login' })).rejects.toThrow('its own workspace');
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

describe('installing the helper on a remote machine', () => {
  it('offers the launchers the probe found', async () => {
    const api = createHubOnboardingApi({
      remote: gateway({
        remoteProbe: () =>
          Promise.resolve({ host: 'hpc-login', os: 'linux', arch: 'x86_64', tmux: { version: '3.0a' }, slurm: { version: 'slurm 23.02.7', srunOverlap: true } }),
      }),
    });
    const options = await api.launcherOptions({ kind: 'ssh', host: 'hpc-login' });
    expect(options).toEqual([
      { launcher: 'direct', recommended: false },
      { launcher: 'tmux', recommended: false, unavailable: 'tmux 3.0a is older than 3.2' },
      { launcher: 'systemd-user', recommended: false, unavailable: 'Not offered yet' },
      { launcher: 'slurm', recommended: true },
    ]);
    await expect(api.launcherOptions({ kind: 'local' })).rejects.toThrow('needs no helper');
  });

  it('installs nothing unseen: only a plan the person reviewed, its progress as lines', async () => {
    const added: string[] = [];
    let finish: (() => void) | undefined;
    const cancelled: string[] = [];
    const progress: RemoteProgress[] = [
      { step: 'Copy pitcrewd 0.1.0 to ~/.pitcrew', state: 'running', detail: 'uploading' },
      { step: 'Copy pitcrewd 0.1.0 to ~/.pitcrew', state: 'running', detail: '40% sent' },
      { step: 'Copy pitcrewd 0.1.0 to ~/.pitcrew', state: 'done' },
      { step: 'add', state: 'done' },
    ];
    const api = createHubOnboardingApi({
      remote: gateway({
        remoteAdd: (plan, onProgress) => {
          added.push(plan);
          for (const p of progress) onProgress(p);
          return new Promise((resolve) => {
            finish = () => resolve({ id: 'ws-remote', name: 'hpc-login', kind: 'remote', state: 'ready' });
          });
        },
        remoteCancel: (plan) => {
          cancelled.push(plan);
          return Promise.resolve();
        },
      }),
    });
    const events: InstallProgressEvent[] = [];
    api.streamInstallHelper({ machine: { kind: 'ssh', host: 'hpc-login' }, launcher: 'slurm' }, (e) => events.push(e));
    expect(events).toEqual([{ type: 'error', message: 'Review what will be installed first: nothing is installed unseen.' }]);
    expect(added).toEqual([]);

    events.length = 0;
    const streamed = api.streamInstallHelper(
      { machine: { kind: 'ssh', host: 'hpc-login' }, launcher: 'tmux', plan: 'plan-1' },
      (e) => events.push(e),
    );
    expect(added).toEqual(['plan-1']);
    finish?.();
    await vi.waitFor(() => expect(events.at(-1)).toEqual({ type: 'done' }));
    expect(events.slice(0, -1)).toEqual(progress.map((p) => ({ type: 'log', line: installLine(p) })));
    const [, sent, , whole] = progress;
    expect(sent === undefined ? '' : installLine(sent)).toBe('Copy pitcrewd 0.1.0 to ~/.pitcrew: 40% sent');
    expect(whole === undefined ? '' : installLine(whole)).toBe('Connected.');
    streamed.cancel();
    expect(cancelled).toEqual(['plan-1']);
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
