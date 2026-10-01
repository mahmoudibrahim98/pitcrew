import { describe, expect, it } from 'vitest';
import { clauses, recapScopeForEvents, recapScopeMap, type RecapCacheLookup } from '../src/data/recaps.ts';
import { EVENT_TYPES, type Event, type EventBody, type Receipt, type Summary } from '../src/data/types.ts';

// ─── clauses() ──────────────────────────────────────────────────────────────────────────────────

function encode(text: string): Uint8Array {
  return new TextEncoder().encode(text);
}

/** The UTF-8 byte range of `phrase`'s first occurrence in `text` (for building test spans). */
function byteRange(text: string, phrase: string): { start: number; end: number } {
  const full = encode(text);
  const part = encode(phrase);
  outer: for (let i = 0; i <= full.length - part.length; i++) {
    for (let j = 0; j < part.length; j++) {
      if (full[i + j] !== part[j]) continue outer;
    }
    return { start: i, end: i + part.length };
  }
  throw new Error(`"${phrase}" not found in "${text}"`);
}

const receipt = (id: string): Receipt => ({ kind: 'event', id });

describe('clauses()', () => {
  it('returns the whole text as one unreceipted clause when there are no spans', () => {
    expect(clauses({ text: 'Nothing happened.', spans: [] })).toEqual([{ text: 'Nothing happened.', receipts: [] }]);
    expect(clauses({ text: '', spans: [] })).toEqual([]);
  });

  it('splits text and spans in order, with the joining punctuation unreceipted', () => {
    const summary: Summary = {
      text: '@writer edited a.txt, then ran tests.',
      spans: [
        { range: byteRange('@writer edited a.txt, then ran tests.', '@writer edited a.txt'), receipts: [receipt('E1')] },
        { range: byteRange('@writer edited a.txt, then ran tests.', 'ran tests'), receipts: [receipt('E2')] },
      ],
    };
    expect(clauses(summary)).toEqual([
      { text: '@writer edited a.txt', receipts: [receipt('E1')] },
      { text: ', then ', receipts: [] },
      { text: 'ran tests', receipts: [receipt('E2')] },
      { text: '.', receipts: [] },
    ]);
  });

  it('decodes multi-byte UTF-8 clauses (É, −, an emoji) correctly, unlike slicing the JS string', () => {
    // "Édited" (É = 2 bytes, 1 UTF-16 unit) and "−2" (− U+2212 = 3 bytes, 1 UTF-16 unit) push every
    // later byte offset ahead of the matching UTF-16 index; the emoji (4 bytes, a UTF-16 surrogate
    // pair) does the same from the other side. A span computed in bytes and sliced as a JS string
    // (UTF-16) would cut these clauses in the wrong place.
    const text = 'Édited −2 files, then 🚀 shipped, done.';
    const first = 'Édited −2 files';
    const second = '🚀 shipped';
    const summary: Summary = {
      text,
      spans: [
        { range: byteRange(text, first), receipts: [receipt('E1')] },
        { range: byteRange(text, second), receipts: [receipt('E2')] },
      ],
    };
    const result = clauses(summary);
    expect(result.map((c) => c.text)).toEqual([first, ', then ', second, ', done.']);
    expect(result[0]?.receipts).toEqual([receipt('E1')]);
    expect(result[2]?.receipts).toEqual([receipt('E2')]);

    // Both spans would be wrong if sliced directly as a JS (UTF-16) string with the byte range.
    const firstSpan = summary.spans[0];
    const secondSpan = summary.spans[1];
    if (firstSpan === undefined || secondSpan === undefined) throw new Error('expected two spans');
    expect(text.slice(firstSpan.range.start, firstSpan.range.end)).not.toBe(first);
    expect(text.slice(secondSpan.range.start, secondSpan.range.end)).not.toBe(second);
  });

  it('handles a span that starts at 0 and one that runs to the end of the text', () => {
    const text = 'Done.';
    const summary: Summary = { text, spans: [{ range: byteRange(text, 'Done'), receipts: [receipt('E1')] }] };
    expect(clauses(summary)).toEqual([
      { text: 'Done', receipts: [receipt('E1')] },
      { text: '.', receipts: [] },
    ]);
  });
});

// ─── recapScopeForEvents() ──────────────────────────────────────────────────────────────────────

function event(body: EventBody): Event {
  return { id: '01JB0000000000000000000EV1', at: 0, workspace: 'W', author: 'M', body };
}

function fakeCache(data: {
  sessions?: Record<string, { task?: string; workstream?: string }>;
  tasks?: Record<string, { workstream?: string; project: string }>;
  workstreams?: Record<string, { project: string }>;
  dispatches?: Record<string, { task: string; session?: string }>;
  asks?: Record<string, { task?: string; session?: string }>;
}): RecapCacheLookup {
  return {
    session: (id) => data.sessions?.[id],
    task: (id) => data.tasks?.[id],
    workstream: (id) => data.workstreams?.[id],
    dispatch: (id) => data.dispatches?.[id],
    ask: (id) => data.asks?.[id],
  };
}

const EMPTY_CACHE = fakeCache({});
const EMPTY_SCOPE = { session: [], task: [], workstream: [], project: [] };

describe('recapScopeForEvents', () => {
  it('touches nothing for an empty batch', () => {
    expect(recapScopeForEvents([], EMPTY_CACHE)).toBeUndefined();
  });

  it('is not activity for machine, persona, team, project and brief-proposal events', () => {
    const notActivity: EventBody[] = [
      { type: 'machine_added', data: { machine: {} as never } },
      { type: 'machine_liveness', data: { machine: 'M1', liveness: 'live' } },
      { type: 'persona_saved', data: { persona: {} as never } },
      { type: 'team_saved', data: { team: {} as never } },
      { type: 'project_created', data: { project: {} as never } },
      { type: 'brief_proposed', data: { target: { kind: 'project', id: 'P1' }, text: 't', receipts: [] } },
    ];
    for (const body of notActivity) {
      expect(recapScopeForEvents([event(body)], EMPTY_CACHE)).toBeUndefined();
    }
  });

  it('invalidates every recap key for member_added (it may rename someone a line names)', () => {
    const member = { id: 'M2', kind: 'agent', handle: '@new', name: 'New' } as const;
    expect(recapScopeForEvents([event({ type: 'member_added', data: { member } })], EMPTY_CACHE)).toBe('everything');
  });

  it('invalidates every recap key for an event type this build does not know', () => {
    const unknown = event({ type: 'something_new', data: {} } as unknown as EventBody);
    expect(recapScopeForEvents([unknown], EMPTY_CACHE)).toBe('everything');
  });

  it('falls back to everything rather than throw on malformed data', () => {
    const malformed = event({ type: 'task_created', data: {} } as unknown as EventBody);
    expect(() => recapScopeForEvents([malformed], EMPTY_CACHE)).not.toThrow();
    expect(recapScopeForEvents([malformed], EMPTY_CACHE)).toBe('everything');
  });

  it('resolves session_discovered from the full session object, plus its task and workstream parents', () => {
    const cache = fakeCache({ tasks: { T1: { workstream: 'W1', project: 'P1' } }, workstreams: { W1: { project: 'P1' } } });
    const session = { id: 'S1', task: 'T1', workstream: 'W1' } as never;
    expect(recapScopeForEvents([event({ type: 'session_discovered', data: { session } })], cache)).toEqual({
      session: ['S1'],
      task: ['T1'],
      workstream: ['W1'],
      project: ['P1'],
    });
  });

  it('resolves a session event through the cache, and falls back to everything when the session is not cached', () => {
    const cache = fakeCache({
      sessions: { S1: { task: 'T1', workstream: 'W1' } },
      tasks: { T1: { workstream: 'W1', project: 'P1' } },
      workstreams: { W1: { project: 'P1' } },
    });
    expect(recapScopeForEvents([event({ type: 'session_ended', data: { session: 'S1' } })], cache)).toEqual({
      session: ['S1'],
      task: ['T1'],
      workstream: ['W1'],
      project: ['P1'],
    });
    expect(recapScopeForEvents([event({ type: 'turn_ended', data: { session: 'S9', receipt: receipt('E1') } })], EMPTY_CACHE)).toBe(
      'everything',
    );
  });

  it('session_linked takes both the old link (from the cache) and the new one', () => {
    const cache = fakeCache({
      sessions: { S1: { task: 'T1', workstream: 'W1' } },
      tasks: { T1: { workstream: 'W1', project: 'P1' }, T2: { workstream: 'W2', project: 'P2' } },
      workstreams: { W1: { project: 'P1' }, W2: { project: 'P2' } },
    });
    const scope = recapScopeForEvents(
      [event({ type: 'session_linked', data: { session: 'S1', task: 'T2', workstream: 'W2', basis: 'manual' } })],
      cache,
    );
    expect(scope).toEqual({ session: ['S1'], task: ['T1', 'T2'], workstream: ['W1', 'W2'], project: ['P1', 'P2'] });
  });

  it('workstream_created resolves from the full object; workstream_changed needs the cache', () => {
    const workstream = { id: 'W1', project: 'P1' } as never;
    expect(recapScopeForEvents([event({ type: 'workstream_created', data: { workstream } })], EMPTY_CACHE)).toEqual({
      ...EMPTY_SCOPE,
      workstream: ['W1'],
      project: ['P1'],
    });
    const cache = fakeCache({ workstreams: { W1: { project: 'P1' } } });
    expect(
      recapScopeForEvents([event({ type: 'workstream_changed', data: { workstream: 'W1', status: 'active', health: 'on_track' } })], cache),
    ).toEqual({ ...EMPTY_SCOPE, workstream: ['W1'], project: ['P1'] });
  });

  it('task_created resolves from the full object; task_moved and task_assigned need the cache', () => {
    const task = { id: 'T1', workstream: 'W1', project: 'P1' } as never;
    expect(recapScopeForEvents([event({ type: 'task_created', data: { task } })], EMPTY_CACHE)).toEqual({
      ...EMPTY_SCOPE,
      task: ['T1'],
      workstream: ['W1'],
      project: ['P1'],
    });
    const cache = fakeCache({ tasks: { T1: { workstream: 'W1', project: 'P1' } } });
    expect(
      recapScopeForEvents([event({ type: 'task_moved', data: { task: 'T1', from: 'todo', to: 'review', mover: { kind: 'person' } } })], cache),
    ).toEqual({ ...EMPTY_SCOPE, task: ['T1'], workstream: ['W1'], project: ['P1'] });
    expect(recapScopeForEvents([event({ type: 'task_assigned', data: { task: 'T1', assignee: 'M1' } })], cache)).toEqual({
      ...EMPTY_SCOPE,
      task: ['T1'],
      workstream: ['W1'],
      project: ['P1'],
    });
  });

  it('task_updated takes the old workstream always, and the new one only when the patch sets one', () => {
    const cache = fakeCache({
      tasks: { T1: { workstream: 'W1', project: 'P1' } },
      workstreams: { W1: { project: 'P1' }, W2: { project: 'P2' } },
    });
    const unrelated = recapScopeForEvents([event({ type: 'task_updated', data: { task: 'T1', patch: { title: 'New' } } })], cache);
    expect(unrelated).toEqual({ ...EMPTY_SCOPE, task: ['T1'], workstream: ['W1'], project: ['P1'] });

    const moved = recapScopeForEvents([event({ type: 'task_updated', data: { task: 'T1', patch: { workstream: 'W2' } } })], cache);
    expect(moved).toEqual({ session: [], task: ['T1'], workstream: ['W1', 'W2'], project: ['P1', 'P2'] });

    const cleared = recapScopeForEvents([event({ type: 'task_updated', data: { task: 'T1', patch: { workstream: null } } })], cache);
    expect(cleared).toEqual({ ...EMPTY_SCOPE, task: ['T1'], workstream: ['W1'], project: ['P1'] });
  });

  it('subtasks_replaced resolves the task like any other task event', () => {
    const cache = fakeCache({ tasks: { T1: { workstream: 'W1', project: 'P1' } } });
    expect(recapScopeForEvents([event({ type: 'subtasks_replaced', data: { task: 'T1', subtasks: [] } })], cache)).toEqual({
      ...EMPTY_SCOPE,
      task: ['T1'],
      workstream: ['W1'],
      project: ['P1'],
    });
  });

  it('dispatch_started resolves the task and names the session directly', () => {
    const cache = fakeCache({ tasks: { T1: { workstream: 'W1', project: 'P1' } } });
    const dispatch = { id: 'D1', task: 'T1', agent: 'M1', session: 'S1', brief: '', started: 0 } as never;
    expect(recapScopeForEvents([event({ type: 'dispatch_started', data: { dispatch } })], cache)).toEqual({
      session: ['S1'],
      task: ['T1'],
      workstream: ['W1'],
      project: ['P1'],
    });
  });

  it('dispatch_finished resolves through the dispatch cache, falling back when it is not cached', () => {
    const cache = fakeCache({
      dispatches: { D1: { task: 'T1', session: 'S1' } },
      tasks: { T1: { workstream: 'W1', project: 'P1' } },
    });
    expect(
      recapScopeForEvents([event({ type: 'dispatch_finished', data: { dispatch: 'D1', outcome: 'succeeded' } })], cache),
    ).toEqual({ session: ['S1'], task: ['T1'], workstream: ['W1'], project: ['P1'] });
    expect(
      recapScopeForEvents([event({ type: 'dispatch_finished', data: { dispatch: 'D9', outcome: 'succeeded' } })], EMPTY_CACHE),
    ).toBe('everything');
  });

  it('ask_raised names the task and session directly, and is empty scope for neither', () => {
    const cache = fakeCache({ tasks: { T1: { workstream: 'W1', project: 'P1' } } });
    const ask = (task?: string, session?: string) => ({ id: 'A1', task, session } as never);
    expect(recapScopeForEvents([event({ type: 'ask_raised', data: { ask: ask('T1', undefined) } })], cache)).toEqual({
      ...EMPTY_SCOPE,
      task: ['T1'],
      workstream: ['W1'],
      project: ['P1'],
    });
    expect(recapScopeForEvents([event({ type: 'ask_raised', data: { ask: ask(undefined, 'S1') } })], cache)).toEqual({
      ...EMPTY_SCOPE,
      session: ['S1'],
    });
    expect(recapScopeForEvents([event({ type: 'ask_raised', data: { ask: ask() } })], cache)).toEqual(EMPTY_SCOPE);
  });

  it('ask_answered resolves through the ask cache, falling back when it is not cached', () => {
    const cache = fakeCache({ asks: { A1: { task: 'T1' } }, tasks: { T1: { workstream: 'W1', project: 'P1' } } });
    expect(
      recapScopeForEvents(
        [event({ type: 'ask_answered', data: { ask: 'A1', answer: { by: 'M1', at: 0 } } })],
        cache,
      ),
    ).toEqual({ ...EMPTY_SCOPE, task: ['T1'], workstream: ['W1'], project: ['P1'] });
    expect(
      recapScopeForEvents([event({ type: 'ask_answered', data: { ask: 'A9', answer: { by: 'M1', at: 0 } } })], EMPTY_CACHE),
    ).toBe('everything');
  });

  it('comment_posted names the task or the workstream directly, and is empty scope for neither', () => {
    const cache = fakeCache({ workstreams: { W1: { project: 'P1' } } });
    expect(
      recapScopeForEvents([event({ type: 'comment_posted', data: { workstream: 'W1', text: 'hi', mentions: [] } })], cache),
    ).toEqual({ ...EMPTY_SCOPE, workstream: ['W1'], project: ['P1'] });
    expect(recapScopeForEvents([event({ type: 'comment_posted', data: { text: 'hi', mentions: [] } })], cache)).toEqual(EMPTY_SCOPE);
  });

  it('brief_accepted resolves a project target directly, and a workstream target through the cache', () => {
    expect(
      recapScopeForEvents(
        [event({ type: 'brief_accepted', data: { target: { kind: 'project', id: 'P1' }, text: 't', pinned: false } })],
        EMPTY_CACHE,
      ),
    ).toEqual({ ...EMPTY_SCOPE, project: ['P1'] });
    const cache = fakeCache({ workstreams: { W1: { project: 'P1' } } });
    expect(
      recapScopeForEvents(
        [event({ type: 'brief_accepted', data: { target: { kind: 'workstream', id: 'W1' }, text: 't', pinned: false } })],
        cache,
      ),
    ).toEqual({ ...EMPTY_SCOPE, workstream: ['W1'], project: ['P1'] });
  });

  it('decision_recorded is empty scope without a workstream, and resolves one when it has it', () => {
    expect(
      recapScopeForEvents([event({ type: 'decision_recorded', data: { text: 'Chose X', receipts: [] } })], EMPTY_CACHE),
    ).toEqual(EMPTY_SCOPE);
    const cache = fakeCache({ workstreams: { W1: { project: 'P1' } } });
    expect(
      recapScopeForEvents(
        [event({ type: 'decision_recorded', data: { workstream: 'W1', text: 'Chose X', receipts: [] } })],
        cache,
      ),
    ).toEqual({ ...EMPTY_SCOPE, workstream: ['W1'], project: ['P1'] });
  });

  it('unions scopes across a batch, short-circuiting on everything, and skipping non-activity events', () => {
    const cache = fakeCache({ tasks: { T1: { workstream: 'W1', project: 'P1' }, T2: { workstream: 'W2', project: 'P2' } } });
    const batch = recapScopeForEvents(
      [
        event({ type: 'machine_liveness', data: { machine: 'M1', liveness: 'live' } }),
        event({ type: 'task_moved', data: { task: 'T1', from: 'todo', to: 'review', mover: { kind: 'person' } } }),
        event({ type: 'task_assigned', data: { task: 'T2', assignee: 'M1' } }),
      ],
      cache,
    );
    expect(batch).toEqual({ session: [], task: ['T1', 'T2'], workstream: ['W1', 'W2'], project: ['P1', 'P2'] });

    const withUnresolved = recapScopeForEvents(
      [
        event({ type: 'task_moved', data: { task: 'T1', from: 'todo', to: 'review', mover: { kind: 'person' } } }),
        event({ type: 'task_moved', data: { task: 'T9', from: 'todo', to: 'review', mover: { kind: 'person' } } }),
      ],
      cache,
    );
    expect(withUnresolved).toBe('everything');
  });

  it('has an entry for every event type', () => {
    expect(Object.keys(recapScopeMap).sort()).toEqual([...EVENT_TYPES].sort());
  });
});
