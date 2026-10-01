import assert from 'node:assert/strict';
import { request } from 'node:http';
import { describe, it } from 'node:test';
import type { ApiError, Ask, Event, HostInfo, Member, Task } from '../src/types.ts';
import { AGENT, DEVICE, ID, call, withServer } from './helpers.ts';

interface Activity {
  events: Event[];
  from_rev: number;
  to_rev: number;
  at_start: boolean;
}

const keys = (tasks: Task[]): string[] => tasks.map((t) => t.key);

describe('host info and auth', () => {
  it('serves host info without a token', () =>
    withServer(async (server) => {
      const res = await call<HostInfo>(server, 'GET', '/v1/host/info');
      assert.equal(res.status, 200);
      assert.equal(res.body.name, 'pitcrewd');
      assert.ok(res.body.protocol_min <= 1 && 1 <= res.body.protocol);
      assert.deepEqual(res.body.roles, ['hub', 'runner']);
      assert.equal(res.headers.get('x-pitcrew-mock-hub'), res.body.version);
    }));

  it('answers 401 without a token or with an unknown one', () =>
    withServer(async (server) => {
      const none = await call<ApiError>(server, 'GET', '/v1/tasks');
      assert.equal(none.status, 401);
      assert.equal(none.body.code, 'unauthorized');
      assert.equal(none.headers.get('www-authenticate'), 'Bearer');
      const unknown = await call<ApiError>(server, 'GET', '/v1/tasks', { token: 'nope' });
      assert.equal(unknown.status, 401);
      assert.equal(unknown.body.code, 'unauthorized');
    }));

  it('forbids agent tokens on device-only routes, and allows them on agent routes', () =>
    withServer(async (server) => {
      const machines = await call<ApiError>(server, 'GET', '/v1/machines', { token: AGENT });
      assert.equal(machines.status, 403);
      assert.equal(machines.body.code, 'forbidden');
      const create = await call<ApiError>(server, 'POST', '/v1/tasks', {
        token: AGENT,
        json: { project: ID.paper, title: 'Sneaky' },
      });
      assert.equal(create.status, 403);
      const me = await call<Member>(server, 'GET', '/v1/me', { token: AGENT });
      assert.equal(me.status, 200);
      assert.equal(me.body.handle, '@writer');
    }));

  it('answers unknown routes with a 404 ApiError', () =>
    withServer(async (server) => {
      const res = await call<ApiError>(server, 'GET', '/v1/nowhere', { token: DEVICE });
      assert.equal(res.status, 404);
      assert.equal(res.body.code, 'not_found');
      assert.equal(typeof res.body.message, 'string');
    }));

  it('accepts hook events from agents and rejects malformed ones', () =>
    withServer(async (server) => {
      const ok = await call(server, 'POST', '/v1/hooks/claude/Stop', {
        token: AGENT,
        json: { session_id: 'abc', hook_event_name: 'Stop' },
      });
      assert.equal(ok.status, 202);
      const engine = await call<ApiError>(server, 'POST', '/v1/hooks/emacs/Stop', { token: AGENT, json: {} });
      assert.equal(engine.status, 400);
      const event = await call<ApiError>(server, 'POST', '/v1/hooks/codex/%3Bbad', { token: AGENT, json: {} });
      assert.equal(event.status, 400);
      const noToken = await call<ApiError>(server, 'POST', '/v1/hooks/claude/Stop', { json: {} });
      assert.equal(noToken.status, 401);
    }));
});

describe('tasks', () => {
  it('lists tasks and applies every filter', () =>
    withServer(async (server) => {
      const get = async (query: string): Promise<string[]> => {
        const res = await call<Task[]>(server, 'GET', `/v1/tasks${query}`, { token: DEVICE });
        assert.equal(res.status, 200);
        return keys(res.body);
      };
      assert.equal((await get('')).length, 10);
      assert.deepEqual(await get(`?project=${ID.tooling}`), ['TL-1', 'TL-2', 'TL-3']);
      assert.deepEqual(await get(`?workstream=${ID.submission}`), ['PAP-1', 'PAP-2', 'PAP-3', 'PAP-7']);
      assert.deepEqual(await get(`?assignee=${ID.writer}`), ['PAP-1', 'PAP-2', 'PAP-3']);
      assert.deepEqual(await get('?status=todo&status=backlog'), ['PAP-2', 'PAP-5', 'PAP-6', 'TL-2']);
      assert.deepEqual(await get(`?project=${ID.paper}&status=in_progress`), ['PAP-1', 'PAP-4']);
    }));

  it('fills serde defaults the fixture leaves out', () =>
    withServer(async (server) => {
      const res = await call<Task>(server, 'GET', '/v1/tasks/PAP-6', { token: DEVICE });
      assert.equal(res.body.description, '');
      assert.deepEqual(res.body.subtasks, []);
    }));

  it('rejects unknown status values with 400', () =>
    withServer(async (server) => {
      const res = await call<ApiError>(server, 'GET', '/v1/tasks?status=finished', { token: DEVICE });
      assert.equal(res.status, 400);
      assert.equal(res.body.code, 'invalid');
    }));

  it('lets an agent read the whole workspace but write only to its own tasks', () =>
    withServer(async (server) => {
      const list = await call<Task[]>(server, 'GET', '/v1/tasks', { token: AGENT });
      assert.equal(list.body.length, 10);
      const other = await call<Task>(server, 'GET', '/v1/tasks/PAP-4', { token: AGENT });
      assert.equal(other.status, 200);
      assert.equal(other.body.key, 'PAP-4');
      // PAP-4 belongs to @runner: every write by @writer is a 403, a legal-looking move included.
      const writes = [
        call<ApiError>(server, 'POST', '/v1/tasks/PAP-4/move', { token: AGENT, json: { to: 'review' } }),
        call<ApiError>(server, 'POST', '/v1/tasks/PAP-4/comments', {
          token: AGENT,
          json: { text: 'Looks good', mentions: [] },
        }),
        call<ApiError>(server, 'PUT', '/v1/tasks/PAP-4/subtasks', { token: AGENT, json: [] }),
      ];
      for (const res of await Promise.all(writes)) {
        assert.equal(res.status, 403);
        assert.equal(res.body.code, 'forbidden');
      }
      const unchanged = await call<Task>(server, 'GET', '/v1/tasks/PAP-4', { token: DEVICE });
      assert.equal(unchanged.body.status, 'in_progress');
    }));

  it('lets an agent comment on its own task, answering 201 with the event', () =>
    withServer(async (server) => {
      const res = await call<Event>(server, 'POST', '/v1/tasks/PAP-1/comments', {
        token: AGENT,
        json: { text: '§3.2 is drafted.', mentions: [ID.sam] },
      });
      assert.equal(res.status, 201);
      assert.equal(res.body.author, ID.writer);
      assert.equal(res.body.on_behalf_of, ID.sam);
      assert.deepEqual(res.body.body, {
        type: 'comment_posted',
        data: { task: ID.pap1, text: '§3.2 is drafted.', mentions: [ID.sam] },
      });
    }));

  it('gets a task by key, by id and by prefixed id', () =>
    withServer(async (server) => {
      for (const ref of ['PAP-4', ID.pap4, `tsk_${ID.pap4}`]) {
        const res = await call<Task>(server, 'GET', `/v1/tasks/${ref}`, { token: DEVICE });
        assert.equal(res.status, 200, ref);
        assert.equal(res.body.id, ID.pap4);
        assert.equal(res.body.key, 'PAP-4');
      }
      const missing = await call<ApiError>(server, 'GET', '/v1/tasks/PAP-99', { token: DEVICE });
      assert.equal(missing.status, 404);
      assert.equal(missing.body.code, 'not_found');
    }));

  it('lets an agent move its own task forward, stamping author and owner', () =>
    withServer(async (server) => {
      const res = await call<Task>(server, 'POST', '/v1/tasks/PAP-2/move', {
        token: AGENT,
        json: { to: 'in_progress' },
      });
      assert.equal(res.status, 200);
      assert.equal(res.body.status, 'in_progress');
      const activity = await call<Activity>(server, 'GET', '/v1/events?limit=1', { token: DEVICE });
      const [event] = activity.body.events;
      assert.equal(activity.body.to_rev, 16);
      assert.equal(event?.author, ID.writer);
      assert.equal(event?.on_behalf_of, ID.sam);
      assert.deepEqual(event?.body, {
        type: 'task_moved',
        data: {
          task: '01JB000000000000000TSK0002',
          from: 'todo',
          to: 'in_progress',
          mover: { kind: 'agent', on_own_task: true },
        },
      });
    }));

  it('refuses moves the rules do not allow with 409', () =>
    withServer(async (server) => {
      const move = (ref: string, to: string, token: string) =>
        call<ApiError>(server, 'POST', `/v1/tasks/${ref}/move`, { token, json: { to } });
      // Agents never move a task to done, not even their own.
      const done = await move('PAP-1', 'done', AGENT);
      assert.equal(done.status, 409);
      assert.equal(done.body.code, 'conflict');
      assert.equal((await move('PAP-3', 'in_progress', AGENT)).status, 409);
      // Nobody moves a task to the status it already has.
      assert.equal((await move('PAP-4', 'in_progress', DEVICE)).status, 409);
      const task = await call<Task>(server, 'GET', '/v1/tasks/PAP-1', { token: DEVICE });
      assert.equal(task.body.status, 'in_progress');
    }));

  it('lets a person make any move', () =>
    withServer(async (server) => {
      const res = await call<Task>(server, 'POST', '/v1/tasks/PAP-7/move', {
        token: DEVICE,
        json: { to: 'todo' },
      });
      assert.equal(res.status, 200);
      assert.equal(res.body.status, 'todo');
      const activity = await call<Activity>(server, 'GET', '/v1/events?limit=1', { token: DEVICE });
      const [event] = activity.body.events;
      assert.equal(event?.author, ID.sam);
      assert.equal(event?.on_behalf_of, undefined);
      assert.equal(event?.body.type, 'task_moved');
      assert.deepEqual(event?.body.type === 'task_moved' && event.body.data.mover, { kind: 'person' });
    }));

  it('validates move bodies', () =>
    withServer(async (server) => {
      const bad = await call<ApiError>(server, 'POST', '/v1/tasks/PAP-2/move', {
        token: DEVICE,
        json: { to: 'finished' },
      });
      assert.equal(bad.status, 400);
      assert.equal(bad.body.code, 'invalid');
      const empty = await call<ApiError>(server, 'POST', '/v1/tasks/PAP-2/move', { token: DEVICE });
      assert.equal(empty.status, 400);
    }));

  it('creates tasks with the next key in their project', () =>
    withServer(async (server) => {
      const create = (json: unknown) => call<Task>(server, 'POST', '/v1/tasks', { token: DEVICE, json });
      const first = await create({ project: ID.paper, title: 'Write the abstract' });
      assert.equal(first.status, 201);
      assert.equal(first.body.key, 'PAP-8');
      assert.equal(first.body.status, 'todo');
      assert.equal(first.body.priority, 'none');
      assert.equal(first.body.id.length, 26);
      assert.equal((await create({ project: ID.paper, title: 'Check references' })).body.key, 'PAP-9');
      const tooling = await create({
        project: ID.tooling,
        workstream: ID.parsers,
        title: 'Benchmark OpenCode parsing',
        status: 'backlog',
        priority: 'low',
        labels: ['performance'],
        due: '2026-10-31',
      });
      assert.equal(tooling.body.key, 'TL-4');
      assert.equal(tooling.body.workstream, ID.parsers);
      const fetched = await call<Task>(server, 'GET', '/v1/tasks/TL-4', { token: DEVICE });
      assert.deepEqual(fetched.body, tooling.body);
      const activity = await call<Activity>(server, 'GET', '/v1/events?limit=1', { token: DEVICE });
      assert.equal(activity.body.events[0]?.body.type, 'task_created');
    }));

  it('rejects malformed new tasks', () =>
    withServer(async (server) => {
      const create = (json: unknown) =>
        call<ApiError>(server, 'POST', '/v1/tasks', { token: DEVICE, json });
      for (const json of [
        { title: 'No project' },
        { project: ID.paper },
        { project: ID.paper, title: 'x', status: 'finished' },
        { project: ID.paper, title: 'x', due: '2026-13-01' },
        { project: ID.paper, workstream: ID.parsers, title: 'Wrong project' },
        { project: '01JB000000000000000PRJ0099', title: 'Unknown project' },
      ]) {
        const res = await create(json);
        assert.equal(res.status, 400, JSON.stringify(json));
        assert.equal(res.body.code, 'invalid');
      }
    }));
});

describe('asks', () => {
  it('answers an open ask once', () =>
    withServer(async (server) => {
      const open = async (): Promise<string[]> =>
        (
          await call<Ask[]>(server, 'GET', `/v1/asks?to=${ID.sam}&state=open`, { token: DEVICE })
        ).body.map((a) => a.id);
      assert.equal((await open()).length, 3);
      const res = await call<Ask>(server, 'POST', '/v1/asks/01JB000000000000000ASK0002/answer', {
        token: DEVICE,
        json: { option: 1 },
      });
      assert.equal(res.status, 200);
      assert.equal(res.body.state, 'answered');
      assert.equal(res.body.answer?.by, ID.sam);
      assert.equal(res.body.answer?.option, 1);
      assert.equal((await open()).includes('01JB000000000000000ASK0002'), false);
      const again = await call<ApiError>(server, 'POST', '/v1/asks/01JB000000000000000ASK0002/answer', {
        token: DEVICE,
        json: { text: 'Changed my mind' },
      });
      assert.equal(again.status, 409);
      const activity = await call<Activity>(server, 'GET', '/v1/events?limit=1', { token: DEVICE });
      assert.equal(activity.body.events[0]?.body.type, 'ask_answered');
    }));

  it('validates answers', () =>
    withServer(async (server) => {
      const answer = (json: unknown, token = DEVICE) =>
        call<ApiError>(server, 'POST', '/v1/asks/01JB000000000000000ASK0001/answer', { token, json });
      assert.equal((await answer({ option: 5 })).status, 400);
      assert.equal((await answer({})).status, 400);
      // ASK0001 is addressed to @sam, so @writer may not answer it.
      assert.equal((await answer({ option: 0 }, AGENT)).status, 403);
      const missing = await call<ApiError>(server, 'POST', '/v1/asks/01JB000000000000000ASK0099/answer', {
        token: DEVICE,
        json: { option: 0 },
      });
      assert.equal(missing.status, 404);
    }));

  it('lets an agent answer only questions and mentions addressed to itself', () =>
    withServer(async (server) => {
      const raise = async (kind: string): Promise<string> => {
        const res = await call<Ask>(server, 'POST', '/v1/asks', {
          token: DEVICE,
          json: { kind, to: ID.writer, title: `A ${kind} for @writer`, options: ['Yes', 'No'] },
        });
        assert.equal(res.status, 201);
        return res.body.id;
      };
      const answer = (id: string, token: string) =>
        call<Ask>(server, 'POST', `/v1/asks/${id}/answer`, { token, json: { option: 0 } });

      const inbox = await call<Ask[]>(server, 'GET', `/v1/asks?to=${ID.writer}&state=open`, { token: AGENT });
      assert.equal(inbox.status, 200);
      assert.equal(inbox.body.length, 0);

      const question = await raise('question');
      const answered = await answer(question, AGENT);
      assert.equal(answered.status, 200);
      assert.equal(answered.body.answer?.by, ID.writer);

      for (const kind of ['decision', 'approval', 'review']) {
        const id = await raise(kind);
        assert.equal((await answer(id, AGENT)).status, 403, kind);
        // @sam owns @writer, so a device token may answer for it.
        assert.equal((await answer(id, DEVICE)).status, 200, kind);
      }
    }));
});

describe('activity', () => {
  it('pages events by revision, newest last', () =>
    withServer(async (server) => {
      const get = async (query: string): Promise<Activity> =>
        (await call<Activity>(server, 'GET', `/v1/events${query}`, { token: DEVICE })).body;
      const latest = await get('?limit=5');
      assert.equal(latest.from_rev, 11);
      assert.equal(latest.to_rev, 15);
      assert.equal(latest.at_start, false);
      assert.deepEqual(
        latest.events.map((e) => e.id.slice(-4)),
        ['0011', '0012', '0013', '0014', '0015'],
      );
      const older = await get(`?limit=5&before=${latest.from_rev}`);
      assert.deepEqual([older.from_rev, older.to_rev, older.at_start], [6, 10, false]);
      const oldest = await get(`?limit=5&before=${older.from_rev}`);
      assert.deepEqual([oldest.from_rev, oldest.to_rev, oldest.at_start], [1, 5, true]);
      const empty = await get('?before=1');
      assert.deepEqual(empty, { events: [], from_rev: 0, to_rev: 0, at_start: true });
    }));

  it('filters events by what they are about, parents included', () =>
    withServer(async (server) => {
      const res = await call<Activity>(server, 'GET', `/v1/events?task=${ID.pap1}`, { token: DEVICE });
      // Dispatch, move and plan name PAP-1; the file edit names its session, which is linked to it.
      assert.deepEqual(
        res.body.events.map((e) => e.body.type),
        ['dispatch_started', 'task_moved', 'subtasks_replaced', 'file_edited'],
      );
      assert.equal(res.body.from_rev, 4);
      assert.equal(res.body.to_rev, 7);
      assert.equal(res.body.at_start, true);
      // With filters, revisions need not be contiguous; at_start looks at matching events only.
      const newestTwo = await call<Activity>(server, 'GET', `/v1/events?task=${ID.pap1}&limit=2`, {
        token: DEVICE,
      });
      assert.deepEqual([newestTwo.body.from_rev, newestTwo.body.to_rev, newestTwo.body.at_start], [6, 7, false]);
      const tooling = await call<Activity>(server, 'GET', `/v1/events?project=${ID.tooling}`, {
        token: DEVICE,
      });
      assert.deepEqual(
        tooling.body.events.map((e) => e.id.slice(-4)),
        ['0008', '0013', '0014'],
      );
    }));

  it('scans a bounded window per filtered request, and pages on across a gap', () =>
    withServer(
      async (server) => {
        const get = async (query: string): Promise<Activity> =>
          (await call<Activity>(server, 'GET', `/v1/events${query}`, { token: DEVICE })).body;
        // PAP-1's events are revisions 4–7; revisions 8–15 are a gap wider than the window of 5.
        const first = await get(`?task=${ID.pap1}`);
        assert.deepEqual(first, { events: [], from_rev: 11, to_rev: 0, at_start: false });
        const second = await get(`?task=${ID.pap1}&before=${first.from_rev}`);
        assert.deepEqual([second.from_rev, second.to_rev, second.at_start], [6, 7, false]);
        const third = await get(`?task=${ID.pap1}&before=${second.from_rev}`);
        assert.deepEqual([third.from_rev, third.to_rev, third.at_start], [4, 5, true]);

        // Paging until at_start finds every match, and only the page that reached revision 1 is
        // at the start.
        const seen: string[] = [];
        let page = await get(`?task=${ID.pap1}&limit=1`);
        const pages = [page];
        while (!page.at_start) {
          page = await get(`?task=${ID.pap1}&limit=1&before=${page.from_rev}`);
          pages.push(page);
        }
        for (const p of pages) {
          seen.unshift(...p.events.map((e) => e.id.slice(-4)));
        }
        assert.deepEqual(seen, ['0004', '0005', '0006', '0007']);
        assert.deepEqual(
          pages.map((p) => p.at_start),
          [...pages.slice(1).map(() => false), true],
        );

        // A filter that matches nothing: empty pages until the scan reaches revision 1.
        const none = '01JB000000000000000TSK0099';
        assert.deepEqual(await get(`?task=${none}`), { events: [], from_rev: 11, to_rev: 0, at_start: false });
        assert.deepEqual(await get(`?task=${none}&before=11`), { events: [], from_rev: 6, to_rev: 0, at_start: false });
        assert.deepEqual(await get(`?task=${none}&before=6`), { events: [], from_rev: 0, to_rev: 0, at_start: true });

        // Without filters the window does not apply.
        const all = await get('?limit=10');
        assert.deepEqual([all.events.length, all.from_rev, all.to_rev, all.at_start], [10, 6, 15, false]);
      },
      { scanWindow: 5 },
    ));
});

describe('http plumbing', () => {
  it('answers CORS preflights for local origins only', () =>
    withServer(async (server) => {
      const preflight = (origin: string) =>
        call<ApiError>(server, 'OPTIONS', '/v1/tasks', {
          headers: {
            origin,
            'access-control-request-method': 'GET',
            'access-control-request-headers': 'authorization',
          },
        });
      for (const origin of [
        'http://localhost:5173',
        'http://127.0.0.1:1420',
        'tauri://localhost',
        'http://tauri.localhost',
        'https://tauri.localhost',
      ]) {
        const ok = await preflight(origin);
        assert.equal(ok.status, 204, origin);
        assert.equal(ok.headers.get('access-control-allow-origin'), origin);
        assert.match(ok.headers.get('access-control-allow-headers') ?? '', /authorization/i);
      }
      for (const origin of ['https://evil.example', 'http://localhost.evil.example', 'https://localhost:5173']) {
        const refused = await preflight(origin);
        assert.equal(refused.status, 403, origin);
        assert.equal(refused.headers.get('access-control-allow-origin'), null);
      }
    }));

  it('rejects malformed JSON and bodies over 1 MiB', () =>
    withServer(async (server) => {
      const malformed = await call<ApiError>(server, 'POST', '/v1/tasks', { token: DEVICE, raw: '{"title":' });
      assert.equal(malformed.status, 400);
      const huge = await call<ApiError>(server, 'POST', '/v1/tasks', {
        token: DEVICE,
        json: { project: ID.paper, title: 'x'.repeat(1024 * 1024) },
      });
      assert.equal(huge.status, 400);
      assert.match(huge.body.message, /larger than/);
    }));

  it('refuses requests addressed to other host names', () =>
    withServer(async (server) => {
      const status = await new Promise<number | undefined>((resolve, reject) => {
        const req = request(`${server.url}/v1/host/info`, { headers: { host: 'rebind.example' } }, (res) => {
          res.resume();
          resolve(res.statusCode);
        });
        req.on('error', reject);
        req.end();
      });
      assert.equal(status, 403);
    }));
});
