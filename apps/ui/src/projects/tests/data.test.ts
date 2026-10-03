import { describe, expect, it } from 'vitest';
import type { Api, Event, Member } from '../../data/index.ts';
import { ACTIVITY_BUDGET, fetchActivity, mentionsIn, newUlid, withRevisions, type ActivityPage } from '../data.ts';
import { describeEvent, plainNames } from '../format.ts';
import { fakePage } from './fake-events.ts';

const member = (id: string, handle: string): Member => ({ id, kind: 'agent', handle, name: handle.slice(1) });

const event = (rev: number): Event => ({
  id: `E${rev}`,
  at: rev,
  workspace: 'W',
  author: 'A',
  body: { type: 'session_ended', data: { session: 'S' } },
});

/** An `Api` whose only call is the fake events feed (an event's `at` is its revision). */
function fakeEvents(total: number, matches?: (rev: number) => boolean, scan?: number) {
  const calls: Record<string, string | undefined>[] = [];
  const feed = { total, event, ...(matches === undefined ? {} : { matches }), ...(scan === undefined ? {} : { scan }) };
  const api = {
    request: async (_method: string, _path: string, init: { query: Record<string, string | undefined> }) => {
      calls.push(init.query);
      return fakePage(feed, init.query);
    },
  } as unknown as Api;
  return { api, calls };
}

const revsOf = (page: ActivityPage) => page.events.map((e) => e.at);

describe('fetchActivity', () => {
  it('asks for one page first', async () => {
    const { api, calls } = fakeEvents(120);
    const page = await fetchActivity(api, { task: 'T' }, undefined);
    expect(calls).toEqual([
      { task: 'T', project: undefined, workstream: undefined, session: undefined, before: undefined, limit: '50' },
    ]);
    expect(page).toMatchObject({ from_rev: 71, to_rev: 120, at_start: false });
    expect(revsOf(page)).toEqual(Array.from({ length: 50 }, (_, i) => 71 + i));
  });

  it('covers everything down to `reach` in 500s, then adds a page', async () => {
    const { api, calls } = fakeEvents(1_200);
    const page = await fetchActivity(api, {}, 1_151);
    expect(calls.map((c) => [c.before, c.limit])).toEqual([
      [undefined, '500'],
      ['701', '50'],
    ]);
    expect(revsOf(page)).toEqual(Array.from({ length: 550 }, (_, i) => 651 + i));
    expect(page).toMatchObject({ from_rev: 651, to_rev: 1_200, at_start: false });
  });

  it('keeps paging through empty pages that are not at the start, within a budget', async () => {
    // Matches at 5–7 and 950–952, 100 revisions scanned per request: a gap of about 900.
    const matches = (rev: number) => (rev >= 5 && rev <= 7) || (rev >= 950 && rev <= 952);
    const { api, calls } = fakeEvents(1_000, matches, 100);

    const first = await fetchActivity(api, { task: 'T' }, undefined);
    expect(calls).toHaveLength(ACTIVITY_BUDGET);
    expect(calls.map((c) => c.before)).toEqual([undefined, '950', '850', '750', '650', '550', '450', '350']);
    expect(revsOf(first)).toEqual([950, 951, 952]);
    // Where it stopped, to resume from; not the end.
    expect(first).toMatchObject({ from_rev: 250, to_rev: 952, at_start: false });

    calls.length = 0;
    const second = await fetchActivity(api, { task: 'T' }, first.from_rev);
    expect(revsOf(second)).toEqual([5, 6, 7, 950, 951, 952]);
    expect(second).toMatchObject({ to_rev: 952, at_start: true });
  });

  it('spends at most its budget on a feed with nothing in reach', async () => {
    const { api, calls } = fakeEvents(100_000, () => false, 100);
    const page = await fetchActivity(api, { task: 'T' }, undefined);
    expect(calls).toHaveLength(ACTIVITY_BUDGET);
    expect(page).toEqual({ events: [], revisions: [], from_rev: 99_201, to_rev: 0, at_start: false });
  });

  it('stops on a page that cannot be continued', async () => {
    const calls: unknown[] = [];
    const api = {
      request: async () => {
        calls.push(1);
        return { events: [], from_rev: 0, to_rev: 0, at_start: false } satisfies ActivityPage;
      },
    } as unknown as Api;
    const page = await fetchActivity(api, { task: 'T' }, undefined);
    expect(calls).toHaveLength(1);
    expect(page.at_start).toBe(true);
  });

  it('stops at the start', async () => {
    const { api } = fakeEvents(30);
    const page = await fetchActivity(api, {}, undefined);
    expect(page.events).toHaveLength(30);
    expect(page.at_start).toBe(true);
  });
});

it('counts only real events and pages past a metadata-only tail defensively', async () => {
  let calls = 0;
  const api = { request: async () => {
    calls++;
    return calls === 1
      ? { events: Array.from({ length: 50 }, (_, i) => ({ ...event(i + 51), body: { type: 'cursor_moved', data: { scope: 'workspace', rev: 1 } } })), from_rev: 51, to_rev: 100, at_start: false }
      : { events: [event(2), event(40)], revisions: [2, 40], from_rev: 2, to_rev: 40, at_start: true };
  } } as unknown as Api;
  const page = await fetchActivity(api, {}, undefined);
  expect(calls).toBe(2);
  expect(page.events.map((e) => e.id)).toEqual(['E2', 'E40']);
  expect(withRevisions(page).map((e) => e.rev)).toEqual([2, 40]);
});

describe('withRevisions', () => {
  it('numbers an unfiltered page from its last revision', () => {
    const page: ActivityPage = { events: [event(14), event(15)], from_rev: 14, to_rev: 15, at_start: false };
    expect(withRevisions(page).map((e) => e.rev)).toEqual([14, 15]);
  });
});

describe('mentionsIn', () => {
  it('finds members by handle, once each, ignoring e-mail addresses', () => {
    const members = [member('R', '@reviewer'), member('W', '@writer'), member('S', '@sam')];
    expect(mentionsIn('@reviewer and @Writer: see @reviewer, not sam@sam.dev', members)).toEqual(['R', 'W']);
  });
});

describe('newUlid', () => {
  it('makes time-ordered 26-character ULIDs', () => {
    const a = newUlid(1_790_000_000_000);
    const b = newUlid(1_790_000_000_001);
    expect(a).toMatch(/^[0-9A-HJKMNP-TV-Z]{26}$/);
    expect(a.slice(0, 10) < b.slice(0, 10)).toBe(true);
  });
});

describe('describeEvent', () => {
  it('falls back to short ids for names it does not know', () => {
    expect(
      describeEvent(
        { ...event(1), body: { type: 'task_moved', data: { task: 'TASK0001', from: 'todo', to: 'done', mover: { kind: 'person' } } } },
        plainNames,
      ),
    ).toBe('moved …0001 from Todo to Done');
  });

  it('describes a task edit', () => {
    expect(
      describeEvent({ ...event(1), body: { type: 'task_updated', data: { task: 'TASK0001', patch: { title: 'New' } } } }, plainNames),
    ).toBe('edited …0001');
  });
});
