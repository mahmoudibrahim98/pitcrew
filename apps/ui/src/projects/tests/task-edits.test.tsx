// @vitest-environment happy-dom
// @vitest-environment-options {"url": "http://localhost:5173/"}
import { fireEvent, screen, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { useState } from 'react';
import { Toaster } from '../../design/index.ts';
import { TaskDrawer } from '../task-drawer.tsx';
import { NewTaskDialog } from '../new-task.tsx';
import { MyTasksPage } from '../my-tasks.tsx';
import { demo, eventually, otherClient, renderWithHub, startHub, stopHub, type Hub } from './harness.tsx';

function ArchivableDrawer() {
  const [open, setOpen] = useState(true);
  return <><Toaster />{open && <TaskDrawer taskId={demo.pap1} open onOpenChange={setOpen} />}</>;
}

describe('task editing', () => {
  let hub: Hub;
  beforeEach(async () => { hub = await startHub(); });
  afterEach(async () => { await stopHub(hub); });

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
  });
});
