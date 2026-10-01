// @vitest-environment happy-dom
// @vitest-environment-options {"url": "http://localhost:5173/"}
//
// Recaps in the Projects layout against the mock hub (crates/fixtures/data/demo-recaps.json): the
// Activity "Summary" of a project and of a workstream, clause marking (multi-byte text included),
// a clause's evidence, paging, and the blocks of work of a session and a task. The harness passes
// `tz: 0`, the only offset the mock has days for.

import { act, fireEvent, screen, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { DaysPage, Receipt, Summary } from '../../data/index.ts';
import { ActivityFeed } from '../activity.tsx';
import { RecapSummary, WorkBlocks } from '../recaps.tsx';
import { TaskDetail } from '../task-drawer.tsx';
import { demo, eventually, renderWithHub, startHub, stopHub, type Hub } from './harness.tsx';

const ses2 = '01JB000000000000000SES0002';
const event = (n: string): Receipt => ({ kind: 'event', id: `01JB000000000000000EVT${n}` });

/** An element's text as people see it: without the screen-reader-only additions. */
function visibleText(element: Element): string {
  const copy = element.cloneNode(true) as Element;
  for (const hidden of copy.querySelectorAll('.sr-only')) hidden.remove();
  return copy.textContent ?? '';
}

const paragraphs = (root: Element) => [...root.querySelectorAll('[data-summary]')].map(visibleText);
const clauseTexts = (paragraph: Element) => [...paragraph.querySelectorAll('[data-clause]')].map(visibleText);
const dates = (root: Element) => [...root.querySelectorAll('h3 time')].map((t) => t.getAttribute('datetime'));

/** The joining text of a paragraph: its text outside any clause. */
function joiningText(paragraph: Element): string[] {
  return [...paragraph.childNodes].filter((n) => n.nodeType === Node.TEXT_NODE).map((n) => n.textContent ?? '');
}

async function openSummary() {
  fireEvent.click(screen.getByRole('radio', { name: 'Summary' }));
  return screen.findByRole('list', { name: 'Days' });
}

const SUBMISSION_0930 =
  '2 bursts of work, 1 file edit (+84 −12), 1 task move. @sam dispatched @writer to PAP-1, @writer moved PAP-1 to in progress, updated the plan for PAP-1 (2 of 4 done). @writer edited method.tex (+84 −12).';
const SEED_RUNS_0930 =
  '3 bursts of work, 1 tool run, 1 ask raised. @runner ran a tool. Job 4815164 diverged, @office asked @sam for a decision ("Seed 3 diverged at epoch 9. Rerun or drop it?"). @office marked Seed runs at risk, @sam pinned the brief for Seed runs.';
const SUBMISSION_0929 =
  '@writer finished PAP-3 ("Drafted answers to all 14 comments; 2 need your call."), moved PAP-3 to review.';
const SEED_RUNS_0929 = '@sam dispatched @runner to PAP-4.';

describe('recaps', () => {
  let hub: Hub;

  beforeEach(async () => {
    hub = await startHub();
  });

  afterEach(async () => {
    vi.useRealTimers();
    await stopHub(hub);
  });

  it("shows PRJ0001's Summary: a paragraph per workstream per day, newest day first", async () => {
    renderWithHub(<ActivityFeed filters={{ project: demo.paper }} title="Activity" />, hub);
    const days = await openSummary();
    await eventually(() => expect(dates(days)).toEqual(['2026-09-30', '2026-09-29']));
    const [latest, earlier] = within(days).getAllByRole('listitem').filter((li) => li.parentElement === days);
    if (latest === undefined || earlier === undefined) throw new Error('two days expected');
    // Within a date, by workstream (the work outside any workstream would come first).
    await within(latest).findByRole('heading', { level: 4, name: 'Submission' });
    expect(within(latest).getAllByRole('heading', { level: 4 }).map((h) => h.textContent)).toEqual([
      'Submission',
      'Seed runs',
    ]);
    expect(paragraphs(latest)).toEqual([SUBMISSION_0930, SEED_RUNS_0930]);
    expect(paragraphs(earlier)).toEqual([SUBMISSION_0929, SEED_RUNS_0929]);
    expect(screen.queryByRole('button', { name: 'Load older days' })).toBeNull();
    expect(screen.getByText('That’s everything, back to the start.')).toBeTruthy();
  });

  it("shows a workstream's Summary: one paragraph per day", async () => {
    renderWithHub(<ActivityFeed filters={{ workstream: demo.seedRuns }} title="Activity" />, hub);
    const days = await openSummary();
    await eventually(() => expect(dates(days)).toEqual(['2026-09-30', '2026-09-29']));
    expect(paragraphs(days)).toEqual([SEED_RUNS_0930, SEED_RUNS_0929]);
    expect(within(days).queryAllByRole('heading', { level: 4 })).toEqual([]);
  });

  it('marks every clause, byte ranges converted for multi-byte text (the fixture’s −)', async () => {
    renderWithHub(<RecapSummary scope={{ workstream: demo.submission }} />, hub);
    const days = await screen.findByRole('list', { name: 'Days' });
    const paragraph = await eventually(() => {
      const found = days.querySelector<HTMLElement>('[data-summary]');
      if (found === null) throw new Error('no paragraph yet');
      return found;
    });
    expect(visibleText(paragraph)).toBe(SUBMISSION_0930);
    expect(clauseTexts(paragraph)).toEqual([
      '2 bursts of work',
      '1 file edit (+84 −12)',
      '1 task move',
      '@sam dispatched @writer to PAP-1',
      '@writer moved PAP-1 to in progress',
      'updated the plan for PAP-1 (2 of 4 done)',
      '@writer edited method.tex (+84 −12)',
    ]);
    // The joining text is plain, outside every clause.
    expect(joiningText(paragraph)).toEqual([', ', ', ', '. ', ', ', ', ', '. ', '.']);
    // Each clause is a button whose name says it has evidence.
    within(paragraph).getByRole('button', { name: '1 file edit (+84 −12), with evidence (1 receipt)' });
    within(paragraph).getByRole('button', { name: '2 bursts of work, with evidence (2 receipts)' });
  });

  it('renders a synthetic summary as text only: emoji, accents, CJK and markup kept literally', async () => {
    const text = 'Fixed the café 🎉 parser, wrote <img src=x onerror="alert(1)"> **docs** for 日本語.';
    const bytes = (s: string) => new TextEncoder().encode(s).length;
    const span = (clause: string, receipts: Receipt[]) => {
      const at = text.indexOf(clause);
      const start = bytes(text.slice(0, at));
      return { range: { start, end: start + bytes(clause) }, receipts };
    };
    const summary: Summary = {
      text,
      spans: [
        span('Fixed the café 🎉 parser', [event('0004')]),
        span('wrote <img src=x onerror="alert(1)"> **docs** for 日本語', [event('0007')]),
      ],
    };
    const page: DaysPage = {
      days: [{ workstream: demo.submission, date: '2026-09-30', blocks: [], summary }],
      at_start: true,
    };
    const fakeDays: typeof fetch = async (input, init) => {
      const href = typeof input === 'string' ? input : input instanceof Request ? input.url : input.toString();
      if (new URL(href).pathname === '/v1/recaps/days') {
        return new Response(JSON.stringify(page), { headers: { 'Content-Type': 'application/json' } });
      }
      return fetch(input, init);
    };
    const { container } = renderWithHub(<RecapSummary scope={{ workstream: demo.submission }} />, hub, { fetch: fakeDays });
    const paragraph = await eventually(() => {
      const found = container.querySelector('[data-summary]');
      if (found === null) throw new Error('no paragraph yet');
      return found;
    });
    expect(visibleText(paragraph)).toBe(text);
    expect(clauseTexts(paragraph)).toEqual(['Fixed the café 🎉 parser', 'wrote <img src=x onerror="alert(1)"> **docs** for 日本語']);
    expect(joiningText(paragraph)).toEqual([', ', '.']);
    expect(container.querySelector('img')).toBeNull();
    expect(container.querySelector('strong, em, b')).toBeNull();
  });

  it('opens a clause’s receipts by keyboard, and leads to its session, task and events', async () => {
    const openReceipt = vi.fn();
    const openSession = vi.fn();
    const openTask = vi.fn();
    renderWithHub(<RecapSummary scope={{ workstream: demo.submission }} />, hub, {
      nav: { openReceipt, openSession, openTask },
    });
    const clause = await screen.findByRole('button', { name: /^@writer moved PAP-1 to in progress, with evidence/ });
    expect(clause.getAttribute('aria-expanded')).toBe('false');

    // Focus previews the evidence, without moving focus or making it interactive.
    act(() => clause.focus());
    const preview = await screen.findByRole('dialog', { name: 'Evidence for “@writer moved PAP-1 to in progress”' });
    expect(preview.hasAttribute('inert')).toBe(true);
    expect(document.activeElement).toBe(clause);

    // Enter opens it: focus moves in, and Tab reaches the receipts and links.
    fireEvent.keyDown(clause, { key: 'Enter' });
    expect(clause.getAttribute('aria-expanded')).toBe('true');
    const dialog = screen.getByRole('dialog', { name: 'Evidence for “@writer moved PAP-1 to in progress”' });
    await eventually(() => expect(document.activeElement).toBe(dialog));
    expect(dialog.hasAttribute('inert')).toBe(false);
    expect(clause.getAttribute('aria-controls')).toBe(dialog.id);

    const receipts = within(dialog).getByRole('list', { name: 'Receipts' });
    fireEvent.click(within(receipts).getByRole('button', { name: 'Event …0005' }));
    expect(openReceipt).toHaveBeenCalledWith(event('0005'));

    // The block the receipt is in: its session and the task the fact names.
    const where = await within(dialog).findByRole('list', { name: 'Where it happened' });
    fireEvent.click(await within(where).findByRole('button', { name: 'Draft method section' }));
    expect(openSession).toHaveBeenCalledWith(demo.ses1, 'chat');
    fireEvent.click(await within(where).findByRole('button', { name: 'PAP-1' }));
    expect(openTask).toHaveBeenCalledWith(demo.pap1);

    // Escape closes it and returns to the clause, without previewing again.
    act(() => dialog.focus());
    fireEvent.keyDown(dialog, { key: 'Escape' });
    await eventually(() => expect(screen.queryByRole('dialog')).toBeNull());
    expect(document.activeElement).toBe(clause);
    expect(clause.getAttribute('aria-expanded')).toBe('false');
  });

  it('shows a file a clause is about, and opens the evidence on click', async () => {
    renderWithHub(<RecapSummary scope={{ workstream: demo.submission }} />, hub);
    const clause = await screen.findByRole('button', { name: /^1 file edit \(\+84 −12\), with evidence/ });
    fireEvent.click(clause);
    const dialog = await screen.findByRole('dialog', { name: 'Evidence for “1 file edit (+84 −12)”' });
    const files = await within(dialog).findByRole('list', { name: 'Files' });
    expect(visibleText(files)).toBe('method.tex +84 −12');
    // A second click on the clause closes it.
    fireEvent.click(clause);
    await eventually(() => expect(screen.queryByRole('dialog')).toBeNull());
  });

  it('previews a clause’s receipts on hover', async () => {
    renderWithHub(<RecapSummary scope={{ workstream: demo.seedRuns }} />, hub);
    const clause = await screen.findByRole('button', { name: /^@runner ran a tool, with evidence \(2 receipts\)/ });
    fireEvent.pointerEnter(clause);
    const preview = await screen.findByRole('dialog', { name: 'Evidence for “@runner ran a tool”' });
    const receipts = within(preview).getByRole('list', { name: 'Receipts' });
    expect(within(receipts).getAllByRole('listitem').map((li) => li.textContent)).toEqual([
      'Event …0009',
      'Transcript @118220',
    ]);
    fireEvent.pointerLeave(clause);
    await eventually(() => expect(screen.queryByRole('dialog')).toBeNull());
  });

  it('pages back to the start with "Load older days"', async () => {
    renderWithHub(<RecapSummary scope={{ project: demo.paper }} daysPerPage={1} />, hub);
    const days = await screen.findByRole('list', { name: 'Days' });
    await eventually(() => expect(dates(days)).toEqual(['2026-09-30']));
    expect(screen.queryByText('That’s everything, back to the start.')).toBeNull();
    fireEvent.click(screen.getByRole('button', { name: 'Load older days' }));
    await eventually(() => expect(dates(days)).toEqual(['2026-09-30', '2026-09-29']));
    await screen.findByText('That’s everything, back to the start.');
    expect(screen.queryByRole('button', { name: /^Load older days/ })).toBeNull();
    expect(paragraphs(days)).toEqual([SUBMISSION_0930, SEED_RUNS_0930, SUBMISSION_0929, SEED_RUNS_0929]);
  });

  it("lists a day's blocks under its paragraph, loading older blocks when it needs them", async () => {
    renderWithHub(<RecapSummary scope={{ workstream: demo.submission }} blocksPerPage={1} />, hub);
    const days = await screen.findByRole('list', { name: 'Days' });
    await eventually(() => expect(dates(days)).toEqual(['2026-09-30', '2026-09-29']));
    const [latest, earlier] = within(days).getAllByRole('button', { name: /bursts? of work$/ });
    if (latest === undefined || earlier === undefined) throw new Error('two disclosures expected');
    expect(latest.textContent).toBe('2 bursts of work');
    expect(earlier.textContent).toBe('1 burst of work');

    // The oldest day's block is on the third page of blocks (one block per page here).
    fireEvent.click(earlier);
    expect(earlier.getAttribute('aria-expanded')).toBe('true');
    const older = await screen.findByRole('list', { name: /^Bursts of work, / }, { timeout: 8_000 });
    expect(paragraphs(older)).toEqual(['@writer finished PAP-3, moved PAP-3 to review']);
    expect(older.closest('[id]')?.id).toBe(earlier.getAttribute('aria-controls'));

    // The newer day's blocks, in the order the paragraph tells them.
    fireEvent.click(latest);
    const lists = await eventually(() => {
      const found = screen.getAllByRole('list', { name: /^Bursts of work, / });
      expect(found).toHaveLength(2);
      return found;
    });
    const newer = lists.find((list) => list !== older);
    if (newer === undefined) throw new Error('no list for the newer day');
    expect(paragraphs(newer)).toEqual([
      '@sam dispatched @writer to PAP-1, @writer moved PAP-1 to in progress, updated the plan for PAP-1 (2 of 4 done)',
      '@writer edited method.tex (+84 −12)',
    ]);
    expect(within(newer).getByText('1 file touched (+84 −12)')).toBeTruthy();

    fireEvent.click(earlier);
    expect(earlier.getAttribute('aria-expanded')).toBe('false');
    expect(screen.getAllByRole('list', { name: /^Bursts of work, / })).toHaveLength(1);
  });

  it("shows a session's blocks of work, newest first, with their counts and Load older", async () => {
    renderWithHub(<WorkBlocks filters={{ session: ses2 }} blocksPerPage={2} />, hub);
    const list = await screen.findByRole('list', { name: 'Bursts of work' });
    await eventually(() => expect(paragraphs(list)).toHaveLength(2));
    expect(paragraphs(list)).toEqual(['job 4815164 diverged, @office asked @sam for a decision', '@runner ran a tool']);
    const [diverged, ran] = within(list).getAllByRole('listitem');
    if (diverged === undefined || ran === undefined) throw new Error('two blocks expected');
    // The session page leaves out its own session, and names the block's tasks.
    await within(diverged).findByText('PAP-4');
    await within(diverged).findByText('PAP-5');
    expect(within(diverged).queryByText('Seed runs 1–5')).toBeNull();
    expect(within(diverged).getByText('1 event')).toBeTruthy();
    expect(within(ran).getByText('1 tool run')).toBeTruthy();

    fireEvent.click(screen.getByRole('button', { name: 'Load older' }));
    await eventually(() => expect(paragraphs(list)).toHaveLength(3));
    expect(paragraphs(list)[2]).toBe('@sam dispatched @runner to PAP-4');
    await eventually(() => expect(screen.queryByRole('button', { name: /^Load older/ })).toBeNull());
  });

  it("shows a task's blocks of work in its Work section", async () => {
    renderWithHub(<TaskDetail taskId={demo.pap1} />, hub);
    const work = await screen.findByRole('region', { name: 'Work' });
    const list = await within(work).findByRole('list', { name: 'Bursts of work' });
    expect(paragraphs(list)).toEqual([
      '@writer edited method.tex (+84 −12)',
      '@sam dispatched @writer to PAP-1, @writer moved PAP-1 to in progress, updated the plan for PAP-1 (2 of 4 done)',
    ]);
    const [edit, dispatch] = within(list).getAllByRole('listitem');
    if (edit === undefined || dispatch === undefined) throw new Error('two blocks expected');
    expect(within(edit).getByText('1 file touched (+84 −12)')).toBeTruthy();
    expect(within(dispatch).getByText('3 events')).toBeTruthy();
    // The task page leaves out its own task, and names the session.
    await within(edit).findByText('Draft method section');
    expect(within(edit).queryByText('PAP-1')).toBeNull();
    expect(screen.queryByRole('button', { name: /^Load older/ })).toBeNull();
  });
});
