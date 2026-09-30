// @vitest-environment happy-dom
// @vitest-environment-options {"url": "http://localhost:5173/"}

import { fireEvent, screen, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import type { Event } from '../../data/index.ts';
import { Board } from '../board.tsx';
import { TaskDrawer, withMentions } from '../task-drawer.tsx';
import { AGENT_TOKEN, demo, eventually, otherClient, renderWithHub, startHub, stopHub, type Hub } from './harness.tsx';

const drawer = (taskId: string) => <TaskDrawer taskId={taskId} open onOpenChange={() => {}} />;

describe('TaskDrawer', () => {
  let hub: Hub;

  beforeEach(async () => {
    hub = await startHub();
  });

  afterEach(async () => {
    await stopHub(hub);
  });

  it('shows PAP-1’s agent-plan subtasks, marked and read-only for people', async () => {
    renderWithHub(drawer(demo.pap1), hub);
    const dialog = await screen.findByRole('dialog', { name: 'Draft the method section' });
    const list = await within(dialog).findByRole('list', { name: 'Subtasks' });
    await within(list).findAllByText('Agent plan · @writer');
    const items = within(list).getAllByRole('listitem');
    expect(items.map((li) => li.textContent)).toEqual([
      'Read notes/method-outline.mdAgent plan · @writer',
      'Write §3.1 ModelAgent plan · @writer',
      'Write §3.2 Noise scheduleAgent plan · @writer',
      'Write §3.3 Training objectiveAgent plan · @writer',
    ]);
    const boxes = within(list).getAllByRole('checkbox') as HTMLInputElement[];
    expect(boxes.map((b) => [b.checked, b.disabled])).toEqual([
      [true, true],
      [true, true],
      [false, true],
      [false, true],
    ]);
    expect(within(dialog).getByText(/only the agent changes them/)).toBeTruthy();
  });

  it('lets a person tick their own subtasks and add one', async () => {
    const other = otherClient(hub);
    renderWithHub(drawer(demo.pap7), hub);
    const dialog = await screen.findByRole('dialog', { name: 'Set up the submission checklist' });
    const box = within(dialog).getByRole('checkbox', { name: 'Anonymisation check' }) as HTMLInputElement;
    await eventually(() => expect(box.disabled).toBe(false));
    fireEvent.click(box);
    await eventually(async () =>
      expect((await other.task('PAP-7')).subtasks.map((s) => s.done)).toEqual([true, false]),
    );
    fireEvent.change(within(dialog).getByRole('textbox', { name: 'New subtask' }), {
      target: { value: 'Supplementary material' },
    });
    fireEvent.click(within(dialog).getByRole('button', { name: 'Add' }));
    await within(dialog).findByRole('checkbox', { name: 'Supplementary material' });
    const saved = await other.task('PAP-7');
    expect(saved.subtasks.at(-1)).toMatchObject({ text: 'Supplementary material', done: false, source: { kind: 'human' } });
    expect(saved.subtasks.at(-1)?.id).toMatch(/^[0-9A-HJKMNP-TV-Z]{26}$/);
  });

  it('opens from a board card', async () => {
    renderWithHub(<Board project={demo.paper} />, hub);
    fireEvent.click(await screen.findByRole('button', { name: 'Run seeds 1–5 on the cluster' }));
    const dialog = await screen.findByRole('dialog', { name: 'Run seeds 1–5 on the cluster' });
    expect(within(dialog).getByText('Watch to epoch 40')).toBeTruthy();
    expect(within(dialog).getByRole('link', { name: 'github: demo-lab/diffusion-paper#12' }).getAttribute('rel')).toBe(
      'noopener noreferrer',
    );
  });

  it('shows the agent run live, with chat and terminal handed to the shell', async () => {
    const opened: string[] = [];
    renderWithHub(drawer(demo.pap1), hub, {
      nav: { openSession: (session, view) => opened.push(`${session}:${view}`) },
    });
    const dialog = await screen.findByRole('dialog', { name: 'Draft the method section' });
    const run = within(dialog).getByRole('region', { name: 'Agent run' });
    await within(run).findByText('Editing method.tex (§3.2)');
    expect(within(run).getByText('Working')).toBeTruthy();
    fireEvent.click(within(run).getByRole('button', { name: 'Open terminal' }));
    expect(opened).toEqual([`${demo.ses1}:terminal`]);

    await otherClient(hub).request('POST', `/v1/sessions/${demo.ses1}/interrupt`);
    await within(run).findByText('Interrupted');
    expect(within(run).getByText('Waiting')).toBeTruthy();
  });

  it('dispatches to an agent, and shows the hub’s refusal for a done task', async () => {
    renderWithHub(drawer(demo.pap7), hub);
    const dialog = await screen.findByRole('dialog', { name: 'Set up the submission checklist' });
    const run = within(dialog).getByRole('region', { name: 'Agent run' });
    await within(run).findByRole('option', { name: '@writer' });
    fireEvent.click(within(run).getByRole('button', { name: 'Dispatch' }));
    const alert = await within(run).findByRole('alert');
    expect(alert.textContent).toBe('Couldn’t dispatch: PAP-7 is done; reopen it before dispatching.');
  });

  it('dispatches PAP-6, and the new session shows up', async () => {
    renderWithHub(drawer(demo.pap6), hub);
    const dialog = await screen.findByRole('dialog', { name: 'Aggregate the results table' });
    const run = within(dialog).getByRole('region', { name: 'Agent run' });
    await within(run).findByText('No agent is working on this task.');
    fireEvent.change(within(run).getByRole('combobox', { name: 'Dispatch to' }), {
      target: { value: '01JB000000000000000MEM0003' },
    });
    fireEvent.click(within(run).getByRole('button', { name: 'Dispatch' }));
    await within(run).findByText('Starting');
    await within(run).findByText('Reading the brief', undefined, { timeout: 5_000 });
  });

  it('posts a comment with an @mention, which arrives through the stream', async () => {
    const other = otherClient(hub);
    renderWithHub(drawer(demo.pap2), hub);
    const dialog = await screen.findByRole('dialog', { name: 'Make figure 3 from the seed runs' });
    const box = within(dialog).getByRole('textbox', { name: 'Add a comment' });
    fireEvent.change(box, { target: { value: 'Can you check the band, @rev' } });
    fireEvent.click(await within(dialog).findByRole('button', { name: '@reviewer' }));
    expect((box as HTMLTextAreaElement).value).toBe('Can you check the band, @reviewer ');
    fireEvent.click(within(dialog).getByRole('button', { name: 'Comment' }));
    const comments = await within(dialog).findByRole('list', { name: 'Comments' });
    expect(within(comments).getByText('@reviewer').className).toContain('text-accent-text');
    const { events } = await other.request<{ events: Event[] }>('GET', '/v1/events', { query: { task: demo.pap2 } });
    const posted = events.at(-1);
    expect(posted?.body).toEqual({
      type: 'comment_posted',
      data: { task: demo.pap2, text: 'Can you check the band, @reviewer', mentions: [demo.reviewer] },
    });
  });

  it('shows history from activity and moves the task from the status field', async () => {
    const other = otherClient(hub);
    renderWithHub(drawer(demo.pap1), hub);
    const dialog = await screen.findByRole('dialog', { name: 'Draft the method section' });
    const history = await within(dialog).findByRole('list', { name: 'History' });
    expect(within(history).getByText('moved PAP-1 from Todo to In progress')).toBeTruthy();
    fireEvent.change(within(dialog).getByRole('combobox', { name: 'Status' }), { target: { value: 'review' } });
    await eventually(async () => expect((await other.task('PAP-1')).status).toBe('review'));
    await within(history).findByText('moved PAP-1 from In progress to Review');
  });

  it('is read-only for an agent: no status, assignee or dispatch controls', async () => {
    renderWithHub(drawer(demo.pap1), hub, { token: AGENT_TOKEN });
    const dialog = await screen.findByRole('dialog', { name: 'Draft the method section' });
    await within(dialog).findByText('In progress');
    expect(within(dialog).queryByRole('combobox', { name: 'Status' })).toBeNull();
    expect(within(dialog).queryByRole('button', { name: 'Dispatch' })).toBeNull();
    expect(within(dialog).queryByRole('textbox', { name: 'New subtask' })).toBeNull();
  });

  it('lists dependencies both ways', async () => {
    renderWithHub(drawer(demo.pap4), hub);
    const dialog = await screen.findByRole('dialog', { name: 'Run seeds 1–5 on the cluster' });
    const blocks = await within(dialog).findByRole('list', { name: 'Blocks' });
    expect(within(blocks).getByText('Make figure 3 from the seed runs')).toBeTruthy();
    expect(within(blocks).getByText('Aggregate the results table')).toBeTruthy();
  });
});

describe('withMentions', () => {
  it('splits text around known handles', () => {
    const reviewer = { id: 'R', kind: 'agent' as const, handle: '@reviewer', name: 'Reviewer' };
    expect(withMentions('hi @reviewer and @nobody.', [reviewer])).toEqual([
      { text: 'hi ' },
      { text: '@reviewer', member: reviewer },
      { text: ' and ' },
      { text: '@nobody' },
      { text: '.' },
    ]);
  });
});
