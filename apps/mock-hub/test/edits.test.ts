// Creating projects and workstreams, editing tasks, and brief next steps (api-v1.md).

import assert from 'node:assert/strict';
import { describe, it } from 'node:test';
import type { RunningServer } from '../src/server.ts';
import type { ApiError, Brief, Dispatch, Event, Project, Task, Workstream } from '../src/types.ts';
import { AGENT, DEVICE, ID, call, withServer } from './helpers.ts';

interface Activity {
  events: Event[];
  from_rev: number;
  to_rev: number;
  at_start: boolean;
}

/** The newest event in the log. */
async function lastEvent(server: RunningServer): Promise<Event | undefined> {
  const res = await call<Activity>(server, 'GET', '/v1/events?limit=1', { token: DEVICE });
  return res.body.events[0];
}

async function rev(server: RunningServer): Promise<number> {
  return (await call<Activity>(server, 'GET', '/v1/events?limit=1', { token: DEVICE })).body.to_rev;
}

/** Asserts an error reply's status and code. */
function refused(res: { status: number; body: unknown }, status: number, what: string): void {
  const codes: Record<number, string> = { 400: 'invalid', 403: 'forbidden', 404: 'not_found', 409: 'conflict' };
  assert.equal(res.status, status, what);
  assert.equal((res.body as ApiError).code, codes[status], what);
}

const UNKNOWN = {
  project: '01JB000000000000000PRJ0099',
  workstream: '01JB000000000000000WST0099',
  member: '01JB000000000000000MEM0099',
  machine: '01JB000000000000000MCH0099',
  task: '01JB000000000000000TSK0099',
};
const LAPTOP = '01JB000000000000000MCH0001';
const LOCAL_ROOT = process.platform === 'win32' ? 'C:/work' : '/work';
/** @office, the back office's agent. */
const OFFICE = '01JB000000000000000MEM0006';
const JOB = { kind: 'job', scheduler: 'slurm', id: '4815170' } as const;
const SEEDS = '01JB000000000000000WST0002';
const PAP2 = '01JB000000000000000TSK0002';
const PAP4 = ID.pap4;

it('archives and restores a task with its original key, plan and history', () => withServer(async (server) => {
  const original = await call<Task>(server, 'GET', '/v1/tasks/PAP-1', { token: DEVICE });
  const before = await rev(server);
  const archived = await call<Task>(server, 'PATCH', '/v1/tasks/PAP-1', { token: DEVICE, json: { archived: true } });
  assert.equal(archived.status, 200);
  assert.equal(archived.body.archived, true);
  assert.deepEqual(archived.body.subtasks, original.body.subtasks);
  assert.deepEqual((await lastEvent(server))?.body, { type: 'task_updated', data: { task: original.body.id, patch: { archived: true } } });
  await call(server, 'PATCH', '/v1/tasks/PAP-1', { token: DEVICE, json: { archived: true } });
  assert.equal(await rev(server), before + 1);
  refused(await call(server, 'PATCH', '/v1/tasks/PAP-1', { token: AGENT, json: { archived: false } }), 403, 'person only');
  refused(await call(server, 'PATCH', '/v1/tasks/PAP-1', { token: DEVICE, json: { archived: 'yes' } }), 400, 'boolean only');
  const restored = await call<Task>(server, 'PATCH', '/v1/tasks/PAP-1', { token: DEVICE, json: { archived: false } });
  assert.equal(restored.status, 200);
  assert.deepEqual(restored.body, { ...original.body, archived: false });
  assert.equal(await rev(server), before + 2);
}));

it('refuses dispatching an archived task until it is restored', () => withServer(async (server) => {
  await call(server, 'PATCH', '/v1/tasks/PAP-5', { token: DEVICE, json: { archived: true } });
  const before = await rev(server);
  const res = await call(server, 'POST', '/v1/tasks/PAP-5/dispatch', { token: DEVICE, json: { agent: ID.runner } });
  refused(res, 409, 'archived');
  assert.match((res.body as ApiError).message, /archived/);
  assert.equal(await rev(server), before, 'nothing appended');
  await call(server, 'PATCH', '/v1/tasks/PAP-5', { token: DEVICE, json: { archived: false } });
  assert.equal((await call(server, 'POST', '/v1/tasks/PAP-5/dispatch', { token: DEVICE, json: { agent: ID.runner } })).status, 202);
}));

it('reads a task’s runs for people only, leaving out runs whose session is excluded', () => withServer(async (server) => {
  const dispatched = await call<Dispatch>(server, 'POST', '/v1/tasks/PAP-5/dispatch', { token: DEVICE, json: { agent: ID.runner } });
  assert.equal(dispatched.status, 202);
  const runs = () => call<Dispatch[]>(server, 'GET', '/v1/tasks/PAP-5/dispatches', { token: DEVICE });
  const shown = await runs();
  assert.equal(shown.status, 200);
  assert.deepEqual(shown.body.map((run) => run.session), [dispatched.body.session]);
  refused(await call(server, 'GET', '/v1/tasks/PAP-5/dispatches', { token: AGENT }), 403, 'person only');
  assert.equal((await call(server, 'PUT', '/v1/import', { token: DEVICE, json: { mode: 'none' } })).status, 200);
  assert.deepEqual((await runs()).body, []);
  assert.equal((await call(server, 'PUT', '/v1/import', { token: DEVICE, json: { mode: 'all' } })).status, 200);
  assert.deepEqual((await runs()).body, shown.body);
}));

describe('POST /v1/projects', () => {
  it('creates a project with the defaults', () =>
    withServer(async (server) => {
      const res = await call<Project>(server, 'POST', '/v1/projects', {
        token: DEVICE,
        json: { key: 'THS', name: 'Thesis' },
      });
      assert.equal(res.status, 201);
      assert.equal(res.body.id.length, 26);
      assert.deepEqual(res.body, {
        id: res.body.id,
        key: 'THS',
        name: 'Thesis',
        status: 'in_progress',
        lead: ID.sam,
        members: [ID.sam],
        external: [],
      });
      const fetched = await call<Project>(server, 'GET', `/v1/projects/${res.body.id}`, { token: DEVICE });
      assert.deepEqual(fetched.body, res.body);
      const event = await lastEvent(server);
      assert.equal(event?.author, ID.sam);
      assert.deepEqual(event?.body, { type: 'project_created', data: { project: res.body } });
      // Its first task gets key THS-1.
      const task = await call<Task>(server, 'POST', '/v1/tasks', {
        token: DEVICE,
        json: { project: res.body.id, title: 'Outline chapter 1' },
      });
      assert.equal(task.body.key, 'THS-1');
    }));

  it('keeps every field given, and always counts the lead as a member', () =>
    withServer(async (server) => {
      const res = await call<Project>(server, 'POST', '/v1/projects', {
        token: DEVICE,
        json: {
          key: 'AB12',
          name: 'Ablations',
          lead: ID.writer,
          members: [ID.runner, ID.runner, ID.sam],
          status: 'planning',
          start: '2026-10-01',
          due: '2026-10-01',
          root: { machine: LAPTOP, path: `${LOCAL_ROOT}/ablations`, branch: 'main' },
        },
      });
      assert.equal(res.status, 201);
      assert.equal(res.body.lead, ID.writer);
      assert.deepEqual(res.body.members, [ID.writer, ID.runner, ID.sam]);
      assert.equal(res.body.status, 'planning');
      assert.deepEqual([res.body.start, res.body.due], ['2026-10-01', '2026-10-01']);
      assert.deepEqual(res.body.root, { machine: LAPTOP, path: `${LOCAL_ROOT}/ablations`, branch: 'main' });
    }));

  it('answers 409 when the key is already used', () =>
    withServer(async (server) => {
      const res = await call<ApiError>(server, 'POST', '/v1/projects', {
        token: DEVICE,
        json: { key: 'PAP', name: 'Another paper' },
      });
      refused(res, 409, 'PAP is taken');
      assert.equal((await call<Project[]>(server, 'GET', '/v1/projects', { token: DEVICE })).body.length, 2);
    }));

  it('rejects malformed projects with 400 and changes nothing', () =>
    withServer(async (server) => {
      const before = await rev(server);
      for (const json of [
        { name: 'No key' },
        { key: 'pap', name: 'Lower case' },
        { key: 'P', name: 'Too short' },
        { key: '1AB', name: 'Starts with a digit' },
        { key: 'TOOLONGKEY1', name: 'Eleven characters' },
        { key: 'A-B', name: 'Punctuation' },
        { key: 'NEW' },
        { key: 'NEW', name: '   ' },
        { key: 'NEW', name: 'x', lead: UNKNOWN.member },
        { key: 'NEW', name: 'x', members: [ID.sam, UNKNOWN.member] },
        { key: 'NEW', name: 'x', status: 'someday' },
        { key: 'NEW', name: 'x', start: '2026-13-01' },
        { key: 'NEW', name: 'x', start: '2026-11-02', due: '2026-11-01' },
        { key: 'NEW', name: 'x', root: { machine: UNKNOWN.machine, path: '/work' } },
        { key: 'NEW', name: 'x', root: { machine: LAPTOP, path: '' } },
      ]) {
        refused(await call(server, 'POST', '/v1/projects', { token: DEVICE, json }), 400, JSON.stringify(json));
      }
      assert.equal(await rev(server), before);
    }));

  it('needs a device token', () =>
    withServer(async (server) => {
      const res = await call(server, 'POST', '/v1/projects', { token: AGENT, json: { key: 'NEW', name: 'x' } });
      refused(res, 403, 'agent');
    }));
});

describe('POST /v1/workstreams', () => {
  it('creates an active, on-track workstream by default', () =>
    withServer(async (server) => {
      const res = await call<Workstream>(server, 'POST', '/v1/workstreams', {
        token: DEVICE,
        json: { project: ID.paper, name: 'Figures' },
      });
      assert.equal(res.status, 201);
      assert.deepEqual(res.body, {
        id: res.body.id,
        project: ID.paper,
        name: 'Figures',
        status: 'active',
        health: 'on_track',
        locations: [],
        external: [],
      });
      const list = await call<Workstream[]>(server, 'GET', `/v1/workstreams?project=${ID.paper}`, { token: DEVICE });
      assert.ok(list.body.some((w) => w.id === res.body.id));
      const event = await lastEvent(server);
      assert.deepEqual(event?.body, { type: 'workstream_created', data: { workstream: res.body } });
    }));

  it('keeps a given status and locations', () =>
    withServer(async (server) => {
      const locations = [{ machine: LAPTOP, path: `${LOCAL_ROOT}/paper/figures` }];
      const res = await call<Workstream>(server, 'POST', '/v1/workstreams', {
        token: DEVICE,
        json: { project: `prj_${ID.tooling}`, name: 'Packaging', status: 'idea', locations },
      });
      assert.equal(res.status, 201);
      assert.equal(res.body.project, ID.tooling);
      assert.equal(res.body.status, 'idea');
      assert.equal(res.body.health, 'on_track');
      assert.deepEqual(res.body.locations, locations);
    }));

  it('answers 404 for an unknown project', () =>
    withServer(async (server) => {
      const res = await call(server, 'POST', '/v1/workstreams', {
        token: DEVICE,
        json: { project: UNKNOWN.project, name: 'Lost' },
      });
      refused(res, 404, 'unknown project');
    }));

  it('rejects malformed workstreams with 400', () =>
    withServer(async (server) => {
      for (const json of [
        { name: 'No project' },
        { project: ID.paper },
        { project: ID.paper, name: '' },
        { project: ID.paper, name: 'x', status: 'finished' },
        { project: ID.paper, name: 'x', health: 'on_track', status: 'on_track' },
        { project: ID.paper, name: 'x', locations: [{ machine: UNKNOWN.machine, path: '/work' }] },
        { project: ID.paper, name: 'x', locations: { machine: LAPTOP, path: '/work' } },
      ]) {
        refused(await call(server, 'POST', '/v1/workstreams', { token: DEVICE, json }), 400, JSON.stringify(json));
      }
    }));

  it('needs a device token', () =>
    withServer(async (server) => {
      const res = await call(server, 'POST', '/v1/workstreams', { token: AGENT, json: { project: ID.paper, name: 'x' } });
      refused(res, 403, 'agent');
    }));
});

describe('PATCH /v1/tasks/{id-or-key}', () => {
  const patch = (server: RunningServer, ref: string, json: unknown, token = DEVICE) =>
    call<Task>(server, 'PATCH', `/v1/tasks/${ref}`, { token, json });

  it('edits fields and emits task_updated with only the fields that changed', () =>
    withServer(async (server) => {
      const res = await patch(server, 'PAP-2', {
        title: '  Make figure 3  ',
        description: 'Mean and spread over the four good seeds.',
        priority: 'medium',
        labels: [' figures ', 'paper', 'figures'],
        start: '2026-10-05',
        accept_auto: false,
        unknown_field: 'ignored',
      });
      assert.equal(res.status, 200);
      assert.equal(res.body.title, 'Make figure 3');
      assert.equal(res.body.description, 'Mean and spread over the four good seeds.');
      assert.deepEqual(res.body.labels, ['figures', 'paper']);
      assert.equal(res.body.start, '2026-10-05');
      const event = await lastEvent(server);
      assert.equal(event?.author, ID.sam);
      // Priority and accept_auto were already medium and false.
      assert.deepEqual(event?.body, {
        type: 'task_updated',
        data: {
          task: PAP2,
          patch: {
            title: 'Make figure 3',
            description: 'Mean and spread over the four good seeds.',
            labels: ['figures', 'paper'],
            start: '2026-10-05',
          },
        },
      });
      const fetched = await call<Task>(server, 'GET', `/v1/tasks/${PAP2}`, { token: DEVICE });
      assert.deepEqual(fetched.body, res.body);
    }));

  it('accepts the key, the id and the prefixed id', () =>
    withServer(async (server) => {
      for (const [i, ref] of ['PAP-6', '01JB000000000000000TSK0006', 'tsk_01JB000000000000000TSK0006'].entries()) {
        const res = await patch(server, ref, { title: `Aggregate the results, take ${i}` });
        assert.equal(res.status, 200, ref);
        assert.equal(res.body.key, 'PAP-6');
      }
    }));

  it('returns the task and emits nothing when nothing changes', () =>
    withServer(async (server) => {
      const before = await rev(server);
      const empty = await patch(server, 'PAP-1', {});
      assert.equal(empty.status, 200);
      assert.equal(empty.body.title, 'Draft the method section');
      const same = await patch(server, 'PAP-1', {
        title: 'Draft the method section ',
        priority: 'high',
        labels: ['writing'],
        due: '2026-10-10',
        workstream: ID.submission,
        blocked_by: [],
        accept_auto: false,
        title_typo: 'x',
      });
      assert.equal(same.status, 200);
      assert.deepEqual(same.body, empty.body);
      // On the plain fields, null is the same as leaving the field out.
      assert.equal((await patch(server, 'PAP-1', { title: null, labels: null })).status, 200);
      assert.equal(await rev(server), before);
    }));

  it('clears workstream, start and due with null', () =>
    withServer(async (server) => {
      await patch(server, 'PAP-1', { start: '2026-10-01' });
      const res = await patch(server, 'PAP-1', { workstream: null, start: null, due: null });
      assert.equal(res.status, 200);
      for (const field of ['workstream', 'start', 'due'] as const) {
        assert.equal(Object.hasOwn(res.body, field), false, field);
      }
      const event = await lastEvent(server);
      assert.deepEqual(event?.body, {
        type: 'task_updated',
        data: { task: ID.pap1, patch: { workstream: null, start: null, due: null } },
      });
      // Clearing what is already clear changes nothing.
      const before = await rev(server);
      assert.equal((await patch(server, 'PAP-1', { workstream: null, due: null })).status, 200);
      assert.equal(await rev(server), before);
    }));

  it('moves a task to another workstream of its project only', () =>
    withServer(async (server) => {
      const moved = await patch(server, 'PAP-1', { workstream: SEEDS });
      assert.equal(moved.status, 200);
      assert.equal(moved.body.workstream, SEEDS);
      refused(await patch(server, 'PAP-1', { workstream: ID.parsers }), 400, 'another project');
      refused(await patch(server, 'PAP-1', { workstream: UNKNOWN.workstream }), 400, 'unknown workstream');
    }));

  it('checks titles: 1 to 500 characters after trimming', () =>
    withServer(async (server) => {
      refused(await patch(server, 'PAP-1', { title: '   ' }), 400, 'blank');
      refused(await patch(server, 'PAP-1', { title: 'x'.repeat(501) }), 400, '501 characters');
      refused(await patch(server, 'PAP-1', { title: 42 }), 400, 'not a string');
      assert.equal((await patch(server, 'PAP-1', { title: ` ${'x'.repeat(500)} ` })).status, 200);
      // Characters are code points, as in Rust: 500 emoji are 1000 UTF-16 units.
      const emoji = await patch(server, 'PAP-1', { title: '\u{1F9EA}'.repeat(500) });
      assert.equal(emoji.status, 200);
      refused(await patch(server, 'PAP-1', { title: '\u{1F9EA}'.repeat(501) }), 400, '501 emoji');
    }));

  it('checks labels: trimmed, deduplicated, 1 to 64 characters, at most 32', () =>
    withServer(async (server) => {
      const many = (n: number): string[] => Array.from({ length: n }, (_, i) => `label-${i}`);
      const ok = await patch(server, 'PAP-1', { labels: [...many(32), ' label-0 ', 'label-31'] });
      assert.equal(ok.status, 200);
      assert.deepEqual(ok.body.labels, many(32));
      refused(await patch(server, 'PAP-1', { labels: many(33) }), 400, '33 labels');
      refused(await patch(server, 'PAP-1', { labels: ['ok', '  '] }), 400, 'blank label');
      refused(await patch(server, 'PAP-1', { labels: ['y'.repeat(65)] }), 400, '65 characters');
      refused(await patch(server, 'PAP-1', { labels: 'writing' }), 400, 'not an array');
      assert.equal((await patch(server, 'PAP-1', { labels: ['z'.repeat(64)] })).status, 200);
      assert.deepEqual((await patch(server, 'PAP-1', { labels: [] })).body.labels, []);
    }));

  it('checks blocked_by: existing tasks, not itself, no cycle (409)', () =>
    withServer(async (server) => {
      refused(await patch(server, 'PAP-1', { blocked_by: [UNKNOWN.task] }), 400, 'unknown task');
      refused(await patch(server, 'PAP-1', { blocked_by: ['not-an-id'] }), 400, 'malformed id');
      refused(await patch(server, 'PAP-1', { blocked_by: [ID.pap1] }), 400, 'itself');
      // PAP-2 already waits on PAP-4.
      refused(await patch(server, 'PAP-4', { blocked_by: [PAP2] }), 409, 'direct cycle');
      const ok = await patch(server, 'PAP-4', { blocked_by: [ID.pap1, ID.pap1] });
      assert.equal(ok.status, 200);
      assert.deepEqual(ok.body.blocked_by, [ID.pap1], 'duplicates dropped');
      // Now PAP-2 → PAP-4 → PAP-1, so PAP-1 may not wait on PAP-2.
      refused(await patch(server, 'PAP-1', { blocked_by: [PAP2] }), 409, 'cycle through PAP-4');
      assert.equal((await patch(server, 'PAP-1', { blocked_by: ['01JB000000000000000TSK0008'] })).status, 200);
      const event = await lastEvent(server);
      assert.deepEqual(event?.body, {
        type: 'task_updated',
        data: { task: ID.pap1, patch: { blocked_by: ['01JB000000000000000TSK0008'] } },
      });
    }));

  it('checks dates: YYYY-MM-DD, and start not after due', () =>
    withServer(async (server) => {
      refused(await patch(server, 'PAP-1', { start: '2026-10-32' }), 400, 'bad day');
      refused(await patch(server, 'PAP-1', { due: '10/10/2026' }), 400, 'bad format');
      refused(await patch(server, 'PAP-1', { start: '2026-10-02', due: '2026-10-01' }), 400, 'both in the patch');
      // PAP-1 is due 2026-10-10.
      refused(await patch(server, 'PAP-1', { start: '2026-10-11' }), 400, 'after the current due');
      assert.equal((await patch(server, 'PAP-1', { start: '2026-10-10' })).status, 200);
      refused(await patch(server, 'PAP-1', { due: '2026-10-09' }), 400, 'before the current start');
      const cleared = await patch(server, 'PAP-1', { due: null, start: '2026-12-01' });
      assert.equal(cleared.status, 200, 'no due, so any start');
    }));

  it('rejects other malformed patches with 400, changing nothing', () =>
    withServer(async (server) => {
      const before = await rev(server);
      for (const json of [
        { priority: 'critical' },
        { accept_auto: 'yes' },
        { description: 7 },
        { workstream: 12 },
        // A valid title does not land when another field is wrong.
        { title: 'A new title', labels: [''] },
      ]) {
        refused(await patch(server, 'PAP-1', json), 400, JSON.stringify(json));
      }
      refused(await call(server, 'PATCH', '/v1/tasks/PAP-1', { token: DEVICE, json: [] }), 400, 'array body');
      assert.equal(await rev(server), before);
      const task = await call<Task>(server, 'GET', '/v1/tasks/PAP-1', { token: DEVICE });
      assert.equal(task.body.title, 'Draft the method section');
    }));

  it('answers 404 for an unknown task and 403 for agents, even on their own task', () =>
    withServer(async (server) => {
      refused(await patch(server, 'PAP-99', { title: 'x' }), 404, 'unknown');
      // PAP-1 is @writer's own task; editing still needs a person.
      refused(await patch(server, 'PAP-1', { title: 'x' }, AGENT), 403, 'agent');
    }));

  it('lets activity find task_updated by task', () =>
    withServer(async (server) => {
      await patch(server, PAP4, { priority: 'urgent' });
      const res = await call<Activity>(server, 'GET', `/v1/events?task=${PAP4}&limit=1`, { token: DEVICE });
      assert.equal(res.body.events[0]?.body.type, 'task_updated');
    }));
});

describe('PUT /v1/briefs: next steps and accepted proposals', () => {
  const put = (server: RunningServer, path: string, json: unknown) =>
    call<Brief>(server, 'PUT', `/v1/briefs/${path}`, { token: DEVICE, json });

  it('stores next, and brief_accepted carries it', () =>
    withServer(async (server) => {
      const res = await put(server, `workstream/${ID.submission}`, {
        text: '§3.2 is drafted.',
        next: 'Send it to the co-authors.',
        pinned: false,
      });
      assert.equal(res.status, 200);
      assert.equal(res.body.next, 'Send it to the co-authors.');
      assert.equal(res.body.source, 'person');
      assert.deepEqual(res.body.receipts, []);
      const event = await lastEvent(server);
      assert.equal(event?.author, ID.sam);
      assert.deepEqual(event?.body, {
        type: 'brief_accepted',
        data: {
          target: { kind: 'workstream', id: ID.submission },
          text: '§3.2 is drafted.',
          next: 'Send it to the co-authors.',
          pinned: false,
        },
      });
      assert.equal(res.body.updated, event?.at);
      const briefs = await call<Brief[]>(server, 'GET', '/v1/briefs', { token: DEVICE });
      assert.deepEqual(
        briefs.body.find((b) => b.target.id === ID.submission),
        res.body,
      );
    }));

  it('copies the receipts of a pending proposal accepted unchanged, as the back office', () =>
    withServer(async (server) => {
      // Revision 15 proposes a new "Where the project stands" for PAP, with no next step.
      const proposal = await lastEvent(server);
      assert.equal(proposal?.body.type, 'brief_proposed');
      if (proposal?.body.type !== 'brief_proposed') {
        return;
      }
      const { text, receipts } = proposal.body.data;
      const accepted = await put(server, `project/${ID.paper}`, { text, pinned: false });
      assert.equal(accepted.status, 200);
      assert.equal(accepted.body.source, 'back_office');
      assert.deepEqual(accepted.body.receipts, receipts);
      assert.equal(accepted.body.next, undefined);
      const event = await lastEvent(server);
      assert.deepEqual(event?.body, {
        type: 'brief_accepted',
        data: { target: { kind: 'project', id: ID.paper }, text, pinned: false, receipts },
      });
      // The accepted brief is now newer than the proposal: the same PUT again ("keep current")
      // is the person's own.
      const kept = await put(server, `project/${ID.paper}`, { text, pinned: false });
      assert.equal(kept.body.source, 'person');
      assert.deepEqual(kept.body.receipts, []);
      const keptEvent = await lastEvent(server);
      assert.equal(keptEvent?.body.type === 'brief_accepted' && keptEvent.body.data.receipts, undefined);
    }));

  it('treats a changed text or next step as the person\'s own', () =>
    withServer(async (server) => {
      const proposal = await lastEvent(server);
      const text = proposal?.body.type === 'brief_proposed' ? proposal.body.data.text : '';
      const withNext = await put(server, `project/${ID.paper}`, { text, next: 'Decide on seed 3.', pinned: false });
      assert.equal(withNext.body.source, 'person');
      assert.deepEqual(withNext.body.receipts, []);
    }));

  it('treats an edited text as the person\'s own, with no receipts', () =>
    withServer(async (server) => {
      const edited = await put(server, `project/${ID.paper}`, { text: 'Half of the method is drafted.', pinned: true });
      assert.equal(edited.body.source, 'person');
      assert.deepEqual(edited.body.receipts, []);
      // A workstream whose brief in force (revision 12) has no newer proposal.
      const seeds = await put(server, `workstream/${SEEDS}`, { text: 'Seed 3 is rerunning.', pinned: true });
      assert.equal(seeds.body.source, 'person');
    }));

  const briefOf = async (server: RunningServer, id: string): Promise<Brief | undefined> =>
    (await call<Brief[]>(server, 'GET', '/v1/briefs', { token: DEVICE })).body.find((b) => b.target.id === id);

  /** Appends a `brief_proposed` from @office, as the back office would. */
  const propose = (server: RunningServer, target: Brief['target'], text: string, next?: string): Event =>
    server.hub.append(OFFICE, {
      type: 'brief_proposed',
      data: { target, text, ...(next === undefined ? {} : { next }), receipts: [JOB] },
    });

  it('lists each brief with its pending proposal', () =>
    withServer(async (server) => {
      // The fixture's revision 15 proposes for PAP, and no brief_accepted for PAP follows it.
      const proposal = await lastEvent(server);
      assert.equal(proposal?.body.type, 'brief_proposed');
      const paper = await briefOf(server, ID.paper);
      assert.deepEqual(
        paper?.proposal,
        proposal?.body.type === 'brief_proposed'
          ? { text: proposal.body.data.text, receipts: proposal.body.data.receipts, at: proposal.at }
          : undefined,
      );
      // The other briefs have nothing pending: the key is left out.
      const briefs = await call<Brief[]>(server, 'GET', '/v1/briefs', { token: DEVICE });
      for (const brief of briefs.body.filter((b) => b.target.id !== ID.paper)) {
        assert.equal(Object.hasOwn(brief, 'proposal'), false, brief.target.id);
      }
      // A new proposal shows with its next step and time.
      const event = propose(server, { kind: 'workstream', id: SEEDS }, 'Seed 3 reran and converged.', 'Make figure 3.');
      assert.deepEqual((await briefOf(server, SEEDS))?.proposal, {
        text: 'Seed 3 reran and converged.',
        next: 'Make figure 3.',
        receipts: [JOB],
        at: event.at,
      });
    }));

  it('"keep current" (a PUT of the current text) clears the proposal', () =>
    withServer(async (server) => {
      propose(server, { kind: 'workstream', id: SEEDS }, 'Seed 3 reran and converged.', 'Make figure 3.');
      const current = await briefOf(server, SEEDS);
      assert.ok(current?.proposal !== undefined);
      const kept = await put(server, `workstream/${SEEDS}`, {
        text: current.text,
        next: current.next,
        pinned: current.pinned,
      });
      assert.equal(kept.status, 200);
      assert.equal(kept.body.source, 'person');
      assert.equal(Object.hasOwn(kept.body, 'proposal'), false);
      const after = await briefOf(server, SEEDS);
      assert.equal(after?.proposal, undefined);
      assert.equal(after?.text, current.text);
    }));

  it('accepting the proposal unchanged clears it, as the back office', () =>
    withServer(async (server) => {
      propose(server, { kind: 'workstream', id: SEEDS }, 'Seed 3 reran and converged.', 'Make figure 3.');
      const accepted = await put(server, `workstream/${SEEDS}`, {
        text: 'Seed 3 reran and converged.',
        next: 'Make figure 3.',
        pinned: true,
      });
      assert.equal(accepted.body.source, 'back_office');
      assert.deepEqual(accepted.body.receipts, [JOB]);
      const after = await briefOf(server, SEEDS);
      assert.equal(after?.proposal, undefined);
      assert.equal(after?.source, 'back_office');
      assert.deepEqual(after?.receipts, [JOB]);
      // The fixture's own pending proposal for PAP clears the same way.
      const paper = await briefOf(server, ID.paper);
      assert.ok(paper?.proposal !== undefined);
      await put(server, `project/${ID.paper}`, { text: paper.proposal.text, pinned: false });
      assert.equal((await briefOf(server, ID.paper))?.proposal, undefined);
    }));

  it('a newer proposal is pending again after an accepted brief', () =>
    withServer(async (server) => {
      await put(server, `workstream/${SEEDS}`, { text: 'Rerunning seed 3.', pinned: true });
      assert.equal((await briefOf(server, SEEDS))?.proposal, undefined);
      propose(server, { kind: 'workstream', id: SEEDS }, 'Seed 3 converged.');
      assert.equal((await briefOf(server, SEEDS))?.proposal?.text, 'Seed 3 converged.');
      // Proposing for one brief leaves the others alone.
      assert.equal((await briefOf(server, ID.submission))?.proposal, undefined);
    }));

  it('keeps its old checks', () =>
    withServer(async (server) => {
      refused(await put(server, `project/${UNKNOWN.project}`, { text: 'x', pinned: false }), 404, 'unknown project');
      refused(await put(server, `task/${ID.pap1}`, { text: 'x', pinned: false }), 404, 'not a brief target');
      refused(await put(server, `project/${ID.paper}`, { text: 'x' }), 400, 'pinned missing');
      refused(await put(server, `project/${ID.paper}`, { text: 'x', next: 3, pinned: false }), 400, 'next not a string');
      const agent = await call(server, 'PUT', `/v1/briefs/project/${ID.paper}`, {
        token: AGENT,
        json: { text: 'x', pinned: false },
      });
      refused(agent, 403, 'agent');
    }));
});
