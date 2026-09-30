// @vitest-environment happy-dom
// @vitest-environment-options {"url": "http://localhost:5173/"}

import { cleanup, fireEvent, screen, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { useAsks, type Ask, type Session } from '../../data/index.ts';
import { ChatView } from '../chat-view.tsx';
import { keysForOption, QuestionCard } from '../question-card.tsx';
import { ID, otherClient, renderWithHub, startHub, stubLayout, type HubProcess, type Logged } from './harness.tsx';

const posts = (requests: Logged[], path: string) => requests.filter((r) => r.method === 'POST' && r.path === path);

describe('QuestionCard against the mock hub', () => {
  let hub: HubProcess | undefined;
  let unstub: () => void = () => {};

  beforeEach(() => {
    unstub = stubLayout({ viewport: 20_000, row: 40 });
  });

  afterEach(async () => {
    cleanup();
    unstub();
    await hub?.close();
    hub = undefined;
  });

  it('answers the ask a transcript question was raised as, and the ask closes', async () => {
    hub = await startHub();
    const { requests } = renderWithHub(hub, <ChatView sessionId={ID.ses3} />);
    const card = await screen.findByRole('region', { name: 'Question: Merge the benchmark change into parsers?' });
    await vi.waitFor(() => expect(within(card).getByText('from @reviewer')).toBeTruthy());
    const merge = within(card).getByRole('button', { name: 'Merge it' });
    expect(merge).toHaveProperty('disabled', false);

    fireEvent.click(merge);
    await vi.waitFor(() => expect(within(card).getByTestId('answer').textContent).toBe('Answered: Merge it'));
    expect(posts(requests, `/v1/asks/${ID.ask1}/answer`).map((r) => r.body)).toEqual([{ option: 0 }]);
    expect(within(card).getByRole('button', { name: 'Merge it' })).toHaveProperty('disabled', true);
    expect(card.dataset.answered).toBe('true');

    const asks = await otherClient(hub).asks({ state: 'answered' });
    expect(asks.find((a) => a.id === ID.ask1)?.answer?.option).toBe(0);
    // The waiting session carries on, and its reply arrives through the stream.
    await screen.findByText('Thanks. Carrying on with that.', undefined, { timeout: 5_000 });
  }, 15_000);

  it('answers an ask on its own (as the Inbox shows it) with text', async () => {
    hub = await startHub();
    function Inbox() {
      const asks = useAsks();
      const ask = asks.data?.find((a) => a.id === '01JB000000000000000ASK0002');
      return ask === undefined ? null : <QuestionCard ask={ask} />;
    }
    const { requests } = renderWithHub(hub, <Inbox />);
    const card = await screen.findByRole('region', { name: /^Decision: Seed 3 diverged/ });
    expect(within(card).getByText(/Loss went to NaN/)).toBeTruthy();
    fireEvent.change(within(card).getByRole('textbox'), { target: { value: 'Rerun it at half the rate' } });
    fireEvent.click(within(card).getByRole('button', { name: 'Answer' }));
    await vi.waitFor(() =>
      expect(within(card).getByTestId('answer').textContent).toBe('Answered: Rerun it at half the rate'),
    );
    expect(posts(requests, '/v1/asks/01JB000000000000000ASK0002/answer').map((r) => r.body)).toEqual([
      { text: 'Rerun it at half the rate' },
    ]);
    // A second answer is refused by the hub; the card no longer offers one.
    expect(within(card).queryByRole('textbox')).toBeNull();
  });

  it('answers a live question with no ask by pressing keys in the session', async () => {
    hub = await startHub();
    const session = await otherClient(hub).session(ID.ses4);
    const question = { kind: 'question' as const, at: 0, text: 'Which parser?', options: ['A', 'B', 'C'], offset: 1 };
    const { requests } = renderWithHub(hub, <QuestionCard session={session} question={question} open />);
    const card = await screen.findByRole('region', { name: 'Question: Which parser?' });
    fireEvent.click(within(card).getByRole('button', { name: 'C' }));
    await vi.waitFor(() => expect(within(card).getByTestId('answer').textContent).toBe('Sent: C'));
    expect(posts(requests, `/v1/sessions/${ID.ses4}/keys`).map((r) => r.body)).toEqual([
      { keys: ['down', 'down', 'enter'] },
    ]);
    expect(keysForOption(0)).toEqual(['enter']);
  });

  it('does not offer an answer when the session cannot take input', async () => {
    hub = await startHub();
    const ended: Session = await otherClient(hub).session(ID.ses6);
    const question = { kind: 'question' as const, at: 0, text: 'Keep going?', options: ['Yes'], offset: 1 };
    renderWithHub(hub, <QuestionCard session={ended} question={question} open />);
    const card = await screen.findByRole('region', { name: 'Question: Keep going?' });
    expect(within(card).getByRole('button', { name: 'Yes' })).toHaveProperty('disabled', true);
    expect(within(card).getByText(/cannot take input/)).toBeTruthy();
  });

  it('shows an answered ask as answered', async () => {
    hub = await startHub();
    const asks: Ask[] = await otherClient(hub).asks({ state: 'answered' });
    const approval = asks.find((a) => a.id === '01JB000000000000000ASK0004') as Ask;
    renderWithHub(hub, <QuestionCard ask={approval} />);
    const card = await screen.findByRole('region', { name: /^Approval: Open a pull request/ });
    expect(within(card).getByTestId('answer').textContent).toBe('Answered: Approve');
    expect(within(card).getByRole('button', { name: 'Decline' })).toHaveProperty('disabled', true);
  });
});
