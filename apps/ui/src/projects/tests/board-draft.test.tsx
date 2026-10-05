// @vitest-environment happy-dom
// @vitest-environment-options {"url": "http://localhost:5173/"}

// "Draft board" against the mock hub: what will be sent and the estimate come first, the start
// sends what was shown, and the review creates only the accepted tasks.

import { fireEvent, screen, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import type { Event, Task } from '../../data/index.ts';
import { DraftBoardPanel } from '../board-draft.tsx';
import { costLine, estimateLine, type BoardDraft } from '../board-drafts.ts';
import { describeEvent, plainNames } from '../format.ts';
import { AGENT_TOKEN, demo, eventually, otherClient, renderWithHub, startHub, stopHub, type Hub } from './harness.tsx';

describe('DraftBoardPanel', () => {
  let hub: Hub;

  beforeEach(async () => {
    hub = await startHub();
  });

  afterEach(async () => {
    await stopHub(hub);
  });

  it('shows the cost first, then creates only the accepted tasks', async () => {
    const person = otherClient(hub);
    const before = await person.request<Task[]>('GET', '/v1/tasks', { query: { workstream: demo.submission } });
    renderWithHub(<DraftBoardPanel workstream={demo.submission} onClose={() => undefined} />, hub);
    const panel = await screen.findByRole('region', { name: 'Draft the board from history' });

    // Cost first: sessions, sizes, redactions and the estimate; and what will be sent, as text.
    await within(panel).findByText(/^2 sessions, 4 existing tasks:/);
    expect(within(panel).getByText(/^Estimated usage: about/)).toBeTruthy();
    const sent = within(panel).getByLabelText('What will be sent');
    expect(sent.textContent).toContain(demo.ses1);
    expect(sent.textContent).toContain('PAP-1');
    // Nothing has started.
    expect(await person.request<BoardDraft[]>('GET', '/v1/board-drafts')).toEqual([]);

    // Drafted by @writer, whose proposal the test sends as the agent.
    fireEvent.change(within(panel).getByLabelText('Agent'), { target: { value: demo.writer } });
    fireEvent.click(within(panel).getByRole('button', { name: 'Send and draft' }));
    await within(panel).findByText(/@writer is drafting the board in Claude Code/);
    const [draft] = await person.request<BoardDraft[]>('GET', '/v1/board-drafts');
    expect(draft?.agent).toBe(demo.writer);
    await otherClient(hub, AGENT_TOKEN).request('POST', `/v1/board-drafts/${draft?.id}/proposal`, {
      body: {
        tasks: [
          { title: 'Finish the method section', status: 'in_progress', evidence: [demo.ses1] },
          { title: 'Ask for a second review', status: 'todo', description: 'Synthetic.' },
        ],
        note: 'Synthetic note.',
      },
    });

    // The review: each task accepted or not; nothing exists until then.
    const list = await within(panel).findByRole('list', { name: 'Proposed tasks' }, { timeout: 10_000 });
    expect(within(panel).getByText('Synthetic note.')).toBeTruthy();
    expect(await person.request<Task[]>('GET', '/v1/tasks', { query: { workstream: demo.submission } })).toHaveLength(
      before.length,
    );
    fireEvent.click(within(list).getByLabelText(/Ask for a second review/));
    fireEvent.click(within(panel).getByRole('button', { name: 'Create 1 task' }));
    await within(panel).findByText(/^Created PAP-8 on the board/);
    const after = await person.request<Task[]>('GET', '/v1/tasks', { query: { workstream: demo.submission } });
    expect(after).toHaveLength(before.length + 1);
    expect(after.find((t) => t.key === 'PAP-8')?.labels).toEqual(['drafted']);
    expect(after.some((t) => t.title === 'Ask for a second review')).toBe(false);
  }, 30_000);

  it('rejects a whole proposal, creating nothing', async () => {
    const person = otherClient(hub);
    const before = (await person.request<Task[]>('GET', '/v1/tasks')).length;
    renderWithHub(<DraftBoardPanel workstream={demo.submission} onClose={() => undefined} />, hub);
    const panel = await screen.findByRole('region', { name: 'Draft the board from history' });
    // The back office (the default agent) drafts; the mock plays it.
    await within(panel).findByText(/^2 sessions/);
    fireEvent.click(within(panel).getByRole('button', { name: 'Send and draft' }));
    await within(panel).findByRole('list', { name: 'Proposed tasks' }, { timeout: 15_000 });
    fireEvent.click(within(panel).getByRole('button', { name: 'Reject all' }));
    await within(panel).findByText('Rejected: no task was created.');
    expect((await person.request<Task[]>('GET', '/v1/tasks')).length).toBe(before);
    await eventually(async () => {
      const [draft] = await person.request<BoardDraft[]>('GET', '/v1/board-drafts');
      expect(draft?.state).toBe('reviewed');
    });
  }, 30_000);

  it('words the cost and the estimate', () => {
    const cost = {
      sessions: 1,
      sessions_left_out: 3,
      tasks: 0,
      summary_bytes: 2048,
      prompt_bytes: 5120,
      redacted: 2,
      estimate: { input_tokens: 16_280, output_tokens: 8192 },
    };
    expect(costLine(cost)).toBe(
      '1 session (3 older left out), 0 existing tasks: a 2.0 KB summary in a 5.0 KB prompt; 2 secret-looking items redacted.',
    );
    expect(estimateLine(cost)).toBe(
      'Estimated usage: about 16,000 tokens read, at most 8,000 written for the proposal, on your own agent’s plan.',
    );
  });

  it('tells the board events in the activity feed', () => {
    const event = (type: string, data: unknown) =>
      ({ id: 'e', at: 0, workspace: 'w', author: demo.sam, body: { type, data } }) as unknown as Event;
    const names = { ...plainNames, workstream: () => 'Submission', member: () => '@office' };
    expect(describeEvent(event('board_draft_started', { workstream: demo.submission, agent: 'M' }), names)).toBe(
      'asked @office to draft the board of Submission',
    );
    expect(describeEvent(event('board_proposed', { workstream: demo.submission, tasks: [{}, {}] }), names)).toBe(
      'proposed a board of 2 tasks for Submission',
    );
    expect(describeEvent(event('board_draft_reviewed', { workstream: demo.submission, accepted: [{}] }), names)).toBe(
      'reviewed the drafted board of Submission: 1 task accepted',
    );
  });
});
