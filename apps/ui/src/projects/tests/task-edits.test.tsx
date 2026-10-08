// @vitest-environment happy-dom
// @vitest-environment-options {"url": "http://localhost:5173/"}
import { fireEvent, screen, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { useState } from 'react';
import { OpenExternalProvider } from '../../console/render/links.tsx';
import type { Task } from '../../data/index.ts';
import { clearToasts, Toaster } from '../../design/index.ts';
import { TaskDrawer } from '../task-drawer.tsx';
import { changedFields } from '../task-editor.tsx';
import { NewTaskDialog } from '../new-task.tsx';
import { MyTasksPage } from '../my-tasks.tsx';
import { demo, eventually, otherClient, renderWithHub, startHub, stopHub, type Hub } from './harness.tsx';

function ArchivableDrawer() {
  const [open, setOpen] = useState(true);
  return <><Toaster />{open && <TaskDrawer taskId={demo.pap1} open onOpenChange={setOpen} />}</>;
}

const json = (body: unknown) => new Response(JSON.stringify(body), { headers: { 'Content-Type': 'application/json' } });
/** A synthetic run of PAP-1 in `demo.ses1`, as `GET /v1/tasks/{id}/dispatches` returns it. */
const run = (fields: object) => ({
  id: '01JB000000000000000DSP0099', task: demo.pap1, agent: demo.writer, session: demo.ses1,
  brief: 'Synthetic test brief', started: 1, ...fields,
});
/** Answers the task's dispatch reads with `runs()`; everything else goes to the hub. */
const dispatchesAre = (runs: () => unknown[]): typeof fetch => async (input, init) =>
  new URL(String(input)).pathname.endsWith('/dispatches') ? json(runs()) : fetch(input, init);

describe('changedFields', () => {
  const task: Task = {
    id: 'T1', key: 'PAP-1', project: 'P', title: 'Title', description: 'Text', status: 'todo', priority: 'none',
    labels: ['a', 'b'], blocked_by: [], accept_auto: false, subtasks: [], due: '2026-10-12',
  };
  const form = { title: 'Title', description: 'Text', priority: 'none' as const, labels: ['a', 'b'], start: null, due: '2026-10-12', workstream: null, accept_auto: false };

  it('is empty when nothing changed, empty dates and workstream included', () => {
    expect(changedFields(task, form)).toEqual({});
  });

  it('holds only what changed, with null for a cleared field', () => {
    expect(changedFields(task, { ...form, labels: ['b', 'a'], due: null, priority: 'high' })).toEqual({ labels: ['b', 'a'], due: null, priority: 'high' });
  });
});

describe('task editing', () => {
  let hub: Hub;
  beforeEach(async () => {
    clearToasts();
    hub = await startHub();
  });
  afterEach(async () => {
    await stopHub(hub);
    window.history.replaceState({}, '', '/');
    delete (window as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__;
  });

  it('saves only the fields changed in the form, so a concurrent edit is not reverted', async () => {
    const sent: unknown[] = [];
    const recording: typeof fetch = async (input, init) => {
      if (init?.method === 'PATCH') sent.push(JSON.parse(String(init.body)));
      return fetch(input, init);
    };
    renderWithHub(<TaskDrawer taskId={demo.pap1} open onOpenChange={() => {}} />, hub, { fetch: recording });
    const dialog = await screen.findByRole('dialog', { name: 'Draft the method section' });
    fireEvent.click(await within(dialog).findByRole('button', { name: 'Edit task' }));
    const form = within(within(dialog).getByRole('form', { name: 'Edit task' }));
    // Someone else (or a sync) changes the title and labels while the form is open.
    await otherClient(hub).patchTask(demo.pap1, { title: 'Retitled elsewhere', labels: ['synced'] });
    await within(dialog).findByRole('heading', { name: 'Retitled elsewhere' });
    fireEvent.change(form.getByLabelText('Priority'), { target: { value: 'urgent' } });
    fireEvent.click(form.getByRole('button', { name: 'Save task' }));
    await eventually(() => expect(sent).toEqual([{ priority: 'urgent' }]));
    const task = await otherClient(hub).task('PAP-1');
    expect([task.title, task.labels, task.priority]).toEqual(['Retitled elsewhere', ['synced'], 'urgent']);
  });

  it('offers no dispatch on an archived task', async () => {
    await otherClient(hub).patchTask(demo.pap5, { archived: true });
    renderWithHub(<TaskDrawer taskId={demo.pap5} open onOpenChange={() => {}} />, hub);
    const dialog = await screen.findByRole('dialog', { name: 'Decide what to do about seed 3' });
    const agentRun = within(dialog).getByRole('region', { name: 'Agent run' });
    await within(agentRun).findByText('Restore the task to dispatch an agent to it.');
    expect(within(agentRun).queryByRole('button', { name: 'Dispatch' })).toBeNull();
  });

  it('keeps a session working after its dispatch ended (a report for review), with its chat', async () => {
    renderWithHub(<TaskDrawer taskId={demo.pap1} open onOpenChange={() => {}} />, hub, {
      fetch: dispatchesAre(() => [run({ ended: 2, outcome: 'succeeded', summary: 'Reported for review.' })]),
      nav: { openSession: () => {} },
    });
    const dialog = await screen.findByRole('dialog', { name: 'Draft the method section' });
    const agentRun = within(dialog).getByRole('region', { name: 'Agent run' });
    await within(agentRun).findByText('Run succeeded: Reported for review.');
    await within(agentRun).findByRole('button', { name: 'Open chat' });
    expect(within(agentRun).queryByText('No agent is working on this task.')).toBeNull();
  });

  it('does not announce a run that had already failed when the drawer opened', async () => {
    renderWithHub(<TaskDrawer taskId={demo.pap1} open onOpenChange={() => {}} />, hub, {
      fetch: dispatchesAre(() => [run({ ended: 2, outcome: 'failed', summary: 'An earlier synthetic failure.' })]),
    });
    const dialog = await screen.findByRole('dialog', { name: 'Draft the method section' });
    const agentRun = within(dialog).getByRole('region', { name: 'Agent run' });
    await within(agentRun).findByText('Run failed: An earlier synthetic failure.');
    expect(within(agentRun).queryByRole('alert')).toBeNull();
  });

  it('copies the page URL in a browser and a pitcrew:// deep link in the desktop app', async () => {
    const copied: string[] = [];
    Object.defineProperty(navigator, 'clipboard', {
      configurable: true,
      value: { writeText: (text: string) => { copied.push(text); return Promise.resolve(); } },
    });
    window.history.replaceState({}, '', '/w/01JB000000000000000WSP0001/projects');
    renderWithHub(<TaskDrawer taskId={demo.pap1} open onOpenChange={() => {}} />, hub);
    const dialog = await screen.findByRole('dialog', { name: 'Draft the method section' });
    fireEvent.click(within(dialog).getByRole('button', { name: 'Copy link' }));
    (window as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__ = {};
    fireEvent.click(within(dialog).getByRole('button', { name: 'Copy link' }));
    await eventually(() => expect(copied).toEqual([
      `http://localhost:5173/w/01JB000000000000000WSP0001/tasks/${demo.pap1}`,
      `pitcrew://w/01JB000000000000000WSP0001/task/${demo.pap1}`,
    ]));
  });

  it('opens description links through the host’s opener, never in the app’s window', async () => {
    await otherClient(hub).patchTask(demo.pap1, { description: 'See [the notes](https://example.com/notes) or [this](javascript:alert(1)).' });
    const opened: string[] = [];
    renderWithHub(
      <OpenExternalProvider open={(url) => opened.push(url)}>
        <TaskDrawer taskId={demo.pap1} open onOpenChange={() => {}} />
      </OpenExternalProvider>,
      hub,
    );
    const dialog = await screen.findByRole('dialog', { name: 'Draft the method section' });
    const link = await within(dialog).findByRole('link', { name: 'the notes' });
    const click = new MouseEvent('click', { bubbles: true, cancelable: true });
    link.dispatchEvent(click);
    expect(click.defaultPrevented).toBe(true);
    expect(opened).toEqual(['https://example.com/notes']);
    expect(within(dialog).queryByRole('link', { name: 'this' })).toBeNull();
  });

  it('edits every property and renders Markdown without interpreting raw HTML', async () => {
    renderWithHub(<TaskDrawer taskId={demo.pap1} open onOpenChange={() => {}} />, hub);
    const dialog = await screen.findByRole('dialog', { name: 'Draft the method section' });
    await eventually(() => expect(within(dialog).queryByRole('button', { name: 'Edit task' })).toBeTruthy());
    fireEvent.click(within(dialog).getByRole('button', { name: 'Edit task' }));
    const form = within(dialog).getByRole('form', { name: 'Edit task' });
    const fields = within(form);
    fireEvent.change(fields.getByLabelText('Title'), { target: { value: 'Revised method' } });
    fireEvent.change(fields.getByLabelText('Description'), { target: { value: '**Safe** <img src=x onerror=alert(1)> [bad](javascript:alert)' } });
    fireEvent.change(fields.getByLabelText('Priority'), { target: { value: 'urgent' } });
    fireEvent.change(fields.getByLabelText('Labels'), { target: { value: 'method, phase 1\ntests' } });
    fireEvent.change(fields.getByLabelText('Start date'), { target: { value: '2026-10-01' } });
    fireEvent.change(fields.getByLabelText('Due date'), { target: { value: '2026-10-12' } });
    fireEvent.click(fields.getByRole('checkbox', { name: 'Allow automatic completion after review' }));
    fireEvent.click(fields.getByRole('button', { name: 'Save task' }));
    await eventually(async () => {
      const task = await otherClient(hub).task('PAP-1');
      expect([task.title, task.priority, task.labels, task.start, task.due, task.accept_auto]).toEqual(['Revised method', 'urgent', ['method, phase 1', 'tests'], '2026-10-01', '2026-10-12', true]);
    });
    await within(dialog).findByText('Safe');
    expect(dialog.querySelector('strong')?.textContent).toBe('Safe');
    expect(dialog.querySelector('img[src="x"]')).toBeNull();
    expect(dialog.querySelector('a[href^="javascript:"]')).toBeNull();
  });

  it('archives, keeps a readable full task, and restores it through Undo', async () => {
    const original = await otherClient(hub).task('PAP-1');
    renderWithHub(<ArchivableDrawer />, hub);
    const dialog = await screen.findByRole('dialog', { name: 'Draft the method section' });
    fireEvent.click(await within(dialog).findByRole('button', { name: 'Archive task' }));
    await eventually(async () => expect((await otherClient(hub).task('PAP-1')).archived).toBe(true));
    await eventually(() => expect(screen.queryByRole('dialog')).toBeNull());
    fireEvent.click(await screen.findByRole('button', { name: 'Undo' }));
    await eventually(async () => expect((await otherClient(hub).task('PAP-1')).archived).toBe(false));
    expect((await otherClient(hub).task('PAP-1')).subtasks).toEqual(original.subtasks);
    await eventually(() => expect(screen.queryByRole('button', { name: 'Undo' })).toBeNull());
  });

  it('takes an archive’s Undo away once the task is restored another way', async () => {
    function Restorable() {
      const [open, setOpen] = useState(true);
      return <><Toaster />{open ? <TaskDrawer taskId={demo.pap1} open onOpenChange={setOpen} /> : <button type="button" onClick={() => setOpen(true)}>Reopen</button>}</>;
    }
    renderWithHub(<Restorable />, hub);
    const dialog = await screen.findByRole('dialog', { name: 'Draft the method section' });
    fireEvent.click(await within(dialog).findByRole('button', { name: 'Archive task' }));
    await screen.findByRole('button', { name: 'Undo' });
    fireEvent.click(screen.getByRole('button', { name: 'Reopen' }));
    const reopened = await screen.findByRole('dialog', { name: 'Draft the method section' });
    fireEvent.click(await within(reopened).findByRole('button', { name: 'Restore task' }));
    await eventually(async () => expect((await otherClient(hub).task('PAP-1')).archived).toBe(false));
    await eventually(() => expect(screen.queryByRole('button', { name: 'Undo' })).toBeNull());
    await within(reopened).findByText('PAP-1 restored');
  });

  it('adds/removes dependencies and shows the API cycle refusal without dropping the draft', async () => {
    renderWithHub(<TaskDrawer taskId={demo.pap4} open onOpenChange={() => {}} />, hub);
    const dialog = await screen.findByRole('dialog', { name: 'Run seeds 1–5 on the cluster' });
    await within(dialog).findByRole('option', { name: /PAP-2 ·/ });
    fireEvent.change(within(dialog).getByRole('combobox', { name: 'Add dependency' }), { target: { value: demo.pap2 } });
    fireEvent.click(within(dialog).getByRole('button', { name: 'Add dependency' }));
    await within(dialog).findByRole('alert');
    expect((await otherClient(hub).task('PAP-4')).blocked_by).not.toContain(demo.pap2);
    fireEvent.change(within(dialog).getByRole('combobox', { name: 'Add dependency' }), { target: { value: demo.pap1 } });
    fireEvent.click(within(dialog).getByRole('button', { name: 'Add dependency' }));
    const remove = await within(dialog).findByRole('button', { name: 'Remove dependency PAP-1' });
    fireEvent.click(remove);
    await eventually(async () => expect((await otherClient(hub).task('PAP-4')).blocked_by).not.toContain(demo.pap1));
  });

  it('filters workstreams even when the project defaults, and sends description, labels, priority and column status', async () => {
    let closed = false;
    renderWithHub(<NewTaskDialog defaults={{ project: demo.paper, workstream: demo.seedRuns, status: 'in_progress' }} close={() => { closed = true; }} />, hub);
    await screen.findByRole('option', { name: 'Seed runs' });
    expect(screen.queryByRole('option', { name: 'Parsers' })).toBeNull();
    fireEvent.change(screen.getByLabelText('Title'), { target: { value: 'Plan next experiment' } });
    fireEvent.change(screen.getByLabelText('Description'), { target: { value: 'Compare the controls.' } });
    fireEvent.change(screen.getByLabelText('Priority'), { target: { value: 'high' } });
    fireEvent.change(screen.getByLabelText('Labels'), { target: { value: 'experiment' } });
    fireEvent.click(screen.getByRole('button', { name: 'Create' }));
    await eventually(() => expect(closed).toBe(true));
    const task = (await otherClient(hub).tasks()).find((item) => item.title === 'Plan next experiment');
    expect(task).toMatchObject({ project: demo.paper, workstream: demo.seedRuns, status: 'in_progress', priority: 'high', description: 'Compare the controls.', labels: ['experiment'] });
  });

  it('shows all My tasks groups and remembers the person’s board preference', async () => {
    localStorage.clear();
    renderWithHub(<MyTasksPage />, hub);
    await screen.findByRole('heading', { name: 'Overdue' });
    for (const group of ['Today', 'Upcoming', 'No date', 'Completed']) expect(screen.getByRole('heading', { name: group })).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: 'Board' }));
    expect(localStorage.getItem(`pitcrew:my-tasks-view:${demo.sam}`)).toBe('board');
  });

  it('refetches dispatch results on session_ended and shows the failure instead of Starting', async () => {
    let failed = false;
    const intercepted: typeof fetch = async (input, init) => {
      const url = new URL(String(input));
      if (url.pathname.endsWith('/dispatches')) return new Response(JSON.stringify([{
        id: '01JB000000000000000DSP0099', task: demo.pap1, agent: demo.writer, session: demo.ses1,
        brief: 'Synthetic test brief', started: Date.now(),
        ...(failed ? { ended: Date.now(), outcome: 'failed', summary: 'The synthetic CLI could not start.' } : {}),
      }]), { headers: { 'Content-Type': 'application/json' } });
      const response = await fetch(input, init);
      if (url.pathname === '/v1/sessions' && !failed) {
        const sessions = await response.json() as { id: string; state: string }[];
        return new Response(JSON.stringify(sessions.map((session) => session.id === demo.ses1 ? { ...session, state: 'starting' } : session)), { headers: { 'Content-Type': 'application/json' } });
      }
      return response;
    };
    renderWithHub(<TaskDrawer taskId={demo.pap1} open onOpenChange={() => {}} />, hub, { fetch: intercepted });
    const dialog = await screen.findByRole('dialog', { name: 'Draft the method section' });
    const run = within(dialog).getByRole('region', { name: 'Agent run' });
    await within(run).findByText('Starting');
    failed = true;
    await otherClient(hub).request('POST', `/v1/sessions/${demo.ses1}/end`, { body: { mode: 'kill' } });
    await within(run).findByText('Run failed: The synthetic CLI could not start.', undefined, { timeout: 1000 });
    expect(within(run).queryByText('Starting')).toBeNull();
    // It failed while the drawer was open: news, so it is announced.
    expect(within(run).getByRole('alert').textContent).toBe('Run failed: The synthetic CLI could not start.');
  });
});
