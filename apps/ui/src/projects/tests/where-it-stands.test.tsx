// @vitest-environment happy-dom
// @vitest-environment-options {"url": "http://localhost:5173/"}

import { fireEvent, screen, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import type { BriefTarget, Event, Receipt } from '../../data/index.ts';
import type { Brief } from '../data.ts';
import { WhereItStands, pendingProposal } from '../where-it-stands.tsx';
import { AGENT_TOKEN, demo, eventually, otherClient, renderWithHub, startHub, stopHub, type Hub } from './harness.tsx';

const PAPER_TEXT =
  'The method section is about half drafted. Seeds 1, 2, 4 and 5 are training on the cluster (epoch 12 of 40); seed 3 diverged and is waiting for your decision. Answers to co-author comments are drafted and waiting for review.';

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
    await within(paper).findByText(PAPER_TEXT);
    expect(within(paper).getByText('Decide on seed 3; figure 3 can start when the runs finish.')).toBeTruthy();
    expect(within(paper).getByText(/From the back office/)).toBeTruthy();
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
    await within(region).findByText(PAPER_TEXT);
    await otherClient(hub).request('PUT', `/v1/briefs/project/${demo.paper}`, {
      body: { text: 'Half the paper is drafted.', pinned: false },
    });
    await eventually(() => expect(within(region).queryByText('Half the paper is drafted.')).not.toBeNull());
  });
});

describe('pendingProposal', () => {
  const target: BriefTarget = { kind: 'workstream', id: 'W' };
  const brief: Brief = { target, text: 'Old.', pinned: true, source: 'person', updated: 100, receipts: [] };
  const proposed = (at: number, text: string, id = 'W'): Event => ({
    id: `E${at}`,
    at,
    workspace: 'S',
    author: 'O',
    body: {
      type: 'brief_proposed',
      data: { target: { kind: 'workstream', id }, text, receipts: [{ kind: 'job', scheduler: 'slurm', id: '1' }] },
    },
  });

  it('is the newest proposal for the target when it is newer and different', () => {
    expect(pendingProposal(brief, [proposed(150, 'Older.'), proposed(200, 'New.')], target)).toEqual({
      text: 'New.',
      at: 200,
      receipts: [{ kind: 'job', scheduler: 'slurm', id: '1' }],
    });
  });

  it('is nothing once the brief in force caught up, or for another target', () => {
    expect(pendingProposal(brief, [proposed(50, 'New.')], target)).toBeUndefined();
    expect(pendingProposal(brief, [proposed(200, 'Old.')], target)).toBeUndefined();
    expect(pendingProposal(brief, [proposed(200, 'New.', 'X')], target)).toBeUndefined();
    expect(pendingProposal(brief, [proposed(200, 'New.'), proposed(300, 'Old.')], target)).toBeUndefined();
  });

  it('is the proposal itself when nothing is in force yet', () => {
    expect(pendingProposal(undefined, [proposed(10, 'First.')], target)?.text).toBe('First.');
  });
});
