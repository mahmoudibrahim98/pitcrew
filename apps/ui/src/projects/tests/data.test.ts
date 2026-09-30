import { describe, expect, it } from 'vitest';
import type { Api, Event, Member } from '../../data/index.ts';
import { fetchActivity, mentionsIn, newUlid, withRevisions, type ActivityPage } from '../data.ts';
import { describeEvent, plainNames } from '../format.ts';

const member = (id: string, handle: string): Member => ({ id, kind: 'agent', handle, name: handle.slice(1) });

const event = (rev: number): Event => ({
  id: `E${rev}`,
  at: rev,
  workspace: 'W',
  author: 'A',
  body: { type: 'session_ended', data: { session: 'S' } },
});

/** A fake `/v1/events` over revisions 1..total, honouring `before` and `limit` like the hub. */
function fakeEvents(total: number) {
  const calls: Record<string, string | undefined>[] = [];
  const api = {
    request: async (_method: string, _path: string, init: { query: Record<string, string | undefined> }) => {
      calls.push(init.query);
      const limit = Number(init.query.limit);
      const before = init.query.before === undefined ? total + 1 : Number(init.query.before);
      const revs: number[] = [];
      for (let rev = before - 1; rev >= 1 && revs.length < limit; rev--) revs.push(rev);
      revs.reverse();
      const page: ActivityPage = {
        events: revs.map(event),
        from_rev: revs[0] ?? 0,
        to_rev: revs.at(-1) ?? 0,
        at_start: (revs[0] ?? 1) <= 1,
      };
      return page;
    },
  } as unknown as Api;
  return { api, calls };
}

describe('fetchActivity', () => {
  it('asks once for up to 500 events', async () => {
    const { api, calls } = fakeEvents(120);
    const page = await fetchActivity(api, { task: 'T' }, 100);
    expect(calls).toEqual([{ task: 'T', project: undefined, workstream: undefined, session: undefined, before: undefined, limit: '100' }]);
    expect(page).toMatchObject({ from_rev: 21, to_rev: 120, at_start: false });
    expect(page.events).toHaveLength(100);
  });

  it('pages back past the 500 cap, oldest first', async () => {
    const { api, calls } = fakeEvents(1_200);
    const page = await fetchActivity(api, {}, 700);
    expect(calls.map((c) => [c.before, c.limit])).toEqual([
      [undefined, '500'],
      ['701', '200'],
    ]);
    expect(page.events.map((e) => e.at)).toEqual(Array.from({ length: 700 }, (_, i) => 501 + i));
    expect(page).toMatchObject({ from_rev: 501, to_rev: 1_200, at_start: false });
  });

  it('stops at the start', async () => {
    const { api } = fakeEvents(30);
    const page = await fetchActivity(api, {}, 600);
    expect(page.events).toHaveLength(30);
    expect(page.at_start).toBe(true);
  });
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
});
