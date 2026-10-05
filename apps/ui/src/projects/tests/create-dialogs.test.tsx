// @vitest-environment happy-dom
// @vitest-environment-options {"url": "http://localhost:5173/"}
import { cleanup, fireEvent, render, screen, within } from '@testing-library/react';
import type { QueryClient } from '@tanstack/react-query';
import { createMemoryHistory, RouterProvider } from '@tanstack/react-router';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { createApi, createQueryClient, DataProvider } from '../../data/index.ts';
import { createAppRouter } from '../../shell/routes.tsx';
import { useShell } from '../../shell/store.ts';
import { feature } from '../index.ts';
import { absoluteRoot, suggestedKey } from '../new-entities.tsx';
import { demo, eventually, startHub, stopHub, type Hub } from './harness.tsx';

const WS = '01JB000000000000000WSP0001';
let hub: Hub;
let client: QueryClient;
beforeEach(async () => { hub = await startHub(); });
afterEach(async () => {
  cleanup(); useShell.getState().setCreating(null);
  await client?.cancelQueries(); client?.clear();
  await stopHub(hub); localStorage.clear();
});
function app(path = 'members') {
  const api = createApi({ baseUrl: hub.url, token: 'dev-device-token' });
  client = createQueryClient();
  const router = createAppRouter([feature], { history: createMemoryHistory({ initialEntries: [`/w/${WS}/${path}`] }) });
  render(<DataProvider api={api} queryClient={client} token="dev-device-token"><RouterProvider router={router} /></DataProvider>);
  return api;
}
async function open(id: string, name: string) {
  await screen.findByRole('main');
  useShell.getState().setCreating(id);
  const dialog = within(await screen.findByRole('dialog', { name }));
  await dialog.findByLabelText('Name');
  return dialog;
}
function fill(dialog: ReturnType<typeof within>, label: string, value: string) {
  fireEvent.change(dialog.getByLabelText(label), { target: { value } });
}

describe('create dialogs against the mock hub', () => {
  it('defaults tasks to the current project and lists only its workstreams, including the fallback project', async () => {
    const api = app(`projects/${demo.tooling}`);
    await screen.findByRole('main');
    useShell.getState().setCreating('task');
    const task = within(await screen.findByRole('dialog', { name: 'New task' }));
    const projects = await api.projects();
    const streams = await api.workstreams();
    await eventually(() => expect((task.getByLabelText('Project') as HTMLSelectElement).value).toBe(demo.tooling));
    const options = () => [...(task.getByLabelText('Workstream (optional)') as HTMLSelectElement).options].map((o) => o.value).filter(Boolean);
    await eventually(() => expect(options()).toEqual(streams.filter((w) => w.project === demo.tooling).map((w) => w.id)));
    fireEvent.change(task.getByLabelText('Project'), { target: { value: demo.paper } });
    await eventually(() => expect(options()).toEqual(streams.filter((w) => w.project === demo.paper).map((w) => w.id)));
    useShell.getState().setCreating(null);
    await eventually(() => expect(screen.queryByRole('dialog')).toBeNull());
    // An explicit empty selection exercises the same resolved project id as a fresh dialog.
    useShell.getState().setCreating('task');
    const reopened = within(await screen.findByRole('dialog', { name: 'New task' }));
    fireEvent.change(reopened.getByLabelText('Project'), { target: { value: '' } });
    const first = projects[0]?.id;
    await eventually(() => expect([...(reopened.getByLabelText('Workstream (optional)') as HTMLSelectElement).options].map((o) => o.value).filter(Boolean)).toEqual(streams.filter((w) => w.project === first).map((w) => w.id)));
  });
  it('creates an agent with every persona field and immediately lists it and its team', async () => {
    const api = app();
    const agent = await open('agent', 'New agent');
    expect(agent.getByLabelText('Name')).toBe(document.activeElement);
    expect((agent.getByRole('option', { name: 'Bypass permissions' }) as HTMLOptionElement).disabled).toBe(true);
    fill(agent, 'Name', 'Synthetic writer'); fill(agent, 'Engine', 'codex');
    fill(agent, 'Model (optional)', 'demo-model'); fill(agent, 'Instructions (optional)', 'Write synthetic examples.');
    fill(agent, 'Permission mode', 'plan');
    fireEvent.click(agent.getByRole('button', { name: 'Create' }));
    await eventually(() => expect(screen.queryByRole('dialog')).toBeNull());
    expect(await within(screen.getByRole('region', { name: 'Agent recipes' })).findByText('Synthetic writer · codex · demo-model')).toBeDefined();
    const recipe = (await api.personas()).find((p) => p.name === 'Synthetic writer');
    expect(recipe).toMatchObject({ engine: 'codex', model: 'demo-model', instructions: 'Write synthetic examples.', permission_mode: 'plan' });
    const team = await open('team', 'New team'); fill(team, 'Name', 'Synthetic crew');
    fireEvent.click(await team.findByRole('checkbox', { name: 'Synthetic writer (agent)' }));
    fireEvent.click(team.getByRole('button', { name: 'Create' }));
    await eventually(() => expect(screen.queryByRole('dialog')).toBeNull());
    expect(await within(screen.getByRole('region', { name: 'Teams' })).findByText(/Synthetic crew/)).toBeDefined();
    const saved = (await api.teams()).find((t) => t.name === 'Synthetic crew');
    const member = (await api.members()).find((m) => m.persona === recipe?.id);
    expect(saved?.members).toContain(member?.id);
    expect(saved?.members).toContain(demo.sam);
  });
  it('shows invalid roots and key conflicts inline, then creates a project and first workstream together', async () => {
    const api = app();
    const dialog = await open('project', 'New project');
    await dialog.findByRole('option', { name: 'This laptop' });
    fill(dialog, 'Name', 'Synthetic project'); fill(dialog, 'Key', 'PAP'); fill(dialog, 'Root path', 'relative');
    fill(dialog, 'First workstream (optional)', 'Synthetic stream');
    fireEvent.click(dialog.getByRole('button', { name: 'Create' }));
    expect(await dialog.findByRole('alert')).toHaveProperty('textContent', 'Enter an absolute path for this machine.');
    expect((await api.projects()).some((p) => p.name === 'Synthetic project')).toBe(false);
    fill(dialog, 'Root path', process.platform === 'win32' ? 'C:/synthetic/project' : '/home/sam/synthetic');
    fireEvent.click(dialog.getByRole('button', { name: 'Create' }));
    expect((await dialog.findByRole('alert')).textContent).toContain('PAP');
    fill(dialog, 'Key', 'SYN');
    fireEvent.click(dialog.getByRole('button', { name: 'Create' }));
    await screen.findByRole('heading', { name: 'Synthetic project', level: 1 });
    const project = (await api.projects()).find((p) => p.key === 'SYN');
    expect((await api.workstreams(project?.id)).map((w) => w.name)).toEqual(['Synthetic stream']);
    fireEvent.click(screen.getByRole('button', { name: 'New workstream' }));
    const stream = within(await screen.findByRole('dialog', { name: 'New workstream' }));
    expect((await stream.findByLabelText('Project') as HTMLSelectElement).value).toBe(project?.id);
    fill(stream, 'Name', 'Second stream');
    fireEvent.click(stream.getByRole('button', { name: 'Create' }));
    await screen.findByRole('heading', { name: 'Second stream', level: 1 });
  });
});
it('suggests editable keys and validates roots by machine platform', () => {
  expect(suggestedKey('123 synthetic project')).toBe('SYNTHETICP');
  const machine = { id: 'demo', name: 'Demo', kind: 'local', liveness: 'live' } as const;
  expect(absoluteRoot('/home/sam/project', { ...machine, info: { os: 'linux' } })).toBe(true);
  expect(absoluteRoot('C:\\demo', { ...machine, info: { os: 'linux' } })).toBe(false);
  expect(absoluteRoot('C:\\demo', { ...machine, info: { os: 'windows' } })).toBe(true);
  expect(absoluteRoot('C:demo', { ...machine, info: { os: 'windows' } })).toBe(false);
  expect(absoluteRoot('\\\\demo\\share\\project', { ...machine, info: { os: 'windows' } })).toBe(true);
  expect(absoluteRoot('/home/sam', { ...machine, kind: 'wsl', info: { os: 'windows' } })).toBe(true);
});
