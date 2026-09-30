// @vitest-environment happy-dom
// @vitest-environment-options {"url": "http://localhost:5173/"}

import { cleanup, fireEvent, render, screen, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { keys, type Member, type Task, type TaskStatus } from '../../data/index.ts';
import { Board, BoardView } from '../board.tsx';
import { AGENT_TOKEN, demo, eventually, otherClient, renderWithHub, startHub, stopHub, type Hub } from './harness.tsx';

const column = (name: string, scope: HTMLElement = document.body) => within(scope).getByRole('group', { name });
const card = (key: string) => {
  const found = document.querySelector<HTMLElement>(`[data-task="${key}"]`);
  if (found === null) throw new Error(`no card ${key}`);
  return found;
};
const cardsIn = (name: string) =>
  Array.from(column(name).querySelectorAll('[data-task]'), (el) => el.getAttribute('data-task')).sort();

/** Presses on a card, moves onto a column and lets go there. */
function drag(from: HTMLElement, to: HTMLElement) {
  fireEvent.pointerDown(from, { button: 0, clientX: 10, clientY: 10, pointerId: 1 });
  fireEvent.pointerMove(to, { button: 0, clientX: 320, clientY: 60, pointerId: 1 });
  fireEvent.pointerUp(to, { button: 0, clientX: 320, clientY: 60, pointerId: 1 });
}

const badges = () =>
  Array.from(document.querySelectorAll<HTMLElement>('[data-task]'))
    .filter((el) => within(el).queryByText('Needs you') !== null)
    .map((el) => el.getAttribute('data-task'))
    .sort();

describe('Board', () => {
  let hub: Hub;

  beforeEach(async () => {
    hub = await startHub();
  });

  afterEach(async () => {
    await stopHub(hub);
  });

  it('shows the demo tasks in their status columns', async () => {
    renderWithHub(<Board project={demo.paper} />, hub);
    await screen.findByText('Draft the method section');
    expect(cardsIn('Backlog')).toEqual(['PAP-6']);
    expect(cardsIn('Todo')).toEqual(['PAP-2', 'PAP-5']);
    expect(cardsIn('In progress')).toEqual(['PAP-1', 'PAP-4']);
    expect(cardsIn('Review')).toEqual(['PAP-3']);
    expect(cardsIn('Done')).toEqual(['PAP-7']);
    // Assignee avatars say person or agent, and agents name their owner.
    expect(await within(card('PAP-1')).findByRole('img', { name: 'Writer, agent of Sam Rivera' })).toBeTruthy();
    expect(within(card('PAP-7')).getByRole('img', { name: 'Sam Rivera' })).toBeTruthy();
    expect(within(card('PAP-5')).getByText('Unassigned')).toBeTruthy();
    expect(within(card('PAP-5')).getByText('Urgent')).toBeTruthy();
    expect(within(card('PAP-1')).getByText('Due 10 Oct')).toBeTruthy();
  });

  it('moves a card a person drags, and the hub keeps it there', async () => {
    const { api } = renderWithHub(<Board project={demo.paper} />, hub);
    await screen.findByText('Make figure 3 from the seed runs');
    drag(card('PAP-2'), column('In progress'));
    expect(cardsIn('In progress')).toContain('PAP-2');
    await eventually(async () => expect((await api.task('PAP-2')).status).toBe('in_progress'));
    // The stream's refresh agrees.
    await new Promise((r) => setTimeout(r, 600));
    expect(cardsIn('In progress')).toContain('PAP-2');
    expect(cardsIn('Todo')).not.toContain('PAP-2');
    expect(screen.queryByRole('alert')).toBeNull();
  });

  it('snaps a card back with the server’s message when the move is refused (409)', async () => {
    // As the agent @writer: its own task PAP-3 may not go from review to done.
    const { api } = renderWithHub(<Board project={demo.paper} />, hub, { token: AGENT_TOKEN });
    await screen.findByText('Respond to co-author comments');
    drag(card('PAP-3'), column('Done'));
    const alert = await screen.findByRole('alert');
    expect(alert.textContent).toContain('Couldn’t move PAP-3 to Done');
    expect(alert.textContent).toContain(
      'An agent may only move a task from backlog or todo to in_progress, or from in_progress to review; not review → done.',
    );
    expect(cardsIn('Review')).toEqual(['PAP-3']);
    expect(cardsIn('Done')).not.toContain('PAP-3');
    expect((await api.task('PAP-3')).status).toBe('review');
  });

  it('settles several moves in flight independently', async () => {
    // As @writer: PAP-2 todo → in progress is allowed, PAP-3 review → done is not.
    const { api } = renderWithHub(<Board project={demo.paper} />, hub, { token: AGENT_TOKEN });
    await screen.findByText('Respond to co-author comments');
    drag(card('PAP-3'), column('Done'));
    drag(card('PAP-2'), column('In progress'));
    const alert = await screen.findByRole('alert');
    expect(alert.textContent).toContain('Couldn’t move PAP-3 to Done');
    await eventually(async () => expect((await api.task('PAP-2')).status).toBe('in_progress'));
    await eventually(() => expect(cardsIn('Review')).toEqual(['PAP-3']));
    expect(cardsIn('In progress')).toContain('PAP-2');
  });

  it('keeps one note per refused move', async () => {
    // As @writer: PAP-3 review → done and PAP-1 in progress → todo are both refused.
    renderWithHub(<Board project={demo.paper} />, hub, { token: AGENT_TOKEN });
    await screen.findByText('Respond to co-author comments');
    drag(card('PAP-3'), column('Done'));
    drag(card('PAP-1'), column('Todo'));
    await eventually(() => expect(screen.getAllByRole('alert')).toHaveLength(2));
    const notes = screen.getAllByRole('alert').map((a) => a.textContent ?? '');
    expect(notes.some((n) => n.startsWith('Couldn’t move PAP-3 to Done'))).toBe(true);
    expect(notes.some((n) => n.startsWith('Couldn’t move PAP-1 to Todo'))).toBe(true);
    fireEvent.click(screen.getByRole('button', { name: 'Dismiss the note about PAP-3' }));
    expect(screen.getAllByRole('alert')).toHaveLength(1);
    expect(cardsIn('Review')).toEqual(['PAP-3']);
    expect(cardsIn('In progress')).toContain('PAP-1');
  });

  it('does not flip an accepted move back when a slow, stale refresh lands late', async () => {
    // The next GET /v1/tasks is answered by the hub at once (before the move), but reaches the
    // board 4 s later: longer than the old 3 s settle window.
    let slow = false;
    const slowFetch: typeof fetch = async (input, init) => {
      const res = await fetch(input, init);
      const url = new URL(String(input));
      if (!slow || url.pathname !== '/v1/tasks' || (init?.method ?? 'GET') !== 'GET') return res;
      slow = false;
      const body = await res.text();
      await new Promise((r) => setTimeout(r, 4_000));
      return new Response(body, { status: res.status, headers: { 'Content-Type': 'application/json' } });
    };
    const { queryClient } = renderWithHub(<Board project={demo.paper} />, hub, { fetch: slowFetch });
    await screen.findByText('Make figure 3 from the seed runs');
    slow = true;
    void queryClient.invalidateQueries({ queryKey: keys.tasks.lists });
    await eventually(() => expect(slow).toBe(false));

    const seen: (string | null)[] = [];
    const sample = setInterval(() => {
      seen.push(card('PAP-2').closest('[data-status]')?.getAttribute('data-status') ?? null);
    }, 20);
    drag(card('PAP-2'), column('In progress'));
    // The stale list lands at about 4 s, the event's refresh after it.
    await new Promise((r) => setTimeout(r, 5_500));
    clearInterval(sample);
    expect(seen.length).toBeGreaterThan(100);
    expect(seen.filter((s) => s !== 'in_progress')).toEqual([]);
    expect(cardsIn('In progress')).toContain('PAP-2');
    expect(screen.queryByRole('alert')).toBeNull();
  }, 20_000);

  it('moves a card with the keyboard, and keeps focus on it', async () => {
    const { api } = renderWithHub(<Board project={demo.paper} />, hub);
    await screen.findByText('Aggregate the results table');
    const handle = screen.getByRole('button', { name: 'Move PAP-6, now Backlog' });
    handle.focus();
    fireEvent.click(handle);
    expect(screen.getByRole('status').textContent).toContain('Picked up PAP-6 in Backlog');
    fireEvent.keyDown(handle, { key: 'ArrowRight' });
    fireEvent.keyDown(handle, { key: 'ArrowRight' });
    expect(screen.getByRole('status').textContent).toBe('PAP-6: In progress.');
    fireEvent.keyDown(handle, { key: 'ArrowLeft' });
    fireEvent.click(handle);
    expect(cardsIn('Todo')).toContain('PAP-6');
    await eventually(async () => expect((await api.task('PAP-6')).status).toBe('todo'));
    await eventually(() =>
      expect(document.activeElement?.getAttribute('aria-label')).toBe('Move PAP-6, now Todo'),
    );
  });

  it('cancels a keyboard move with Escape', async () => {
    const { api } = renderWithHub(<Board project={demo.paper} />, hub);
    await screen.findByText('Aggregate the results table');
    const handle = screen.getByRole('button', { name: 'Move PAP-6, now Backlog' });
    fireEvent.click(handle);
    fireEvent.keyDown(handle, { key: 'ArrowRight' });
    fireEvent.keyDown(handle, { key: 'Escape' });
    expect(handle.getAttribute('aria-pressed')).toBe('false');
    expect(cardsIn('Backlog')).toEqual(['PAP-6']);
    expect((await api.task('PAP-6')).status).toBe('backlog');
  });

  it('moves a card when another client moves the task, without a reload', async () => {
    renderWithHub(<Board project={demo.paper} />, hub);
    await screen.findByText('Make figure 3 from the seed runs');
    await otherClient(hub).moveTask('PAP-2', 'review');
    await eventually(() => expect(cardsIn('Review')).toEqual(['PAP-2', 'PAP-3']));
    expect(cardsIn('Todo')).toEqual(['PAP-5']);
  });

  it('shows Needs you exactly where open asks to me are, and clears it once answered', async () => {
    renderWithHub(<Board />, hub);
    await screen.findByText('Draft the method section');
    const other = otherClient(hub);
    const tasks = await other.tasks();
    const keyOf = (id: string | undefined) => tasks.find((t) => t.id === id)?.key;
    const open = await other.asks({ to: demo.sam, state: 'open' });
    const expected = open.map((a) => keyOf(a.task)).sort();
    expect(expected).toEqual(['PAP-3', 'PAP-5', 'TL-3']);
    await eventually(() => expect(badges()).toEqual(expected));

    await other.request('POST', `/v1/asks/${demo.ask2}/answer`, { body: { option: 1 } });
    await eventually(() => expect(badges()).toEqual(['PAP-3', 'TL-3']));
  });

  it('updates an agent’s live status line from the stream', async () => {
    renderWithHub(<Board project={demo.paper} />, hub);
    await screen.findByText('Editing method.tex (§3.2)');
    expect(within(card('PAP-1')).getByText('Working')).toBeTruthy();
    await otherClient(hub).request('POST', `/v1/sessions/${demo.ses1}/interrupt`);
    await eventually(() => expect(within(card('PAP-1')).queryByText('Interrupted')).not.toBeNull());
    expect(within(card('PAP-1')).getByText('Waiting')).toBeTruthy();
  });

  it('groups by workstream, one lane each', async () => {
    renderWithHub(<Board project={demo.paper} />, hub);
    await screen.findByText('Draft the method section');
    fireEvent.click(screen.getByRole('radio', { name: 'By workstream' }));
    const submission = await screen.findByRole('region', { name: 'Submission' });
    const seeds = screen.getByRole('region', { name: 'Seed runs' });
    expect(within(column('In progress', submission)).getByText('PAP-1')).toBeTruthy();
    expect(within(column('In progress', seeds)).getByText('PAP-4')).toBeTruthy();
    expect(within(column('Todo', seeds)).getByText('PAP-5')).toBeTruthy();
    expect(within(submission).queryByText('PAP-4')).toBeNull();
  });

  it('creates a task from a column', async () => {
    renderWithHub(<Board project={demo.paper} />, hub);
    await screen.findByText('Draft the method section');
    fireEvent.click(screen.getByRole('button', { name: 'Add a task to Todo' }));
    fireEvent.change(screen.getByRole('textbox', { name: 'New task in Todo' }), {
      target: { value: 'Check the camera-ready template' },
    });
    fireEvent.click(screen.getByRole('button', { name: 'Add' }));
    await eventually(() => expect(cardsIn('Todo')).toContain('PAP-8'));
    expect(screen.getByText('Check the camera-ready template')).toBeTruthy();
  });
});

describe('BoardView', () => {
  afterEach(() => {
    cleanup();
    vi.restoreAllMocks();
  });

  it('keeps the DOM small with 10,000 tasks (virtualised columns)', () => {
    // happy-dom has no layout; give every box a size so the virtualiser has a viewport.
    vi.spyOn(HTMLElement.prototype, 'offsetHeight', 'get').mockReturnValue(600);
    vi.spyOn(HTMLElement.prototype, 'offsetWidth', 'get').mockReturnValue(300);
    vi.spyOn(Element.prototype, 'getBoundingClientRect').mockReturnValue({
      x: 0,
      y: 0,
      top: 0,
      left: 0,
      width: 300,
      height: 600,
      right: 300,
      bottom: 600,
      toJSON: () => ({}),
    } as DOMRect);
    const statuses: TaskStatus[] = ['backlog', 'todo', 'in_progress', 'review', 'done'];
    const tasks: Task[] = Array.from({ length: 10_000 }, (_, i) => ({
      id: `T${String(i).padStart(25, '0')}`,
      key: `BIG-${i + 1}`,
      project: 'P',
      title: `Task ${i + 1}`,
      description: '',
      status: statuses[i % statuses.length] ?? 'todo',
      priority: 'none',
      labels: [],
      blocked_by: [],
      accept_auto: false,
      subtasks: [],
    }));
    const started = performance.now();
    render(
      <BoardView
        tasks={tasks}
        statusOf={(t) => t.status}
        pending={new Set()}
        sessions={new Map()}
        needsYou={new Set()}
        members={new Map<string, Member>()}
        lanes={[{ id: 'all' }]}
        laneOf={() => 'all'}
        onMove={() => {}}
        onOpen={() => {}}
      />,
    );
    const rendered = document.querySelectorAll('[data-task]').length;
    expect(rendered).toBeGreaterThan(0);
    expect(rendered).toBeLessThan(200);
    expect(screen.getByRole('group', { name: 'Todo' }).textContent).toContain('2000');
    expect(performance.now() - started).toBeLessThan(5_000);
  });
});
