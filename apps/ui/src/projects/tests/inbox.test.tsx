// @vitest-environment happy-dom
// @vitest-environment-options {"url": "http://localhost:5173/"}

import { fireEvent, screen, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import type { Ask } from '../../data/index.ts';
import { Inbox, groupAsks } from '../inbox.tsx';
import { demo, eventually, otherClient, renderWithHub, startHub, stopHub, type Hub } from './harness.tsx';

const card = (askId: string) => {
  const found = document.querySelector<HTMLElement>(`[data-ask="${askId}"]`);
  if (found === null) throw new Error(`no ask ${askId}`);
  return found;
};

// The question card itself is a lazy chunk (stream M's, from `src/console`); the very first one
// any test in this file renders pays for loading it, which can take longer than the usual
// patience under a busy test run. Later renders in the same file reuse the resolved module.
const FIRST_PAINT = { timeout: 15_000 };

describe('Inbox', () => {
  let hub: Hub;

  beforeEach(async () => {
    hub = await startHub();
  });

  afterEach(async () => {
    await stopHub(hub);
  });

  it(
    'groups my open asks by kind, with their receipts',
    async () => {
      renderWithHub(<Inbox />, hub);
      await screen.findByText('Merge the benchmark change into parsers?', {}, FIRST_PAINT);
      const groups = screen.getAllByRole('heading', { level: 2 }).map((h) => h.textContent);
      expect(groups).toEqual(['Questions 1', 'Decisions 1', 'Reviews 1']);
      expect(screen.getByRole('region', { name: 'Decisions 1' })).toBeTruthy();
      expect(screen.getByText('3 open')).toBeTruthy();
      // The answered approval is not here.
      expect(screen.queryByText('Open a pull request on demo-lab/lab-tools')).toBeNull();

      const decision = card(demo.ask2);
      expect(within(decision).getByText('Seed 3 diverged at epoch 9. Rerun or drop it?')).toBeTruthy();
      const receipts = within(decision).getByRole('list', { name: 'Receipts' });
      expect(within(receipts).getAllByRole('listitem').map((li) => li.textContent)).toEqual([
        'Job 4815164',
        'Transcript @120544',
      ]);
      await eventually(() =>
        expect(within(card(demo.ask3)).getByText('File responses.md').getAttribute('title')).toBe(
          '/home/sam/work/diffusion-paper/paper/responses.md on This laptop',
        ),
      );
      await within(decision).findByText('PAP-5');
    },
    20_000,
  );

  it('answers an ask in place with an option', async () => {
    const other = otherClient(hub);
    renderWithHub(<Inbox />, hub);
    await screen.findByText('Merge the benchmark change into parsers?');
    fireEvent.click(within(card(demo.ask1)).getByRole('button', { name: 'Merge it' }));
    // The console's QuestionCard shows its own answer at once, ahead of the stream.
    await within(card(demo.ask1)).findByText('Answered: Merge it');
    // Answered on the hub, and gone from the open list once the stream says so.
    const asks = await other.asks({ to: demo.sam });
    expect(asks.find((a) => a.id === demo.ask1)).toMatchObject({ state: 'answered', answer: { by: demo.sam, option: 0 } });
    await eventually(() => expect(document.querySelector(`[data-ask="${demo.ask1}"]`)).toBeNull());
    expect(screen.getByText('2 open')).toBeTruthy();
    expect(screen.queryByRole('heading', { name: /^Questions/ })).toBeNull();
    // The Inbox's own live region announces the open count dropping.
    await eventually(() =>
      expect(screen.getAllByRole('status').some((s) => s.textContent?.includes('2 open asks now.'))).toBe(true),
    );
  });

  it('answers a review in my own words', async () => {
    const other = otherClient(hub);
    renderWithHub(<Inbox />, hub);
    await screen.findByText('Co-author responses are ready for review');
    const review = card(demo.ask3);
    expect(within(review).queryByRole('group', { name: 'Options' })).toBeNull();
    fireEvent.change(within(review).getByRole('textbox'), { target: { value: 'Keep 6, rewrite 11.' } });
    fireEvent.click(within(review).getByRole('button', { name: 'Answer' }));
    await eventually(() => expect(document.querySelector(`[data-ask="${demo.ask3}"]`)).toBeNull());
    const asks = await other.asks({ to: demo.sam });
    expect(asks.find((a) => a.id === demo.ask3)?.answer?.text).toBe('Keep 6, rewrite 11.');
  });

  it('shows a new ask as soon as it is raised', async () => {
    renderWithHub(<Inbox />, hub);
    await screen.findByText('3 open');
    await otherClient(hub, 'dev-agent-token').request('POST', '/v1/asks', {
      body: { kind: 'mention', to: demo.sam, title: '@sam: figure 3 colours?', task: demo.pap2 },
    });
    await screen.findByRole('heading', { name: /^Mentions/ });
    expect(screen.getByText('4 open')).toBeTruthy();
  });
});

describe('groupAsks', () => {
  it('orders groups by kind and asks newest first', () => {
    const ask = (id: string, kind: Ask['kind'], created: number): Ask => ({
      id,
      kind,
      from: 'a',
      to: 'b',
      title: id,
      body: '',
      options: [],
      receipts: [],
      state: 'open',
      created,
    });
    const groups = groupAsks([ask('m', 'mention', 1), ask('q1', 'question', 1), ask('q2', 'question', 2)]);
    expect(groups.map((g) => [g.kind, g.asks.map((a) => a.id)])).toEqual([
      ['question', ['q2', 'q1']],
      ['mention', ['m']],
    ]);
  });
});
