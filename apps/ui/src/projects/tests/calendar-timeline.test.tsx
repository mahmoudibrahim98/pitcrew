// @vitest-environment happy-dom
// @vitest-environment-options {"url": "http://localhost:5173/"}
import { fireEvent, screen, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { CalendarPage } from '../calendar.tsx';
import { Timeline } from '../timeline.tsx';
import { NewTaskDialog } from '../new-task.tsx';
import { demo, eventually, otherClient, renderWithHub, startHub, stopHub, type Hub } from './harness.tsx';

describe('Calendar and Timeline against the live hub', () => {
  let hub: Hub;
  beforeEach(async () => { hub = await startHub(); });
  afterEach(async () => { await stopHub(hub); });

  it('moves focus across a month edge and Enter exposes that day’s tasks', async () => {
    renderWithHub(<CalendarPage />, hub);
    const first = await screen.findByRole('button', { name: /Thursday, 1 October 2026,/ });
    first.focus();
    fireEvent.keyDown(first, { key: 'ArrowLeft' });
    await eventually(() => expect(document.activeElement?.getAttribute('data-day')).toBe('2026-09-30'));
    expect(screen.getByRole('heading', { name: 'September 2026' })).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: /Wednesday, 30 September 2026,/ }));
    expect(screen.getByRole('region', { name: 'Selected day tasks' })).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: 'Today' }));
    expect(screen.getByRole('heading', { name: 'October 2026' })).toBeTruthy();
  });

  it('filters by project, workstream and me, opens the drawer and refreshes due dates', async () => {
    renderWithHub(<CalendarPage />, hub);
    const day = await screen.findByRole('button', { name: /Saturday, 10 October 2026, 1 tasks/ });
    fireEvent.click(day);
    const selected = screen.getByRole('region', { name: 'Selected day tasks' });
    fireEvent.click(within(selected).getByRole('button', { name: /PAP-1/ }));
    await screen.findByRole('dialog');
    fireEvent.click(screen.getByRole('button', { name: 'Close' }));
    fireEvent.change(screen.getByLabelText('Project'), { target: { value: demo.paper } });
    fireEvent.change(screen.getByLabelText('Workstream'), { target: { value: demo.submission } });
    fireEvent.click(screen.getByLabelText('Only my tasks'));
    await eventually(() => expect(screen.getByRole('button', { name: /Saturday, 10 October 2026, 0 tasks/ })).toBeTruthy());
    fireEvent.click(screen.getByLabelText('Only my tasks'));
    await otherClient(hub).patchTask(demo.pap1, { due: '2026-10-11' });
    await screen.findByRole('button', { name: /Sunday, 11 October 2026, 1 tasks/ });
    expect(screen.getByRole('button', { name: /Saturday, 10 October 2026, 0 tasks/ })).toBeTruthy();
  });

  it('opens tasks in the projects layout’s one drawer when there is one, so drawers never stack', async () => {
    const opened: string[] = [];
    const nav = { openTask: (id: string) => { opened.push(id); } };
    const calendar = renderWithHub(<CalendarPage />, hub, { nav });
    fireEvent.click(await screen.findByRole('button', { name: /Saturday, 10 October 2026, 1 tasks/ }));
    fireEvent.click(within(screen.getByRole('region', { name: 'Selected day tasks' })).getByRole('button', { name: /PAP-1/ }));
    expect(opened).toEqual([demo.pap1]);
    expect(screen.queryByRole('dialog')).toBeNull();
    calendar.unmount();

    renderWithHub(<Timeline project={demo.paper} />, hub, { nav });
    const table = await screen.findByRole('table');
    fireEvent.click(await within(table).findByRole('button', { name: /PAP-1/ }));
    expect(opened).toEqual([demo.pap1, demo.pap1]);
    expect(screen.queryByRole('dialog')).toBeNull();
  });

  it('shows undated tasks apart and refreshes the date, workstream and status through the stream', async () => {
    renderWithHub(<Timeline project={demo.paper} />, hub);
    await screen.findByRole('rowheader', { name: 'Submission' });
    const api = otherClient(hub);
    await api.patchTask(demo.pap1, { due: null });
    const undated = screen.getByRole('region', { name: 'Tasks without a due date' });
    await within(undated).findByRole('button', { name: /PAP-1/ });
    await api.patchTask(demo.pap1, { due: '2026-10-12', workstream: demo.seedRuns });
    await api.moveTask(demo.pap1, 'done');
    const table = screen.getByRole('table');
    await eventually(() => {
      const row = within(table).getByRole('rowheader', { name: 'Seed runs' }).closest('tr');
      expect(row?.querySelector(`[data-scheduled-task="${demo.pap1}"]`)?.getAttribute('data-status')).toBe('done');
      expect(within(undated).queryByRole('button', { name: /PAP-1/ })).toBeNull();
    });
    fireEvent.click(screen.getByRole('button', { name: 'Months' }));
    expect(screen.getByRole('columnheader', { name: /Oct 2026/ })).toBeTruthy();
  });

  it('offers a due date on New task so the empty state gives an actionable path', async () => {
    const { api } = renderWithHub(<NewTaskDialog close={() => {}} />, hub);
    await screen.findByRole('option', { name: 'Paper · Diffusion study' });
    fireEvent.change(screen.getByLabelText('Title'), { target: { value: 'Calendar test task' } });
    fireEvent.change(screen.getByLabelText('Due date (optional)'), { target: { value: '2026-10-12' } });
    fireEvent.click(screen.getByRole('button', { name: 'Create' }));
    await eventually(async () => expect((await api.tasks({ project: demo.paper })).find((task) => task.title === 'Calendar test task')?.due).toBe('2026-10-12'));
  });
});
