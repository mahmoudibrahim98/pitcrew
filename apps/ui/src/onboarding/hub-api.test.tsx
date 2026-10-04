// @vitest-environment happy-dom
//
// The real `OnboardingApi` (`createHubOnboardingApi`): setup is `POST /v1/setup` (here a stand-in
// for the data layer's `setUp`), the host list is the gateway's `sshHosts`, the scan is
// `POST /v1/machines/{id}/scan` and creating from it `POST /v1/projects` and `/v1/workstreams`
// (here a stand-in for the data layer's client), and every other call is unavailable. Without the
// client the first run is Welcome, Workspace, Done; with it, Scan and Create come between. The
// hub's refusals land by the right field, and a workspace set up meanwhile goes Home.

import { cleanup, fireEvent, screen, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import {
  ApiError,
  SetupConflict,
  type Machine,
  type NewProject,
  type NewWorkstream,
  type Project,
  type RemoteGateway,
  type Setup,
  type SetupResult,
  type TransportResponse,
  type Workstream,
} from '../data/index.ts';
import { SetupRefused, type OnboardingApi, type ProjectSelection, type ScanProgressEvent } from './api.ts';
import { createHubOnboardingApi, type HubData } from './hub-api.ts';
import { SCAN_REPORT } from './scan-fixture.ts';
import { toScanResult } from './scan-wire.ts';
import { stepsFor } from './steps.ts';
import { renderWizard, TEST_WS } from './test-support.tsx';

afterEach(() => cleanup());

const RESULT: SetupResult = {
  workspace: { id: '01JB000000000000000WSPFRSH', name: 'Demo Lab' },
  me: { id: '01JB000000000000000MEM0001', kind: 'human', handle: '@sam', name: 'Sam Rivera' },
  machine: { id: '01JB000000000000000MAC0001', name: 'This laptop', kind: 'local', liveness: 'live' },
};

const INPUT = { workspaceName: 'Demo Lab', person: { name: 'Sam Rivera', handle: '@sam' }, machineName: 'This laptop' };

function remoteWith(hosts: string[]): RemoteGateway {
  const no = () => Promise.reject(new Error('not in this test'));
  return {
    sshHosts: () => Promise.resolve(hosts),
    remoteProbe: no,
    remotePlan: no,
    remoteAdd: no,
    workspaceRemove: no,
    workspaceRetry: no,
    remoteCancel: no,
    onPrompt: no,
    onPromptClosed: no,
    replyPrompt: no,
  };
}

describe('createHubOnboardingApi', () => {
  it('serves setup and the host list, and marks everything else unavailable', async () => {
    const setUp = vi.fn<(setup: Setup) => Promise<SetupResult>>(() => Promise.resolve(RESULT));
    const api = createHubOnboardingApi({ setUp, remote: remoteWith(['hpc-login', 'build-box']) });
    expect([...api.unavailable]).not.toContain('setupWorkspace');
    expect([...api.unavailable]).not.toContain('discoverHosts');
    expect(stepsFor(api).map((s) => s.id)).toEqual(['welcome', 'workspace', 'done']);

    expect(await api.discoverHosts()).toEqual([
      { kind: 'ssh', id: 'hpc-login' },
      { kind: 'ssh', id: 'build-box' },
    ]);
    expect(await api.setupWorkspace(INPUT)).toEqual({
      workspace: { id: RESULT.workspace.id, name: 'Demo Lab' },
      me: { name: 'Sam Rivera', handle: '@sam' },
    });
    expect(setUp).toHaveBeenCalledWith({
      workspace_name: 'Demo Lab',
      person: { name: 'Sam Rivera', handle: '@sam' },
      machine_name: 'This laptop',
    });

    await expect(api.checkMachine({ kind: 'local' })).rejects.toThrow('not available yet');
    await expect(api.saveSafety({ permissionMode: 'default', backOfficeEnabled: false, backOfficeCaps: { maxAutoAcceptPerHour: 1 } })).rejects.toThrow();
  });

  it('has no setup outside a workspace, and no hosts in a browser', async () => {
    const api = createHubOnboardingApi({ remote: null });
    expect(api.unavailable.has('setupWorkspace')).toBe(true);
    expect(api.unavailable.has('discoverHosts')).toBe(true);
    await expect(api.discoverHosts()).rejects.toThrow('not available yet');
    await expect(api.setupWorkspace(INPUT)).rejects.toThrow('not available yet');
  });

  it.each([
    [new SetupConflict(new ApiError('conflict', 'This workspace is already set up.', 409), true), { alreadySetUp: true, field: undefined }],
    [new SetupConflict(new ApiError('conflict', 'The handle @sam is already taken.', 409), false), { alreadySetUp: false, field: 'handle' }],
    [new ApiError('invalid', 'machine_name must be 1 to 60 characters.', 400), { alreadySetUp: false, field: 'machineName' }],
    [new ApiError('invalid', 'The body is not JSON.', 400), { alreadySetUp: false, field: undefined }],
  ])('turns %s into a SetupRefused', async (error, expected) => {
    const api = createHubOnboardingApi({ setUp: () => Promise.reject(error) });
    const refused = await api.setupWorkspace(INPUT).catch((e: unknown) => e);
    expect(refused).toBeInstanceOf(SetupRefused);
    expect(refused).toMatchObject({ ...expected, message: error.message });
  });

  it('passes any other failure on as it is', async () => {
    const offline = new ApiError('unavailable', 'Cannot reach the hub', 0);
    const api = createHubOnboardingApi({ setUp: () => Promise.reject(offline) });
    expect(await api.setupWorkspace(INPUT).catch((e: unknown) => e)).toBe(offline);
  });
});

describe('the real first run', () => {
  function walkToWorkspace() {
    return screen.findByRole('heading', { level: 1, name: 'Welcome to PitCrew' }).then(async () => {
      fireEvent.click(screen.getByRole('button', { name: 'Get started' }));
      await screen.findByRole('heading', { level: 1, name: 'Your first workspace' });
      fireEvent.change(screen.getByLabelText('Workspace name'), { target: { value: '  Demo Lab ' } });
      fireEvent.change(screen.getByLabelText('Your name'), { target: { value: 'Sam Rivera' } });
      fireEvent.change(screen.getByLabelText('This machine’s name'), { target: { value: 'This laptop' } });
    });
  }

  it('is Welcome, Workspace, Done, sending the trimmed names once', async () => {
    const setUp = vi.fn<(setup: Setup) => Promise<SetupResult>>(() => Promise.resolve(RESULT));
    const { router } = renderWizard(createHubOnboardingApi({ setUp }));
    await walkToWorkspace();
    expect(screen.getAllByRole('tab').map((t) => t.textContent?.replace(/^\d/, ''))).toEqual(['Welcome', 'Workspace', 'Done']);
    fireEvent.click(screen.getByRole('button', { name: 'Continue' }));
    await screen.findByRole('heading', { level: 1, name: "You're set up" });
    expect(setUp).toHaveBeenCalledTimes(1);
    expect(setUp.mock.calls[0]?.[0]).toEqual({
      workspace_name: 'Demo Lab',
      person: { name: 'Sam Rivera', handle: '@sam' },
      machine_name: 'This laptop',
    });
    fireEvent.click(screen.getByRole('button', { name: 'Go to Home' }));
    await screen.findByRole('heading', { level: 1, name: 'Home' });
    expect(router.state.location.pathname).toBe(`/w/${TEST_WS}/home`);
  });

  it("shows the hub's 400 by the field it names", async () => {
    const setUp = vi.fn(() => Promise.reject(new ApiError('invalid', 'machine_name must not contain control characters.', 400)));
    renderWizard(createHubOnboardingApi({ setUp }));
    await walkToWorkspace();
    fireEvent.click(screen.getByRole('button', { name: 'Continue' }));
    const alert = await screen.findByRole('alert');
    expect(alert.textContent).toBe('machine_name must not contain control characters.');
    const machine = screen.getByLabelText('This machine’s name');
    expect(machine.getAttribute('aria-invalid')).toBe('true');
    expect(machine.getAttribute('aria-describedby')).toContain(alert.id);
    expect(document.activeElement).toBe(machine);
  });

  it('shows a taken handle by the handle', async () => {
    const taken = new SetupConflict(new ApiError('conflict', 'The handle @sam is already taken.', 409), false);
    renderWizard(createHubOnboardingApi({ setUp: () => Promise.reject(taken) }));
    await walkToWorkspace();
    fireEvent.click(screen.getByRole('button', { name: 'Continue' }));
    const alert = await screen.findByRole('alert');
    expect(alert.textContent).toBe('The handle @sam is already taken.');
    expect(screen.getByLabelText('Your handle').getAttribute('aria-invalid')).toBe('true');
    expect(screen.getByRole('heading', { level: 1 }).textContent).toBe('Your first workspace');
  });

  it('goes Home when the workspace was set up meanwhile', async () => {
    const done = new SetupConflict(new ApiError('conflict', 'This workspace is already set up.', 409), true);
    const { router } = renderWizard(createHubOnboardingApi({ setUp: () => Promise.reject(done) }));
    await walkToWorkspace();
    fireEvent.click(screen.getByRole('button', { name: 'Continue' }));
    await screen.findByRole('heading', { level: 1, name: 'Home' });
    expect(router.state.location.pathname).toBe(`/w/${TEST_WS}/home`);
  });

  it('checks the form before sending anything', async () => {
    const setUp = vi.fn<(setup: Setup) => Promise<SetupResult>>(() => Promise.resolve(RESULT));
    renderWizard(createHubOnboardingApi({ setUp }));
    await walkToWorkspace();
    fireEvent.change(screen.getByLabelText('Your handle'), { target: { value: '@Sam' } });
    fireEvent.click(screen.getByRole('button', { name: 'Continue' }));
    expect((await screen.findByRole('alert')).textContent).toBe('A handle is "@" and 1 to 32 lower-case letters, digits, "_" or "-".');
    // The person typed a handle: the name no longer changes it.
    fireEvent.change(screen.getByLabelText('Your name'), { target: { value: 'Alex Kim' } });
    expect((screen.getByLabelText('Your handle') as HTMLInputElement).value).toBe('@Sam');
    expect(setUp).not.toHaveBeenCalled();
  });
});

// ─── The scan and creating from it ─────────────────────────────────────────────────────────────

const LAPTOP = '01JB000000000000000MCH0001';
const SAM = '01JB000000000000000MEM0001';
const MACHINES: Machine[] = [
  { id: '01JB000000000000000MCH0002', name: 'a SLURM cluster', kind: 'ssh', liveness: 'live' },
  { id: LAPTOP, name: 'This laptop', kind: 'local', liveness: 'live' },
];

function ndjson(...frames: unknown[]): string {
  return frames.map((frame) => `${JSON.stringify(frame)}\n`).join('');
}

const SCANNED: TransportResponse = {
  status: 200,
  contentType: 'application/x-ndjson',
  body: ndjson({ type: 'progress', scanned: 0 }, { type: 'progress', scanned: 6, total: 6 }, { type: 'done', report: SCAN_REPORT }),
};

interface FakeHubOptions {
  /** The scan's answer. */
  scan?: TransportResponse;
  /** Keys of the workspace's projects. */
  keys?: string[];
  /** Keys the hub answers 409 for, as if another client took them meanwhile. */
  takenMeanwhile?: string[];
  /** The workstream create (counting from 0) that fails once, as if the hub were out of reach. */
  failWorkstream?: number;
}

/** A stand-in for the data layer's client, recording what it is sent. */
function fakeHub(options: FakeHubOptions = {}) {
  const sent: string[] = [];
  const projects: NewProject[] = [];
  const workstreams: NewWorkstream[] = [];
  let failWorkstream = options.failWorkstream;
  const existing: Project[] = (options.keys ?? []).map((key, i) => ({
    id: `prj-existing-${i}`,
    key,
    name: key,
    status: 'in_progress',
    lead: SAM,
    members: [SAM],
    external: [],
  }));
  const data: HubData = {
    transport: {
      kind: 'browser',
      label: 'a test hub',
      request: (method, path) => {
        sent.push(`${method} ${path}`);
        if (path.endsWith('/hooks/diff')) return Promise.resolve({ status: 200, body: JSON.stringify({revision: 'test-preview', files: [], engines: []}) });
        if (path === '/v1/safety') return Promise.resolve({ status: 200, body: JSON.stringify({permissionMode: 'default', backOfficeEnabled: false, backOfficeCaps: {maxAutoAcceptPerHour: 20}}) });
        if (path === '/v1/import/dry-run') return Promise.resolve({ status: 200, body: '{"count":6}' });
        if (path === '/v1/import') return Promise.resolve({ status: 200, body: '{"imported":6}' });
        return Promise.resolve(options.scan ?? SCANNED);
      },
      openSocket: () => {
        throw new Error('no sockets in this test');
      },
    },
    machines: () => Promise.resolve(MACHINES),
    projects: () => Promise.resolve(existing),
    createProject: (project) => {
      if (options.takenMeanwhile?.includes(project.key) === true) {
        return Promise.reject(new ApiError('conflict', `The key ${project.key} is already used.`, 409));
      }
      projects.push(project);
      const created: Project = {
        id: `prj-${projects.length}`,
        key: project.key,
        name: project.name,
        status: 'in_progress',
        lead: SAM,
        members: [SAM],
        ...(project.root === undefined ? {} : { root: project.root }),
        external: [],
      };
      return Promise.resolve(created);
    },
    createWorkstream: (workstream) => {
      if (failWorkstream === workstreams.length) {
        failWorkstream = undefined;
        return Promise.reject(new ApiError('unavailable', 'Cannot reach the hub', 0));
      }
      workstreams.push(workstream);
      const created: Workstream = {
        id: `wst-${workstreams.length}`,
        project: workstream.project,
        name: workstream.name,
        status: 'active',
        health: 'on_track',
        locations: workstream.locations ?? [],
        external: [],
      };
      return Promise.resolve(created);
    },
  };
  return { data, sent, projects, workstreams };
}

/** Every event of one scan, up to its last. */
function scanned(api: OnboardingApi, machine: Parameters<OnboardingApi['streamScan']>[0]['machine'] = { kind: 'local' }) {
  return new Promise<ScanProgressEvent[]>((resolve) => {
    const events: ScanProgressEvent[] = [];
    api.streamScan({ machine }, (event) => {
      events.push(event);
      if (event.type !== 'progress') resolve(events);
    });
  });
}

const PAPER = '/home/sam/work/paper';
const TOOLS = '/home/sam/work/tools';
const DRAFTS = '/home/sam/work/paper/drafts';
const REVISION = '/home/sam/work/paper#revision-2';

describe('the scan, from the hub', () => {
  it('is served once the hub api has the workspace’s client: Scan and Create join the first run', () => {
    const api = createHubOnboardingApi({ setUp: () => Promise.reject(new Error('unused')), data: fakeHub().data });
    expect(api.unavailable.has('streamScan')).toBe(false);
    expect(api.unavailable.has('createFromScan')).toBe(false);
    expect(stepsFor(api).map((s) => s.id)).toEqual(['welcome', 'workspace', 'scan', 'create', 'import', 'hooks', 'safety', 'done']);
    for (const call of ['checkMachine'] as const) {
      expect(api.unavailable.has(call)).toBe(true);
    }
  });

  it('scans the hub’s own machine, passing on its progress and then the mapped result', async () => {
    const hub = fakeHub();
    const events = await scanned(createHubOnboardingApi({ data: hub.data }));
    expect(hub.sent).toEqual([`POST /v1/machines/${LAPTOP}/scan`]);
    expect(events).toEqual([
      { type: 'progress', scanned: 0 },
      { type: 'progress', scanned: 6, total: 6 },
      { type: 'done', result: toScanResult(SCAN_REPORT) },
    ]);
  });

  it('says why the hub refused a scan', async () => {
    const message = 'A scan of This laptop is already running; wait for it to finish.';
    const hub = fakeHub({ scan: { status: 409, body: JSON.stringify({ code: 'conflict', message }) } });
    expect(await scanned(createHubOnboardingApi({ data: hub.data }))).toEqual([{ type: 'error', message }]);
    const bare = fakeHub({ scan: { status: 502, body: '<html>Bad gateway</html>' } });
    expect(await scanned(createHubOnboardingApi({ data: bare.data }))).toEqual([
      { type: 'error', message: 'The hub answered 502.' },
    ]);
  });

  it('ends with an error for an answer that is not the contract’s, or has no report', async () => {
    const nonsense = fakeHub({ scan: { status: 200, body: 'nonsense\n' } });
    const [malformed] = await scanned(createHubOnboardingApi({ data: nonsense.data }));
    expect(malformed).toMatchObject({ type: 'error', message: expect.stringContaining('malformed') as unknown });
    const cut = fakeHub({ scan: { status: 200, body: ndjson({ type: 'progress', scanned: 0 }) } });
    expect((await scanned(createHubOnboardingApi({ data: cut.data }))).at(-1)).toEqual({
      type: 'error',
      message: 'The scan ended without a result.',
    });
    const failed = fakeHub({
      scan: { status: 200, body: ndjson({ type: 'error', code: 'internal', message: 'The scan failed.' }) },
    });
    expect(await scanned(createHubOnboardingApi({ data: failed.data }))).toEqual([
      { type: 'error', message: 'The scan failed.' },
    ]);
  });

  it('scans only this machine for now', async () => {
    const hub = fakeHub();
    const [event] = await scanned(createHubOnboardingApi({ data: hub.data }), { kind: 'ssh', host: 'hpc-login' });
    expect(event?.type).toBe('error');
    expect(hub.sent).toEqual([]);
  });

  it('sends nothing when cancelled before its request went out, as StrictMode’s first mount is', async () => {
    const hub = fakeHub();
    const onEvent = vi.fn();
    const streamed = createHubOnboardingApi({ data: hub.data }).streamScan({ machine: { kind: 'local' } }, onEvent);
    streamed.cancel();
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(hub.sent).toEqual([]);
    expect(onEvent).not.toHaveBeenCalled();
  });
});

describe('creating from the scan', () => {
  const both: ProjectSelection[] = [
    {
      suggestionId: PAPER,
      name: ' Paper ',
      template: 'research',
      workstreams: [
        { suggestionId: DRAFTS, name: 'Drafts' },
        { suggestionId: REVISION, name: '' },
      ],
    },
    { suggestionId: TOOLS, name: 'tools', template: 'software', workstreams: [] },
  ];

  it('needs a scan first', async () => {
    await expect(createHubOnboardingApi({ data: fakeHub().data }).createFromScan(both)).rejects.toThrow('Scan this machine first.');
  });

  it('creates the chosen projects at their roots and the workstreams where the scan found them', async () => {
    const hub = fakeHub({ keys: ['PAP'] });
    const api = createHubOnboardingApi({ data: hub.data });
    await scanned(api);
    const result = await api.createFromScan(both);
    // Keys from the names, unique in the workspace: `PAP` is taken, so the paper is `PAP2`.
    expect(hub.projects).toEqual([
      { key: 'PAP2', name: 'Paper', root: { machine: LAPTOP, path: PAPER } },
      { key: 'TOO', name: 'tools', root: { machine: LAPTOP, path: TOOLS } },
    ]);
    // A folder is its own place; a branch is the project's root on that branch. A cleared name is
    // the suggestion's.
    expect(hub.workstreams).toEqual([
      { project: 'prj-1', name: 'Drafts', locations: [{ machine: LAPTOP, path: DRAFTS }] },
      { project: 'prj-1', name: 'revision-2', locations: [{ machine: LAPTOP, path: PAPER, branch: 'revision-2' }] },
    ]);
    expect(result.projects.map((p) => p.key)).toEqual(['PAP2', 'TOO']);
    expect(result.workstreams.map((w) => w.project)).toEqual(['prj-1', 'prj-1']);
  });

  it('keeps a workstream’s place when it is moved to another project', async () => {
    const hub = fakeHub();
    const api = createHubOnboardingApi({ data: hub.data });
    await scanned(api);
    await api.createFromScan([
      { suggestionId: TOOLS, name: 'Tools', template: 'blank', workstreams: [{ suggestionId: DRAFTS, name: 'Drafts' }] },
    ]);
    expect(hub.workstreams).toEqual([
      { project: 'prj-1', name: 'Drafts', locations: [{ machine: LAPTOP, path: DRAFTS }] },
    ]);
  });

  it('tries the next key when the hub says one was taken meanwhile', async () => {
    const hub = fakeHub({ takenMeanwhile: ['PAP', 'PAP2'] });
    const api = createHubOnboardingApi({ data: hub.data });
    await scanned(api);
    await api.createFromScan([{ suggestionId: PAPER, name: 'paper', template: 'research', workstreams: [] }]);
    expect(hub.projects.map((p) => p.key)).toEqual(['PAP3']);
  });

  it('creates only what is missing when tried again after a failure part-way', async () => {
    const hub = fakeHub({ failWorkstream: 1 });
    const api = createHubOnboardingApi({ data: hub.data });
    await scanned(api);
    await expect(api.createFromScan(both)).rejects.toThrow('Cannot reach the hub');
    expect(hub.projects).toHaveLength(1);
    const result = await api.createFromScan(both);
    expect(hub.projects.map((p) => p.key)).toEqual(['PAP', 'TOO']);
    expect(hub.workstreams.map((w) => w.name)).toEqual(['Drafts', 'revision-2']);
    expect(result.projects.map((p) => p.id)).toEqual(['prj-1', 'prj-2']);
    expect(result.workstreams.map((w) => w.id)).toEqual(['wst-1', 'wst-2']);
  });

  it('refuses a suggestion the last scan did not have', async () => {
    const api = createHubOnboardingApi({ data: fakeHub().data });
    await scanned(api);
    await expect(
      api.createFromScan([{ suggestionId: '/home/sam/elsewhere', name: 'Elsewhere', template: 'blank', workstreams: [] }]),
    ).rejects.toThrow('not in the last scan');
  });
});

describe('the real first run, with the scan', () => {
  it('is Welcome, Workspace, Scan, Create, Done, creating what stayed ticked', async () => {
    const setUp = vi.fn<(setup: Setup) => Promise<SetupResult>>(() => Promise.resolve(RESULT));
    const hub = fakeHub();
    renderWizard(createHubOnboardingApi({ setUp, data: hub.data }));
    await screen.findByRole('heading', { level: 1, name: 'Welcome to PitCrew' });
    expect(screen.getAllByRole('tab').map((t) => t.textContent?.replace(/^\d/, ''))).toEqual([
      'Welcome',
      'Workspace',
      'Scan',
      'Create',
      'Import',
      'Hooks',
      'Safety',
      'Done',
    ]);
    fireEvent.click(screen.getByRole('button', { name: 'Get started' }));
    await screen.findByRole('heading', { level: 1, name: 'Your first workspace' });
    fireEvent.change(screen.getByLabelText('Workspace name'), { target: { value: 'Demo Lab' } });
    fireEvent.change(screen.getByLabelText('Your name'), { target: { value: 'Sam Rivera' } });
    fireEvent.click(screen.getByRole('button', { name: 'Continue' }));

    await screen.findByRole('heading', { level: 1, name: 'Scanning for sessions' });
    await screen.findByText(/Found 2 likely projects/);
    expect(screen.getByText(DRAFTS)).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: 'Continue' }));

    await screen.findByRole('heading', { level: 1, name: 'Create projects and workstreams' });
    fireEvent.click(screen.getByRole('checkbox', { name: 'Include revision-2' }));
    fireEvent.click(screen.getByRole('button', { name: 'Create' }));

    await screen.findByText('This will import 6 sessions.');
    fireEvent.click(screen.getByRole('button', { name: 'Continue' }));
    await screen.findByRole('button', { name: 'Install hooks' });
    fireEvent.click(screen.getByRole('button', { name: 'Skip hooks' }));
    await screen.findByLabelText('Let the back office accept low-risk actions automatically');
    await waitFor(() => expect((screen.getByRole('button', { name: 'Continue' }) as HTMLButtonElement).disabled).toBe(false));
    fireEvent.click(screen.getByRole('button', { name: 'Continue' }));
    await screen.findByRole('heading', { level: 1, name: "You're set up" });
    expect(screen.getByText('Created 2 projects.')).toBeTruthy();
    expect(hub.projects.map((p) => [p.key, p.name])).toEqual([
      ['PAP', 'paper'],
      ['TOO', 'tools'],
    ]);
    expect(hub.workstreams.map((w) => w.name)).toEqual(['drafts']);
    expect(hub.sent).toEqual([`POST /v1/machines/${LAPTOP}/scan`, "POST /v1/import/dry-run", "PUT /v1/import", `POST /v1/machines/${LAPTOP}/hooks/diff`, "GET /v1/safety", "PUT /v1/safety"]);
  });

  it('says why a scan failed, and tries again on request', async () => {
    const message = 'A scan of This laptop is already running; wait for it to finish.';
    let answers = [
      { status: 409, body: JSON.stringify({ code: 'conflict', message }) },
      SCANNED,
    ];
    const hub = fakeHub();
    const data: HubData = {
      ...hub.data,
      transport: {
        ...hub.data.transport,
        request: (method, path, body, signal) => {
          const [next, ...rest] = answers;
          answers = rest;
          void hub.data.transport.request(method, path, body, signal);
          return Promise.resolve(next ?? SCANNED);
        },
      },
    };
    renderWizard(createHubOnboardingApi({ setUp: () => Promise.resolve(RESULT), data }));
    await screen.findByRole('heading', { level: 1, name: 'Welcome to PitCrew' });
    fireEvent.click(screen.getByRole('button', { name: 'Get started' }));
    await screen.findByRole('heading', { level: 1, name: 'Your first workspace' });
    fireEvent.change(screen.getByLabelText('Workspace name'), { target: { value: 'Demo Lab' } });
    fireEvent.change(screen.getByLabelText('Your name'), { target: { value: 'Sam Rivera' } });
    fireEvent.click(screen.getByRole('button', { name: 'Continue' }));

    expect((await screen.findByRole('alert')).textContent).toBe(message);
    expect((screen.getByRole('button', { name: 'Continue' }) as HTMLButtonElement).disabled).toBe(true);
    fireEvent.click(screen.getByRole('button', { name: 'Try again' }));
    await screen.findByText(/Found 2 likely projects/);
    expect(hub.sent).toHaveLength(2);
  });
});
