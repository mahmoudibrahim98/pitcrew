// @vitest-environment happy-dom
// @vitest-environment-options {"url": "http://localhost:5173/"}
import { fireEvent, screen, within } from '@testing-library/react';
import { createMemoryHistory, RouterProvider } from '@tanstack/react-router';
import { afterEach, expect, it, vi } from 'vitest';
import { feature as projects } from '../../projects/index.ts';
import { createAppRouter } from '../../shell/routes.tsx';
import { initialShellState, useShell } from '../../shell/store.ts';
import { feature } from '../index.ts';
import { resetWorkbenchStores } from '../workbench/store.ts';
import { absoluteFolder } from '../new-session.tsx';
import { eventually, renderWithHub, startHub, unmountAndSettle, type HubProcess } from './harness.tsx';

vi.mock('../terminal/terminal-view.tsx', () => ({ TerminalView: () => <div>Live terminal</div> }));
const ws = '01JB000000000000000WSP0001';
let hub: HubProcess | undefined;
afterEach(async () => { await unmountAndSettle(); await hub?.close(); localStorage.clear(); resetWorkbenchStores(); useShell.setState(initialShellState); });

async function mount(path: string, fetcher?: typeof fetch) {
  hub = await startHub();
  const router = createAppRouter([feature, projects], { history: createMemoryHistory({ initialEntries: [path] }) });
  return { router, ...renderWithHub(hub, <RouterProvider router={router} />, fetcher === undefined ? {} : { fetch: fetcher }) };
}

it.each(['new', 'list', 'workstream'])('starts from %s and opens the new session’s terminal', async (entry) => {
  const path = entry === 'workstream' ? `/w/${ws}/projects/01JB000000000000000PRJ0001/workstreams/01JB000000000000000WST0001` : `/w/${ws}/console`;
  const { router, requests, api } = await mount(path);
  if (entry === 'new') {
    fireEvent.keyDown(await screen.findByRole('button', { name: 'New' }), { key: 'Enter' });
    fireEvent.click(await screen.findByRole('menuitem', { name: 'Session' }));
  } else fireEvent.click(await screen.findByRole('button', { name: 'Start session' }));
  const dialog = await screen.findByRole('dialog', { name: 'New session' });
  const form = within(dialog);
  await eventually(() => expect((form.getByLabelText('Engine') as HTMLSelectElement).options.length).toBe(3));
  if (entry === 'workstream') expect((form.getByLabelText('Folder') as HTMLInputElement).value).not.toBe('');
  fireEvent.change(form.getByLabelText('Folder'), { target: { value: '/home/sam/work/start' } });
  fireEvent.change(form.getByLabelText('Title (optional)'), { target: { value: 'Synthetic launch' } });
  fireEvent.change(form.getByLabelText('First prompt (optional)'), { target: { value: 'Check the synthetic project.' } });
  fireEvent.click(form.getByRole('button', { name: 'Start session' }));
  await eventually(() => expect(router.state.location.href).toMatch(/\/console\/[0-9A-Z]+\?view=terminal$/u));
  expect(await screen.findByText('Live terminal')).toBeTruthy();
  const id = router.state.location.pathname.split('/').at(-1);
  if (id === undefined) throw new Error('No session id');
  const session = await api.session(id);
  expect(session.title).toBe('Synthetic launch');
  expect(session.terminal).toBeTruthy();
  expect(requests.filter((r) => r.method === 'POST' && r.path === '/v1/sessions').map((r) => r.body)).toEqual([
    { ...(entry === 'workstream' ? { workstream: '01JB000000000000000WST0001' } : {}), machine: '01JB000000000000000MCH0001', engine: 'claude', cwd: '/home/sam/work/start', permission_mode: 'default', title: 'Synthetic launch', brief: 'Check the synthetic project.' },
  ]);
});

it.each([409, 503])('keeps input and gives an actionable inline error for %s', async (status) => {
  await mount(`/w/${ws}/console`, (input, init) => init?.method === 'POST' && String(input).endsWith('/v1/sessions')
    ? Promise.resolve(new Response(JSON.stringify({ code: status === 409 ? 'conflict' : 'unavailable', message: 'Synthetic refusal.' }), { status, headers: { 'Content-Type': 'application/json' } })) : fetch(input, init));
  fireEvent.click(await screen.findByRole('button', { name: 'Start session' }));
  const dialog = await screen.findByRole('dialog', { name: 'New session' });
  const form = within(dialog);
  await eventually(() => expect((form.getByLabelText('Engine') as HTMLSelectElement).options.length).toBe(3));
  fireEvent.change(form.getByLabelText('Folder'), { target: { value: '/home/sam/work/start' } });
  fireEvent.change(form.getByLabelText('Engine'), { target: { value: 'codex' } });
  expect(form.getByLabelText('Permission mode').textContent).not.toContain('Plan');
  expect(form.getByLabelText('Permission mode').textContent).not.toContain('Bypass');
  fireEvent.click(form.getByRole('button', { name: 'Start session' }));
  const error = await form.findByRole('alert');
  expect(error.textContent).toContain('Synthetic refusal.');
  expect(error.textContent).toContain(status === 409 ? 'another folder' : 'runner');
  expect((form.getByLabelText('Folder') as HTMLInputElement).value).toBe('/home/sam/work/start');
});

it('validates absolute paths using the selected machine’s syntax', () => {
  expect(absoluteFolder('/home/sam/work', 'unix')).toBe(true);
  expect(absoluteFolder('relative', 'unix')).toBe(false);
  expect(absoluteFolder('C:\\work', 'windows')).toBe(true);
  expect(absoluteFolder('\\\\example.com\\share\\work', 'windows')).toBe(true);
  expect(absoluteFolder('C:work', 'windows')).toBe(false);
  expect(absoluteFolder('/work', 'windows')).toBe(false);
  expect(absoluteFolder('/work\n', 'unix')).toBe(false);
});

it('preselects saved safety within each engine’s modes and keeps an explicit choice', async () => {
  await mount(`/w/${ws}/console`, async (input, init) => String(input).endsWith('/v1/safety') && init?.method !== 'PUT'
    ? new Response(JSON.stringify({ permission_mode: 'plan' }), { headers: { 'Content-Type': 'application/json' } }) : fetch(input, init));
  fireEvent.click(await screen.findByRole('button', { name: 'Start session' }));
  const form = within(await screen.findByRole('dialog', { name: 'New session' }));
  await eventually(() => expect(form.getByLabelText('Permission mode')).toHaveProperty('value', 'plan'));
  fireEvent.change(form.getByLabelText('Engine'), { target: { value: 'codex' } });
  expect(form.getByLabelText('Permission mode')).toHaveProperty('value', 'default');
  fireEvent.change(form.getByLabelText('Engine'), { target: { value: 'opencode' } });
  expect(form.getByLabelText('Permission mode')).toHaveProperty('value', 'default');
  fireEvent.change(form.getByLabelText('Engine'), { target: { value: 'claude' } });
  expect(form.getByLabelText('Permission mode')).toHaveProperty('value', 'plan');
  fireEvent.change(form.getByLabelText('Permission mode'), { target: { value: 'accept_edits' } });
  expect(form.getByLabelText('Permission mode')).toHaveProperty('value', 'accept_edits');
});

it('explains batch-wrapper first-prompt limits and disables the start before any request', async () => {
  const { requests } = await mount(`/w/${ws}/console`, async (input, init) => {
    const response = await fetch(input, init);
    if (!String(input).endsWith('/session-options')) return response;
    const data = await response.json();
    data.engines[0].first_prompt_forbidden = ['\n', '"', '%', '!', '^', '&', '|', '<', '>', '(', ')'];
    return new Response(JSON.stringify(data), { headers: { 'Content-Type': 'application/json' } });
  });
  fireEvent.click(await screen.findByRole('button', { name: 'Start session' }));
  const form = within(await screen.findByRole('dialog', { name: 'New session' }));
  fireEvent.change(form.getByLabelText('Folder'), { target: { value: '/home/sam/work/start' } });
  await eventually(() => expect(form.getByRole('button', { name: 'Start session' })).toHaveProperty('disabled', false));
  for (const prompt of ['first\nsecond', '"', '%', '!', '^', '&', '|', '<', '>', '(', ')']) {
    fireEvent.change(form.getByLabelText('First prompt (optional)'), { target: { value: prompt } });
    expect(form.getByRole('button', { name: 'Start session' })).toHaveProperty('disabled', true);
    expect(form.getByRole('alert').textContent).toContain('enter it in the terminal');
  }
  expect(requests.filter((r) => r.method === 'POST' && r.path === '/v1/sessions')).toHaveLength(0);
  fireEvent.change(form.getByLabelText('First prompt (optional)'), { target: { value: '' } });
  expect(form.getByRole('button', { name: 'Start session' })).toHaveProperty('disabled', false);
});
