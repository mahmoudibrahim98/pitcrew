// @vitest-environment happy-dom
// @vitest-environment-options {"url": "http://localhost:5173/"}

// "Draft board" against the mock hub: what will be sent and the estimate come first, the start
// sends what was shown with a CLI found on the hub's machine, and the review creates only the
// tasks the person accepts (none starts accepted).

import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fireEvent, screen, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import type { Event, Task } from '../../data/index.ts';
import { DraftBoardPanel } from '../board-draft.tsx';
import { costLine, estimateLine, type BoardDraft } from '../board-drafts.ts';
import { describeEvent, plainNames } from '../format.ts';
import { demo, eventually, otherClient, renderWithHub, startHub, stopHub, type Hub } from './harness.tsx';

describe('DraftBoardPanel', () => {
  let hub: Hub;
  // Where the mock writes each draft's session token, as the hub gives it to the draft's CLI.
  let tokens: string;

  beforeEach(async () => {
    tokens = mkdtempSync(join(tmpdir(), 'pitcrew-ui-tokens-'));
    hub = await startHub({ PITCREW_MOCK_SESSION_TOKENS: tokens });
  });

  afterEach(async () => {
    await stopHub(hub);
    rmSync(tokens, { recursive: true, force: true });
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

    // Only the CLIs found on the hub's machine are offered (the mock's has all three).
    const cli = (await within(panel).findByLabelText('CLI')) as HTMLSelectElement;
    expect([...cli.options].map((o) => o.value)).toEqual(['claude', 'codex', 'opencode']);
    // Drafted by @writer in Codex; the test sends its proposal as the draft's CLI would, with the
    // session token the draft was given.
    fireEvent.change(await within(panel).findByLabelText('Agent'), { target: { value: demo.writer } });
    fireEvent.change(cli, { target: { value: 'codex' } });
    fireEvent.click(within(panel).getByRole('button', { name: 'Send and draft' }));
    await within(panel).findByText(/@writer is drafting the board in Codex/);
    const [draft] = await person.request<BoardDraft[]>('GET', '/v1/board-drafts');
    expect(draft?.agent).toBe(demo.writer);
    expect(draft?.engine).toBe('codex');
    const token = readFileSync(join(tokens, `${draft?.session}.token`), 'utf8');
    await otherClient(hub, token).request('POST', `/v1/board-drafts/${draft?.id}/proposal`, {
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
    // Nothing starts accepted: the person chooses.
    for (const box of within(list).getAllByRole('checkbox')) expect((box as HTMLInputElement).checked).toBe(false);
    expect(within(panel).getByRole('button', { name: 'Create no tasks' })).toHaveProperty('disabled', true);
    fireEvent.click(within(list).getByLabelText(/Finish the method section/));
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
    fireEvent.click(await within(panel).findByRole('button', { name: 'Send and draft' }));
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
