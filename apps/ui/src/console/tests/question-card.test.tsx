// @vitest-environment happy-dom
// @vitest-environment-options {"url": "http://localhost:5173/"}

import { fireEvent, screen, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { appendRecord, userPrompt, type TranscriptRecord } from '../../../../mock-hub/src/transcripts.ts';
import { useAsks, type Ask, type Session } from '../../data/index.ts';
import { ChatView } from '../chat-view.tsx';
import { keysForOption, QuestionCard } from '../question-card.tsx';
import {
  eventually,
  holdPath,
  ID,
  otherClient,
  renderWithHub,
  serveSynthetic,
  startHub,
  stubLayout,
  SYNTHETIC,
  unmountAndSettle,
  type HubProcess,
  type Logged,
} from './harness.tsx';

const posts = (requests: Logged[], path: string) => requests.filter((r) => r.method === 'POST' && r.path === path);

/** A transcript ending in a question no ask was raised for. */
function openQuestion(): TranscriptRecord[] {
  const at = 1_790_000_000_000;
  const records: TranscriptRecord[] = [];
  appendRecord(records, [userPrompt(at, 'Pick a parser for the new format.')]);
  appendRecord(records, [{ kind: 'question', at, text: 'Which parser?', options: ['Streaming', 'Batch'] }]);
  return records;
}

/** Whether `card` offers any way to answer: an enabled option or a text field. */
function answerable(card: HTMLElement): boolean {
  const options = within(card).queryAllByRole('button').filter((b) => !(b as HTMLButtonElement).disabled);
  return options.length > 0 || within(card).queryByRole('textbox') !== null;
}

describe('QuestionCard while the asks are loading', () => {
  let hub: HubProcess | undefined;
  let unstub: () => void = () => {};

  beforeEach(() => {
    unstub = stubLayout({ viewport: 20_000, row: 40 });
  });

  afterEach(async () => {
    await unmountAndSettle();
    unstub();
    await hub?.close();
    hub = undefined;
  });

  it('offers no keys for a question raised as an ask, then answers the ask once asks are in', async () => {
    hub = await startHub();
    const asks = holdPath('/v1/asks');
    const { requests } = renderWithHub(hub, <ChatView sessionId={ID.ses3} />, { fetch: asks.fetch });
    // The card needs the session, so it is there once the session has loaded; the asks have not.
    const card = await screen.findByRole('region', { name: 'Question: Merge the benchmark change into parsers?' });
    expect(requests.some((r) => r.path === '/v1/asks')).toBe(true);
    expect(answerable(card)).toBe(false);
    fireEvent.click(within(card).getByRole('button', { name: 'Merge it' }));
    expect(posts(requests, `/v1/sessions/${ID.ses3}/keys`)).toHaveLength(0);

    asks.release();
    await eventually(() => expect(within(card).getByText('from @reviewer')).toBeTruthy());
    const merge = within(card).getByRole('button', { name: 'Merge it' });
    expect(merge).toHaveProperty('disabled', false);
    fireEvent.click(merge);
    await eventually(() => expect(within(card).getByTestId('answer').textContent).toBe('Answered: Merge it'));
    expect(posts(requests, `/v1/asks/${ID.ask1}/answer`).map((r) => r.body)).toEqual([{ option: 0 }]);
    expect(posts(requests, `/v1/sessions/${ID.ses3}/keys`)).toHaveLength(0);
  }, 15_000);

  it('offers no keys for a question without an ask until asks are in, then answers with keys', async () => {
    hub = await startHub();
    const asks = holdPath('/v1/asks');
    const { requests } = renderWithHub(hub, <ChatView sessionId={SYNTHETIC} />, {
      fetch: serveSynthetic(openQuestion(), asks.fetch),
    });
    const card = await screen.findByRole('region', { name: 'Question: Which parser?' });
    expect(answerable(card)).toBe(false);
    fireEvent.click(within(card).getByRole('button', { name: 'Batch' }));
    expect(posts(requests, `/v1/sessions/${SYNTHETIC}/keys`)).toHaveLength(0);

    asks.release();
    await eventually(() => expect(within(card).getByRole('button', { name: 'Batch' })).toHaveProperty('disabled', false));
    // A CLI's picker takes keys, not text: no text field for a question with options.
    expect(within(card).queryByRole('textbox')).toBeNull();
    fireEvent.click(within(card).getByRole('button', { name: 'Batch' }));
    await eventually(() => expect(within(card).getByTestId('answer').textContent).toBe('Sent: Batch'));
    expect(posts(requests, `/v1/sessions/${SYNTHETIC}/keys`).map((r) => r.body)).toEqual([{ keys: ['down', 'enter'] }]);
  }, 15_000);
});

describe('QuestionCard against the mock hub', () => {
  let hub: HubProcess | undefined;
  let unstub: () => void = () => {};

  beforeEach(() => {
    unstub = stubLayout({ viewport: 20_000, row: 40 });
  });

  afterEach(async () => {
    await unmountAndSettle();
    unstub();
    await hub?.close();
    hub = undefined;
  });

  it('answers the ask a transcript question was raised as, and the ask closes', async () => {
    hub = await startHub();
    const { requests } = renderWithHub(hub, <ChatView sessionId={ID.ses3} />);
    const card = await screen.findByRole('region', { name: 'Question: Merge the benchmark change into parsers?' });
    await eventually(() => expect(within(card).getByText('from @reviewer')).toBeTruthy());
    const merge = within(card).getByRole('button', { name: 'Merge it' });
    expect(merge).toHaveProperty('disabled', false);

    fireEvent.click(merge);
    await eventually(() => expect(within(card).getByTestId('answer').textContent).toBe('Answered: Merge it'));
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
    await eventually(() =>
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
    await eventually(() => expect(within(card).getByTestId('answer').textContent).toBe('Sent: C'));
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
