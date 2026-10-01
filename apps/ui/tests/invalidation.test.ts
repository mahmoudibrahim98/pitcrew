import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';
import { invalidationMap, keysToInvalidate, type CacheLookup } from '../src/data/invalidation.ts';
import { keys } from '../src/data/keys.ts';
import { EVENT_TYPES, type Event, type EventBody } from '../src/data/types.ts';

const noCache: CacheLookup = { taskWorkstream: () => undefined };

function event(body: EventBody): Event {
  return { id: '01JB0000000000000000000EV1', at: 0, workspace: 'W', author: 'M', body };
}

describe('invalidation map', () => {
  it('has an entry for every event type, and nothing else', () => {
    expect(Object.keys(invalidationMap).sort()).toEqual([...EVENT_TYPES].sort());
    for (const type of EVENT_TYPES) {
      expect(typeof invalidationMap[type]).toBe('function');
    }
  });

  it('matches every EventBody variant in crates/protocol', () => {
    const source = readFileSync(new URL('../../../crates/protocol/src/events.rs', import.meta.url), 'utf8');
    const start = source.indexOf('pub enum EventBody {');
    expect(start).toBeGreaterThan(-1);
    const body = source.slice(start, source.indexOf('\n}', start));
    // Variants sit at one level of indentation; serde renames them to snake_case.
    const variants = [...body.matchAll(/^ {4}([A-Z]\w*)\s*[{(,]/gm)].map((m) =>
      (m[1] ?? '').replace(/(?<!^)([A-Z])/g, '_$1').toLowerCase(),
    );
    expect(variants.length).toBeGreaterThan(0);
    expect(variants.sort()).toEqual([...EVENT_TYPES].sort());
  });

  it('knows every event type the mock hub can send', () => {
    // The protocol is the reference; the mock may lag it (it lacked machine_added, member_added,
    // persona_saved, team_saved and session_updated when they were added to crates/protocol).
    const source = readFileSync(new URL('../../mock-hub/src/types.ts', import.meta.url), 'utf8');
    const union = source.slice(source.indexOf('export type EventBody'), source.indexOf('export interface HostInfo'));
    const types = [...union.matchAll(/type: '([a-z_]+)'/g)].map((m) => m[1] ?? '');
    expect(types.length).toBeGreaterThan(0);
    const known: readonly string[] = EVENT_TYPES;
    expect(types.filter((type) => !known.includes(type))).toEqual([]);
  });

  it('refreshes workspace membership and session facts', () => {
    const member = { id: 'M2', kind: 'agent', handle: '@new', name: 'New' } as const;
    expect(keysToInvalidate([event({ type: 'member_added', data: { member } })], noCache)).toEqual([
      keys.events,
      keys.members,
      keys.me,
    ]);
    expect(
      keysToInvalidate([event({ type: 'session_updated', data: { session: 'S1', title: 'Renamed' } })], noCache),
    ).toEqual([keys.events, keys.sessions.detail('S1'), keys.sessions.lists]);
  });

  it('leaves events that carry whole objects to the patches, apart from the activity feed', () => {
    const task = { id: 'T9' } as never;
    expect(keysToInvalidate([event({ type: 'task_created', data: { task } })], noCache)).toEqual([keys.events]);
    expect(
      keysToInvalidate([event({ type: 'subtasks_replaced', data: { task: 'T9', subtasks: [] } })], noCache),
    ).toEqual([keys.events]);
  });

  it('touches only the newest transcript page', () => {
    const touched = keysToInvalidate(
      [event({ type: 'file_edited', data: { session: 'S1', path: 'a.txt', added: 1, removed: 0 } })],
      noCache,
    );
    expect(touched).toContainEqual(keys.sessions.transcript('S1'));
    expect(touched).not.toContainEqual(['sessions', 'transcript', 'S1']);
  });
  it('task_moved touches the task, the task lists and its workstream', () => {
    const cache: CacheLookup = { taskWorkstream: (id) => (id === 'T1' ? 'WS1' : undefined) };
    const touched = keysToInvalidate(
      [event({ type: 'task_moved', data: { task: 'T1', from: 'todo', to: 'review', mover: { kind: 'person' } } })],
      cache,
    );
    expect(touched).toEqual([
      keys.events,
      keys.tasks.detail('T1'),
      keys.tasks.lists,
      keys.workstreams.detail('WS1'),
    ]);
  });

  it('task_updated touches the task, the task lists, and its old and new workstream', () => {
    const cache: CacheLookup = { taskWorkstream: (id) => (id === 'T1' ? 'WS1' : undefined) };
    const edited = keysToInvalidate([event({ type: 'task_updated', data: { task: 'T1', patch: { title: 'New' } } })], cache);
    expect(edited).toEqual([keys.events, keys.tasks.detail('T1'), keys.tasks.lists, keys.workstreams.detail('WS1')]);
    const moved = keysToInvalidate(
      [event({ type: 'task_updated', data: { task: 'T1', patch: { workstream: 'WS2' } } })],
      cache,
    );
    expect(moved).toEqual([
      keys.events,
      keys.tasks.detail('T1'),
      keys.tasks.lists,
      keys.workstreams.detail('WS1'),
      keys.workstreams.detail('WS2'),
    ]);
    const cleared = keysToInvalidate(
      [event({ type: 'task_updated', data: { task: 'T1', patch: { workstream: null } } })],
      cache,
    );
    expect(cleared).toEqual([keys.events, keys.tasks.detail('T1'), keys.tasks.lists, keys.workstreams.detail('WS1')]);
  });

  it('dedupes keys across a batch', () => {
    const moved = (to: 'review' | 'done') =>
      event({ type: 'task_moved', data: { task: 'T1', from: 'todo', to, mover: { kind: 'person' } } });
    const touched = keysToInvalidate([moved('review'), moved('done')], noCache);
    expect(touched).toEqual([keys.events, keys.tasks.detail('T1'), keys.tasks.lists]);
  });

  it('session events touch that session and not tasks', () => {
    const touched = keysToInvalidate(
      [event({ type: 'session_ended', data: { session: 'S1' } })],
      noCache,
    );
    expect(touched).toEqual([keys.events, keys.sessions.detail('S1'), keys.sessions.lists]);
  });

  it('a state change also touches the newest transcript page (a question or a turn may have landed)', () => {
    const touched = keysToInvalidate(
      [event({ type: 'session_state_changed', data: { session: 'S1', from: 'working', to: 'waiting' } })],
      noCache,
    );
    expect(touched).toEqual([
      keys.events,
      keys.sessions.detail('S1'),
      keys.sessions.lists,
      keys.sessions.transcript('S1'),
    ]);
    expect(touched).not.toContainEqual(['sessions', 'transcript', 'S1']);
  });

  it('refetches everything for an event type this build does not know', () => {
    const unknown = event({ type: 'something_new', data: {} } as unknown as EventBody);
    expect(keysToInvalidate([unknown], noCache)).toEqual([[]]);
  });

  it('touches nothing for an empty batch', () => {
    expect(keysToInvalidate([], noCache)).toEqual([]);
  });
});
