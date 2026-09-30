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

  it('matches every event type in the mock hub', () => {
    // The mock hub mirrors crates/protocol; a type there but not here means a missing entry.
    const source = readFileSync(new URL('../../mock-hub/src/types.ts', import.meta.url), 'utf8');
    const union = source.slice(source.indexOf('export type EventBody'), source.indexOf('export interface HostInfo'));
    const types = [...union.matchAll(/type: '([a-z_]+)'/g)].map((m) => m[1]);
    expect(types.sort()).toEqual([...EVENT_TYPES].sort());
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

  it('dedupes keys across a batch', () => {
    const moved = (to: 'review' | 'done') =>
      event({ type: 'task_moved', data: { task: 'T1', from: 'todo', to, mover: { kind: 'person' } } });
    const touched = keysToInvalidate([moved('review'), moved('done')], noCache);
    expect(touched).toEqual([keys.events, keys.tasks.detail('T1'), keys.tasks.lists]);
  });

  it('session events touch that session and not tasks', () => {
    const touched = keysToInvalidate(
      [event({ type: 'session_state_changed', data: { session: 'S1', from: 'working', to: 'idle' } })],
      noCache,
    );
    expect(touched).toEqual([keys.events, keys.sessions.detail('S1'), keys.sessions.lists]);
  });

  it('refetches everything for an event type this build does not know', () => {
    const unknown = event({ type: 'something_new', data: {} } as unknown as EventBody);
    expect(keysToInvalidate([unknown], noCache)).toEqual([[]]);
  });

  it('touches nothing for an empty batch', () => {
    expect(keysToInvalidate([], noCache)).toEqual([]);
  });
});
