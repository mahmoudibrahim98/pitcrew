// @vitest-environment happy-dom
// @vitest-environment-options {"url": "http://localhost:5173/"}

import { fireEvent, screen, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import type { Receipt } from '../../data/index.ts';
import { sameTarget, type Brief } from '../data.ts';
import { WhereItStands } from '../where-it-stands.tsx';
import { AGENT_TOKEN, demo, eventually, otherClient, renderWithHub, startHub, stopHub, type Hub } from './harness.tsx';

const PAPER_TEXT =
  'The method section is about half drafted. Seeds 1, 2, 4 and 5 are training on the cluster (epoch 12 of 40); seed 3 diverged and is waiting for your decision. Answers to co-author comments are drafted and waiting for review.';
const PAPER_NEXT = 'Decide on seed 3; figure 3 can start when the runs finish.';
const PAPER_PROJECT = { kind: 'project', id: demo.paper } as const;

/**
 * The fixture's only `brief_proposed` (on the PAP project) has no `next`; this adds one to the
 * hub's real `GET /v1/briefs` answer, to check the proposal shows it when there is one.
 */
function withProposedNext(step: string): typeof fetch {
  return (async (input, init) => {
    const href = typeof input === 'string' ? input : input instanceof Request ? input.url : input.toString();
    const res = await fetch(input, init);
    if (new URL(href).pathname !== '/v1/briefs') return res;
    const briefs = (await res.json()) as Brief[];
    const withNext = briefs.map((brief) =>
      sameTarget(brief.target, PAPER_PROJECT) && brief.proposal !== undefined
        ? { ...brief, proposal: { ...brief.proposal, next: step } }
        : brief,
    );
    return new Response(JSON.stringify(withNext), { status: 200, headers: { 'Content-Type': 'application/json' } });
  }) as typeof fetch;
}

describe('WhereItStands', () => {
  let hub: Hub;

  beforeEach(async () => {
    hub = await startHub();
  });

  afterEach(async () => {
    await stopHub(hub);
  });

  it('renders the fixture briefs with their receipts', async () => {
    const opened: Receipt[] = [];
    renderWithHub(
      <>
        <WhereItStands target={{ kind: 'project', id: demo.paper }} />
        <WhereItStands target={{ kind: 'workstream', id: demo.seedRuns }} title="Seed runs" />
        <WhereItStands target={{ kind: 'workstream', id: demo.parsers }} title="Parsers" />
      </>,
      hub,
      { nav: { openReceipt: (r) => opened.push(r) } },
    );
    const paper = await screen.findByRole('region', { name: 'Where it stands' });
    await within(paper).findByText(PAPER_NEXT);
    // The fixture's pending proposal repeats the brief's own text, so it shows up twice: once as
    // "Where it stands", once in the "Proposed update" box below (see the proposal tests below).
    expect(within(paper).getAllByText(PAPER_TEXT)).toHaveLength(2);
    expect(within(paper).getByText(/Written automatically from recent work/)).toBeTruthy();
    const receipts = within(paper).getByRole('list', { name: 'Receipts' });
    expect(within(receipts).getAllByRole('button').map((b) => b.textContent)).toEqual(['Event …0010', 'Job 4815162']);
    fireEvent.click(within(receipts).getByRole('button', { name: 'Job 4815162' }));
    expect(opened).toEqual([{ kind: 'job', scheduler: 'slurm', id: '4815162' }]);

    const seeds = screen.getByRole('region', { name: 'Seed runs' });
    await within(seeds).findByText(/Four of five seeds are healthy/);
    expect(within(seeds).getByText('Pinned')).toBeTruthy();
    expect(within(seeds).getByText(/Written by a person/)).toBeTruthy();
    expect(within(seeds).queryByRole('list', { name: 'Receipts' })).toBeNull();

    const parsers = screen.getByRole('region', { name: 'Parsers' });
    await within(parsers).findByRole('button', { name: 'Transcript @48213' });
    expect(within(parsers).getByRole('button', { name: 'Transcript @48213' }).getAttribute('title')).toBe(
      '“Review parser benchmarks” at byte 48213',
    );
  });

  it('lets a person edit and pin it', async () => {
    const other = otherClient(hub);
    renderWithHub(<WhereItStands target={{ kind: 'workstream', id: demo.submission }} />, hub);
    const region = await screen.findByRole('region', { name: 'Where it stands' });
    fireEvent.click(await within(region).findByRole('button', { name: 'Edit' }));
    fireEvent.change(within(region).getByRole('textbox', { name: 'Where it stands' }), {
      target: { value: '§3 is drafted; figure 3 waits for the runs.' },
    });
    fireEvent.change(within(region).getByRole('textbox', { name: 'Next step' }), {
      target: { value: 'Read §3 end to end.' },
    });
    fireEvent.click(within(region).getByRole('button', { name: 'Save' }));
    await eventually(() => expect(within(region).queryByRole('textbox')).toBeNull());
    expect(within(region).getByText('§3 is drafted; figure 3 waits for the runs.')).toBeTruthy();
    expect(within(region).getByText('Read §3 end to end.')).toBeTruthy();
    expect(within(region).getByText(/Written by a person/)).toBeTruthy();

    fireEvent.click(within(region).getByRole('button', { name: 'Pin' }));
    await within(region).findByText('Pinned');
    const briefs = await other.request<Brief[]>('GET', '/v1/briefs');
    expect(briefs.find((b) => b.target.id === demo.submission)).toMatchObject({
      text: '§3 is drafted; figure 3 waits for the runs.',
      next: 'Read §3 end to end.',
      pinned: true,
      source: 'person',
    });
    expect(within(region).getByRole('button', { name: 'Unpin' }).getAttribute('aria-pressed')).toBe('true');
  });

  it('offers no edit or pin to an agent', async () => {
    renderWithHub(<WhereItStands target={{ kind: 'project', id: demo.paper }} />, hub, { token: AGENT_TOKEN });
    // Briefs are device-only, so an agent sees why nothing loads rather than a form.
    const region = await screen.findByRole('region', { name: 'Where it stands' });
    await within(region).findByRole('alert');
    expect(within(region).queryByRole('button', { name: 'Edit' })).toBeNull();
  });

  it('updates live when someone else edits it', async () => {
    renderWithHub(<WhereItStands target={{ kind: 'project', id: demo.paper }} />, hub);
    const region = await screen.findByRole('region', { name: 'Where it stands' });
    await within(region).findByText(PAPER_NEXT);
    await otherClient(hub).request('PUT', `/v1/briefs/project/${demo.paper}`, {
      body: { text: 'Half the paper is drafted.', pinned: false },
    });
    await eventually(() => expect(within(region).queryByText('Half the paper is drafted.')).not.toBeNull());
  });

  it('shows a pending proposal with its receipts and next step', async () => {
    renderWithHub(<WhereItStands target={PAPER_PROJECT} />, hub, {
      nav: { openReceipt: () => {} },
      fetch: withProposedNext('Rebuild the figures with the final seeds.'),
    });
    const region = await screen.findByRole('region', { name: 'Where it stands' });
    const proposed = await within(region).findByRole('group', { name: 'Proposed update' });
    expect(within(proposed).getByText(PAPER_TEXT)).toBeTruthy();
    expect(within(proposed).getByText('Rebuild the figures with the final seeds.')).toBeTruthy();
    const receipts = within(proposed).getByRole('list', { name: 'Proposal receipts' });
    expect(within(receipts).getAllByRole('button').map((b) => b.textContent)).toEqual(['Event …0010', 'Job 4815162']);
  });

  it('accepting a proposal applies its text and next step, and clears it', async () => {
    const other = otherClient(hub);
    renderWithHub(<WhereItStands target={PAPER_PROJECT} />, hub);
    const region = await screen.findByRole('region', { name: 'Where it stands' });
    // `findByRole` (not `getByRole`): the buttons only appear once `useMe` resolves, which can lag
    // behind the proposal itself.
    fireEvent.click(await within(region).findByRole('button', { name: 'Accept' }));
    await eventually(() => expect(within(region).queryByRole('group', { name: 'Proposed update' })).toBeNull());
    // The fixture's proposal has no next step of its own, so accepting it drops the brief's.
    expect(within(region).queryByText(PAPER_NEXT)).toBeNull();
    const briefs = await other.request<Brief[]>('GET', '/v1/briefs');
    const paper = briefs.find((b) => b.target.id === demo.paper);
    expect(paper?.proposal).toBeUndefined();
    expect(paper?.next).toBeUndefined();
    expect(paper).toMatchObject({ text: PAPER_TEXT, source: 'back_office' });
  });

  it('keeping the current brief clears the proposal without changing the text', async () => {
    const other = otherClient(hub);
    renderWithHub(<WhereItStands target={PAPER_PROJECT} />, hub);
    const region = await screen.findByRole('region', { name: 'Where it stands' });
    fireEvent.click(await within(region).findByRole('button', { name: 'Keep current' }));
    await eventually(() => expect(within(region).queryByRole('group', { name: 'Proposed update' })).toBeNull());
    expect(within(region).getAllByText(PAPER_TEXT)).toHaveLength(1);
    expect(within(region).getByText(PAPER_NEXT)).toBeTruthy();
    // The text happens to match the proposal's, but not its (missing) next step, so this is not
    // an accept: the brief becomes the person's own, with no receipts.
    expect(within(region).getByText(/Written by a person/)).toBeTruthy();
    const briefs = await other.request<Brief[]>('GET', '/v1/briefs');
    const paper = briefs.find((b) => b.target.id === demo.paper);
    expect(paper?.proposal).toBeUndefined();
    expect(paper).toMatchObject({ text: PAPER_TEXT, next: PAPER_NEXT, source: 'person', receipts: [] });
  });
});
