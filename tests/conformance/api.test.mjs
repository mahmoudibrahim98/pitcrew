import assert from 'node:assert/strict';
import { before, test } from 'node:test';
import { readFile } from 'node:fs/promises';
import { request as httpRequest } from 'node:http';
import { randomBytes } from 'node:crypto';
import { schemas, list } from './schema.mjs';

const base = process.env.PITCREW_CONFORMANCE_URL;
const person = process.env.PITCREW_CONFORMANCE_PERSON;
const agent = process.env.PITCREW_CONFORMANCE_AGENT;
assert.ok(base && person && agent, 'Set PITCREW_CONFORMANCE_URL, _PERSON and _AGENT');
assert.ok(
  ['127.0.0.1', 'localhost', '[::1]'].includes(new URL(base).hostname),
  'Conformance changes synthetic local state only',
);
const expected = process.env.PITCREW_CONFORMANCE_EXPECTED
  ? JSON.parse(await readFile(process.env.PITCREW_CONFORMANCE_EXPECTED, 'utf8'))
  : {};
const missing = '01J00000000000000000000000';
const statuses = {
  400: 'invalid',
  401: 'unauthorized',
  403: 'forbidden',
  404: 'not_found',
  409: 'conflict',
  413: 'too_large',
  501: 'unsupported',
  500: 'internal',
  503: 'unavailable',
};
class StatusMismatch extends Error {
  constructor(actual, wanted) {
    super(`HTTP ${actual}, expected ${wanted}`);
    this.actual = actual;
    this.wanted = wanted;
  }
}
async function raw(path, { method = 'GET', body, token = person, headers = {} } = {}) {
  const response = await fetch(new URL(path, base), {
    method,
    headers: {
      ...(token ? { Authorization: `Bearer ${token}` } : {}),
      ...(body !== undefined ? { 'Content-Type': 'application/json' } : {}),
      ...headers,
    },
    body: body === undefined ? undefined : JSON.stringify(body),
    signal: AbortSignal.timeout(5000),
  });
  const text = await response.text();
  let data;
  try {
    data = text ? JSON.parse(text) : undefined;
  } catch {
    data = text;
  }
  return { status: response.status, data };
}
async function api(path, status = 200, schema, options = {}) {
  const response = await raw(path, options);
  if (response.status !== status) {
    if (response.status >= 400) {
      schemas.error(response.data);
      assert.equal(response.data.code, statuses[response.status]);
    }
    throw new StatusMismatch(response.status, status);
  }
  if (status >= 400) {
    schemas.error(response.data);
    assert.equal(response.data.code, statuses[status]);
    assert.ok(response.data.message.length);
  } else if (schema) schema(response.data);
  return response.data;
}
function check(name, fn) {
  test(name, { timeout: 15000 }, async (t) => {
    const deviation = expected[name];
    if (!deviation) return fn(t);
    try {
      await fn(t);
    } catch (error) {
      assert.ok(
        error instanceof StatusMismatch,
        `Expected status deviation ${deviation.row}, received ${error.name}`,
      );
      assert.equal(error.actual, deviation.actual);
      assert.equal(error.wanted, deviation.wanted);
      t.diagnostic(`Expected daemon deviation: ${deviation.row} in MISMATCHES.md`);
      return;
    }
    assert.fail(`Deviation ${deviation.row} now passes: remove its expected-failure entry`);
  });
}

check('task archival is reversible, typed, authorized and event based', async () => {
  const tasks = await api('/v1/tasks', 200, list(schemas.task));
  const original = tasks.find((task) => task.key === 'PAP-1');
  assert.ok(original);
  const path = `/v1/tasks/${original.id}`;
  const archived = await api(path, 200, schemas.task, { method: 'PATCH', body: { archived: true } });
  assert.equal(archived.archived, true);
  assert.deepEqual(archived.subtasks, original.subtasks);
  const events = await api(`/v1/events?task=${original.id}`, 200, schemas.events);
  assert.ok(events.events.some((event) => event.body.type === 'task_updated' && event.body.data.patch.archived === true));
  await api(path, 403, undefined, { method: 'PATCH', token: agent, body: { archived: false } });
  await api(path, 400, undefined, { method: 'PATCH', body: { archived: 'yes' } });
  const restored = await api(path, 200, schemas.task, { method: 'PATCH', body: { archived: false } });
  assert.equal(restored.archived, false);
  const { archived: ignoredBefore, ...before } = original;
  const { archived: ignoredAfter, ...after } = restored;
  assert.deepEqual(after, before);
});

check('task dispatch reads resolve keys and ids for both token scopes', async () => {
  const task = await api('/v1/tasks/PAP-1', 200, schemas.task);
  const runs = await api('/v1/tasks/PAP-1/dispatches', 200, list(schemas.dispatch));
  assert.ok(runs.every((run) => run.task === task.id));
  assert.deepEqual(await api(`/v1/tasks/${task.id}/dispatches`, 200, list(schemas.dispatch), { token: agent }), runs);
  await api('/v1/tasks/PAP-99999/dispatches', 404);
  await api('/v1/tasks/PAP-1/dispatches', 401, undefined, { token: '' });
});
let context = {};
check('per-person read cursors are forward-only and person-only', async () => {
  const second = process.env.PITCREW_CONFORMANCE_SECOND_PERSON;
  assert.ok(second, 'Runner must provide a second synthetic device credential');
  const projects = await api('/v1/projects');
  const streams = await api('/v1/workstreams');
  const checkCursor = (v) => {
    assert.equal(typeof v.scope, 'string');
    assert.ok(Number.isSafeInteger(v.rev) && v.rev >= 0);
  };
  for (const scope of ['workspace', `project:${projects[0].id}`, `workstream:${streams[0].id}`]) {
    const path = `/v1/me/cursors/${encodeURIComponent(scope)}`;
    const moved = await api(path, 200, checkCursor, { method: 'PUT', body: { rev: 10 } });
    for (const rev of [2, 10, 0]) {
      assert.deepEqual(await api(path, 200, checkCursor, { method: 'PUT', body: { rev } }), moved);
    }
    assert.deepEqual(await api(path, 200, checkCursor, { method: 'PUT', token: second, body: { rev: 3 } }), { scope, rev: 3 });
    await api(path, 403, undefined, { method: 'PUT', token: agent, body: { rev: 'bad' } });
  }
  const mine = await api('/v1/me/cursors', 200, list(checkCursor));
  const theirs = await api('/v1/me/cursors', 200, list(checkCursor), { token: second });
  assert.equal(mine.length, 3);
  assert.ok(mine.every((c) => c.rev === 10));
  assert.equal(theirs.length, 3);
  assert.ok(theirs.every((c) => c.rev === 3));
  await api('/v1/me/cursors', 403, undefined, { token: agent });
  for (const rev of [-1, 1.5, '10', Number.MAX_SAFE_INTEGER]) {
    await api('/v1/me/cursors/workspace', 400, undefined, { method: 'PUT', body: { rev } });
  }
  await api('/v1/me/cursors/task:bad', 400, undefined, { method: 'PUT', body: { rev: 1 } });
  await api(`/v1/me/cursors/project:${missing}`, 404, undefined, { method: 'PUT', body: { rev: 1 } });
});
before(async () => {
  const [me, agentMe, machines, members, projects, streams, sessions] = await Promise.all([
    api('/v1/me', 200, schemas.member),
    api('/v1/me', 200, schemas.member, { token: agent }),
    api('/v1/machines', 200, list(schemas.machine)),
    api('/v1/members', 200, list(schemas.member)),
    api('/v1/projects', 200, list(schemas.project)),
    api('/v1/workstreams', 200, list(schemas.workstream)),
    api('/v1/sessions', 200, list(schemas.session)),
  ]);
  assert.ok(projects.length && streams.length && sessions.length, 'Use a fresh seeded demo server');
  const local = machines.find((m) => m.kind === 'local');
  const remoteSession = sessions.find((s) => s.machine !== local.id);
  context = { me, agentMe, machines, members, projects, streams, sessions, local, remoteSession };
});
check('host info without auth', async () => {
  const h = await api('/v1/host/info', 200, schemas.host, { token: null });
  assert.equal(h.name, 'pitcrewd');
  assert.ok(h.capabilities.includes('scan'));
  assert.ok(h.protocol_min <= h.protocol);
});
const personRoutes = [
  '/v1/workspace',
  '/v1/machines',
  '/v1/personas',
  '/v1/teams',
  '/v1/projects',
  '/v1/workstreams',
  '/v1/sessions',
  '/v1/briefs',
  '/v1/events',
  '/v1/recaps/blocks',
  '/v1/recaps/days?project=' + missing,
];
for (const path of [...personRoutes, '/v1/me', '/v1/members', '/v1/tasks', '/v1/asks']) {
  check(`no auth ${path}`, () => api(path, 401, undefined, { token: null }));
  check(`unknown token ${path}`, () =>
    api(path, 401, undefined, { token: 'synthetic-invalid-token' }),
  );
  check(`query token ${path}`, () =>
    api(path + (path.includes('?') ? '&' : '?') + 'token=synthetic-invalid-token', 401, undefined, {
      token: null,
    }),
  );
}
for (const path of personRoutes)
  check(`agent forbidden ${path}`, () => api(path, 403, undefined, { token: agent }));
for (const [path, schema] of [
  ['workspace', schemas.workspace],
  ['me', schemas.member],
  ['machines', list(schemas.machine)],
  ['members', list(schemas.member)],
  ['personas', schemas.personas],
  ['teams', schemas.teams],
  ['projects', list(schemas.project)],
  ['workstreams', list(schemas.workstream)],
  ['tasks', list(schemas.task)],
  ['sessions', list(schemas.session)],
  ['asks', list(schemas.ask)],
  ['briefs', list(schemas.brief)],
  ['events', schemas.events],
  ['recaps/blocks', schemas.blocks],
])
  check(`GET shape /v1/${path}`, () => api(`/v1/${path}`, 200, schema));
check('setup seeded conflict', () =>
  api('/v1/setup', 409, undefined, {
    method: 'POST',
    body: {
      workspace_name: 'Synthetic lab',
      person: { name: 'Sam', handle: '@sam' },
      machine_name: 'Local',
    },
  }),
);
check('setup agent forbidden', () =>
  api('/v1/setup', 403, undefined, { method: 'POST', body: {}, token: agent }),
);
check('unknown route error', () => api('/v1/synthetic-missing', 404));
check('unknown method error', () => api('/v1/projects', 404, undefined, { method: 'DELETE' }));
for (const entity of ['projects', 'workstreams', 'tasks', 'sessions']) {
  check(`unknown ${entity} path`, () => api(`/v1/${entity}/${missing}`, 404));
  check(`ambiguous malformed ${entity} path`, async (t) => {
    const r = await raw(`/v1/${entity}/malformed`);
    assert.ok([400, 404].includes(r.status));
    schemas.error(r.data);
    assert.equal(r.data.code, statuses[r.status]);
    t.diagnostic(`Contract does not specify malformed path ids: observed ${r.status}`);
  });
}
check('project create/defaults/detail/duplicate', async () => {
  const p = await api('/v1/projects', 201, schemas.project, {
    method: 'POST',
    body: { key: 'CONF', name: 'Conformance' },
  });
  assert.equal(p.lead, context.me.id);
  assert.deepEqual(p.members, [context.me.id]);
  assert.equal(p.status, 'in_progress');
  assert.deepEqual(p.external, []);
  assert.deepEqual(await api(`/v1/projects/${p.id}`, 200, schemas.project), p);
  context.project = p;
  await api('/v1/projects', 409, undefined, {
    method: 'POST',
    body: { key: 'CONF', name: 'Duplicate' },
  });
});
for (const [name, body] of [
  ['blank name', { key: 'BAD', name: '' }],
  ['bad key', { key: 'bad', name: 'Synthetic' }],
  ['unknown lead', { key: 'BAD', name: 'Synthetic', lead: missing }],
  ['date order', { key: 'BAD', name: 'Synthetic', start: '2026-12-01', due: '2026-01-01' }],
])
  check(`project invalid ${name}`, () =>
    api('/v1/projects', 400, undefined, { method: 'POST', body }),
  );
check('workstream create/defaults/detail/filter/patch', async () => {
  const w = await api('/v1/workstreams', 201, schemas.workstream, {
    method: 'POST',
    body: { project: context.project.id, name: 'Synthetic stream' },
  });
  context.workstream = w;
  assert.equal(w.status, 'active');
  assert.equal(w.health, 'on_track');
  assert.deepEqual(w.locations, []);
  assert.deepEqual(await api(`/v1/workstreams/${w.id}`, 200, schemas.workstream), w);
  assert.ok(
    (
      await api(`/v1/workstreams?project=${context.project.id}`, 200, list(schemas.workstream))
    ).every((x) => x.project === context.project.id),
  );
  const changed = await api(`/v1/workstreams/${w.id}`, 200, schemas.workstream, {
    method: 'PATCH',
    body: { status: 'paused', health: 'at_risk' },
  });
  assert.equal(changed.status, 'paused');
  assert.equal(changed.health, 'at_risk');
});
check('workstream unknown project exception', () =>
  api('/v1/workstreams', 404, undefined, {
    method: 'POST',
    body: { project: missing, name: 'Synthetic' },
  }),
);
check('workstream bad status', () =>
  api(`/v1/workstreams/${context.workstream.id}`, 400, undefined, {
    method: 'PATCH',
    body: { status: 'bad' },
  }),
);
check('task create/defaults/id/key/prefix', async () => {
  const task = await api('/v1/tasks', 201, schemas.task, {
    method: 'POST',
    body: {
      project: context.project.id,
      workstream: context.workstream.id,
      title: 'Synthetic task',
    },
  });
  context.task = task;
  assert.equal(task.key, 'CONF-1');
  assert.equal(task.status, 'todo');
  assert.equal(task.priority, 'none');
  assert.deepEqual(task.labels, []);
  for (const key of [task.id, task.key, `tsk_${task.id}`])
    assert.equal((await api(`/v1/tasks/${key}`, 200, schemas.task)).id, task.id);
});
check('task filter combinations and repeated status', async () => {
  const all = await api('/v1/tasks', 200, list(schemas.task));
  const filtered = await api(
    `/v1/tasks?project=${context.project.id}&workstream=${context.workstream.id}&status=todo&status=review`,
    200,
    list(schemas.task),
  );
  assert.deepEqual(
    filtered.map((t) => t.id).sort(),
    all
      .filter(
        (t) =>
          t.project === context.project.id &&
          t.workstream === context.workstream.id &&
          ['todo', 'review'].includes(t.status),
      )
      .map((t) => t.id)
      .sort(),
  );
  const assigned = await api(`/v1/tasks?assignee=${context.agentMe.id}`, 200, list(schemas.task));
  assert.ok(assigned.every((t) => t.assignee === context.agentMe.id));
});
check('task unknown project in body', () =>
  api('/v1/tasks', 400, undefined, {
    method: 'POST',
    body: { project: missing, title: 'Synthetic' },
  }),
);
check('task patch trim/dedupe/clear/no-op', async () => {
  const changed = await api(`/v1/tasks/${context.task.id}`, 200, schemas.task, {
    method: 'PATCH',
    body: { title: '  Trimmed  ', labels: [' x ', 'x', 'y'], due: '2026-12-01' },
  });
  assert.equal(changed.title, 'Trimmed');
  assert.deepEqual(changed.labels, ['x', 'y']);
  const cleared = await api(`/v1/tasks/${context.task.id}`, 200, schemas.task, {
    method: 'PATCH',
    body: { due: null },
  });
  assert.ok(!Object.hasOwn(cleared, 'due'));
  const rev = (await api('/v1/workspace', 200, schemas.workspace)).rev;
  await api(`/v1/tasks/${context.task.id}`, 200, schemas.task, { method: 'PATCH', body: {} });
  assert.equal((await api('/v1/workspace', 200, schemas.workspace)).rev, rev);
});
for (const [name, patch] of [
  ['title', { title: '' }],
  ['self dependency', null],
  ['unknown workstream', { workstream: missing }],
  ['priority', { priority: 'bad' }],
  ['date', { due: 'invalid' }],
])
  check(`task patch invalid ${name}`, () =>
    api(`/v1/tasks/${context.task.id}`, 400, undefined, {
      method: 'PATCH',
      body: patch ?? { blocked_by: [context.task.id] },
    }),
  );
check('task assign/null/move/conflict', async () => {
  const assigned = await api(`/v1/tasks/${context.task.id}/assign`, 200, schemas.task, {
    method: 'POST',
    body: { assignee: context.agentMe.id },
  });
  assert.equal(assigned.assignee, context.agentMe.id);
  const moved = await api(`/v1/tasks/${context.task.id}/move`, 200, schemas.task, {
    method: 'POST',
    body: { to: 'in_progress' },
    token: agent,
  });
  assert.equal(moved.status, 'in_progress');
  await api(`/v1/tasks/${context.task.id}/move`, 409, undefined, {
    method: 'POST',
    body: { to: 'in_progress' },
    token: agent,
  });
  await api(`/v1/tasks/${context.task.id}/move`, 409, undefined, {
    method: 'POST',
    body: { to: 'done' },
    token: agent,
  });
  const cleared = await api(`/v1/tasks/${context.task.id}/assign`, 200, schemas.task, {
    method: 'POST',
    body: { assignee: null },
  });
  assert.ok(!Object.hasOwn(cleared, 'assignee'));
});
check('agent cannot write unowned task', () =>
  api(`/v1/tasks/${context.task.id}/move`, 403, undefined, {
    method: 'POST',
    body: { to: 'review' },
    token: agent,
  }),
);
check('subtasks replacement', async () => {
  const subtasks = [
    { id: missing, text: 'Synthetic checklist', done: false, source: { kind: 'human' } },
  ];
  const task = await api(`/v1/tasks/${context.task.id}/subtasks`, 200, schemas.task, {
    method: 'PUT',
    body: subtasks,
  });
  assert.deepEqual(task.subtasks, subtasks);
});
check('comment author cannot be forged', async () => {
  const event = await api(`/v1/tasks/${context.task.id}/comments`, 201, schemas.event, {
    method: 'POST',
    body: { text: 'Synthetic comment', mentions: [], author: missing, on_behalf_of: missing },
  });
  assert.equal(event.author, context.me.id);
  assert.ok(!Object.hasOwn(event, 'on_behalf_of'));
  assert.equal(event.body.type, 'comment_posted');
  assert.equal(event.body.data.task, context.task.id);
});
check('dispatch success', async () => {
  const d = await api(`/v1/tasks/${context.task.id}/dispatch`, 202, schemas.dispatch, {
    method: 'POST',
    body: { agent: context.agentMe.id },
  });
  assert.equal(d.task, context.task.id);
});
check('dispatch completed conflict', async () => {
  await api(`/v1/tasks/${context.task.id}/move`, 200, schemas.task, {
    method: 'POST',
    body: { to: 'done' },
  });
  await api(`/v1/tasks/${context.task.id}/dispatch`, 409, undefined, {
    method: 'POST',
    body: { agent: context.agentMe.id },
  });
});
check('session details/filters', async () => {
  const session = context.sessions[0];
  assert.equal((await api(`/v1/sessions/${session.id}`, 200, schemas.session)).id, session.id);
  const all = await api('/v1/sessions', 200, list(schemas.session));
  for (const [key, value] of [
    ['machine', session.machine],
    ['state', session.state],
    ['task', missing],
    ['workstream', missing],
  ]) {
    const got = await api(`/v1/sessions?${key}=${value}`, 200, list(schemas.session));
    assert.deepEqual(
      got.map((s) => s.id).sort(),
      all
        .filter((s) => s[key] === value)
        .map((s) => s.id)
        .sort(),
    );
  }
});
check('session start invalid engine', () =>
  api('/v1/sessions', 400, undefined, {
    method: 'POST',
    body: { machine: context.local.id, engine: 'invalid', cwd: '/synthetic/missing' },
  }),
);
for (const [verb, body] of [
  ['send', { text: 'Synthetic' }],
  ['keys', { keys: ['enter'] }],
  ['interrupt', undefined],
  ['end', { mode: 'graceful' }],
])
  check(`session ${verb} unknown`, () =>
    api(`/v1/sessions/${missing}/${verb}`, 404, undefined, { method: 'POST', body }),
  );
check('session link validates references', async () => {
  const path = `/v1/sessions/${context.sessions[0].id}/link`;
  for (const body of [{}, { workstream: missing }, { task: missing },
    { workstream: context.streams[0].id, task: context.task.id }]) {
    await api(path, 400, undefined, { method: 'POST', body });
  }
  await api(`/v1/sessions/${missing}/link`, 404, undefined, {
    method: 'POST', body: { workstream: context.workstream.id },
  });
});
check('session link rejects a task without membership in the named workstream', async () => {
  const task = await api('/v1/tasks', 201, schemas.task, {
    method: 'POST', body: { project: context.project.id, title: 'Synthetic task without a workstream' },
  });
  const path = `/v1/sessions/${context.sessions[0].id}/link`;
  await api(path, 400, undefined, {
    method: 'POST', body: { workstream: context.workstream.id, task: task.id },
  });
  const linked = await api(path, 200, schemas.session, { method: 'POST', body: { task: task.id } });
  assert.equal(linked.task, task.id);
  assert.ok(!Object.hasOwn(linked, 'workstream'));
});
check('session link task derives workstream and emits an event', async () => {
  const id = context.sessions[0].id;
  const s = await api(`/v1/sessions/${id}/link`, 200, schemas.session, {
    method: 'POST', body: { task: context.task.id },
  });
  assert.equal(s.task, context.task.id);
  assert.equal(s.workstream, context.workstream.id);
  assert.equal(s.link_basis, 'manual');
  const page = await api(`/v1/events?session=${id}`, 200, schemas.events);
  const event = page.events.find((e) => e.body.type === 'session_linked' && e.body.data.task === context.task.id);
  assert.ok(event);
  assert.equal(event.body.data.basis, 'manual');
  assert.equal(event.author, context.me.id);
});
check('session link manual', async () => {
  const s = await api(`/v1/sessions/${context.sessions[0].id}/link`, 200, schemas.session, {
    method: 'POST',
    body: { workstream: context.workstream.id },
  });
  assert.equal(s.workstream, context.workstream.id);
  assert.ok(!Object.hasOwn(s, 'task'));
  assert.equal(s.link_basis, 'manual');
});
check('transcript page and paging', async () => {
  const path = `/v1/sessions/${context.sessions[0].id}/transcript`;
  let page = await api(path + '?limit=1', 200, schemas.transcript);
  let before = Infinity;
  for (let n = 0; !page.at_start; n++) {
    assert.ok(n < 100);
    assert.ok(page.from < before);
    before = page.from;
    page = await api(path + `?limit=1&before=${before}`, 200, schemas.transcript);
  }
  await api(path + '?limit=0', 400);
  await api(path + '?before=bad', 400);
});
check('ask create/filter/answer', async () => {
  const ask = await api('/v1/asks', 201, schemas.ask, {
    method: 'POST',
    token: agent,
    body: {
      kind: 'question',
      to: context.agentMe.id,
      title: 'Synthetic question',
      options: ['Yes', 'No'],
    },
  });
  context.ask = ask;
  assert.equal(ask.from, context.agentMe.id);
  const filtered = await api(
    `/v1/asks?to=${context.agentMe.id}&state=open`,
    200,
    list(schemas.ask),
  );
  assert.ok(filtered.some((x) => x.id === ask.id));
  assert.ok(filtered.every((x) => x.to === context.agentMe.id && x.state === 'open'));
  const answered = await api(`/v1/asks/${ask.id}/answer`, 200, schemas.ask, {
    method: 'POST',
    token: agent,
    body: { option: 0, text: 'Yes' },
  });
  assert.equal(answered.state, 'answered');
  assert.equal(answered.answer.by, context.agentMe.id);
});
check('agent cannot answer decisions', async () => {
  const ask = await api('/v1/asks', 201, schemas.ask, {
    method: 'POST',
    body: { kind: 'decision', to: context.agentMe.id, title: 'Synthetic decision' },
  });
  await api(`/v1/asks/${ask.id}/answer`, 403, undefined, {
    method: 'POST',
    token: agent,
    body: { text: 'Synthetic' },
  });
});
check('brief edit/read', async () => {
  const brief = await api(`/v1/briefs/project/${context.project.id}`, 200, schemas.brief, {
    method: 'PUT',
    body: { text: 'Synthetic brief', next: 'Synthetic next step', pinned: true },
  });
  assert.equal(brief.source, 'person');
  assert.equal(brief.next, 'Synthetic next step');
  assert.equal(brief.pinned, true);
  assert.ok(
    (await api('/v1/briefs', 200, list(schemas.brief))).some(
      (x) => x.target.id === context.project.id,
    ),
  );
});
check('events backwards pagination/revision bounds', async () => {
  // Compare the complete seed + edit history, beyond the default 100-event page.
  const newest = await api('/v1/events?limit=500', 200, schemas.events);
  assert.equal(newest.at_start, true);
  let page = await api('/v1/events?limit=1', 200, schemas.events);
  const ids = [];
  let before = Infinity;
  for (let n = 0; ; n++) {
    assert.ok(n < 200);
    ids.unshift(...page.events.map((e) => e.id));
    if (page.events.length) {
      assert.ok(page.from_rev <= page.to_rev);
      assert.ok(page.to_rev < before);
    }
    if (page.at_start) break;
    assert.ok(page.from_rev < before);
    before = page.from_rev;
    page = await api(`/v1/events?limit=1&before=${before}`, 200, schemas.events);
  }
  assert.deepEqual(
    ids,
    newest.events.map((e) => e.id),
  );
  assert.equal(new Set(ids).size, ids.length);
});
for (const path of ['/v1/events', '/v1/recaps/blocks']) {
  check(`zero limit ${path}`, () => api(path + '?limit=0', 400));
  check(`bad cursor ${path}`, () => api(path + '?before=bad', 400));
}
check('events filters combine', async () => {
  const events = await api(
    `/v1/events?project=${context.project.id}&task=${context.task.id}`,
    200,
    schemas.events,
  );
  assert.ok(events.events.length);
  assert.ok(
    events.events.some(
      (e) => e.body.type === 'task_created' && e.body.data.task.id === context.task.id,
    ),
  );
  const empty = await api(
    `/v1/events?project=${missing}&task=${context.task.id}`,
    200,
    schemas.events,
  );
  assert.deepEqual(empty.events, []);
  assert.equal(empty.at_start, true);
});
check('recap blocks paging/filter combinations', async () => {
  const whole = await api('/v1/recaps/blocks', 200, schemas.blocks);
  const ids = [];
  let before;
  let page;
  for (let n = 0; ; n++) {
    assert.ok(n < 100);
    page = await api(
      '/v1/recaps/blocks?limit=1' + (before ? `&before=${before}` : ''),
      200,
      schemas.blocks,
    );
    ids.push(...page.blocks.map((b) => b.block.id));
    if (page.at_start) break;
    assert.ok(page.blocks.length);
    before = page.blocks.at(-1).block.id;
  }
  assert.deepEqual(
    ids,
    whole.blocks.map((b) => b.block.id),
  );
  for (const [key, value] of [
    ['project', context.projects[0].id],
    ['session', context.sessions[0].id],
    ['workstream', context.streams[0].id],
    ['task', missing],
  ]) {
    const got = await api(`/v1/recaps/blocks?${key}=${value}`, 200, schemas.blocks);
    assert.deepEqual(
      got.blocks.map((b) => b.block.id),
      whole.blocks
        .filter((b) => (key === 'task' ? b.block.tasks.includes(value) : b.block[key] === value))
        .map((b) => b.block.id),
    );
  }
  const empty = await api(
    `/v1/recaps/blocks?project=${missing}&session=${context.sessions[0].id}`,
    200,
    schemas.blocks,
  );
  assert.deepEqual(empty.blocks, []);
  assert.equal(empty.at_start, true);
});
check('recap days scope/paging', async () => {
  for (const scope of [
    `project=${context.projects[0].id}`,
    `workstream=${context.streams[0].id}`,
  ]) {
    let page = await api(`/v1/recaps/days?${scope}&limit=1`, 200, schemas.days);
    let before = '9999-12-31';
    for (let n = 0; ; n++) {
      assert.ok(n < 100);
      assert.ok(page.days.every((d) => d.date < before));
      if (page.at_start) break;
      assert.ok(page.days.length);
      before = page.days.at(-1).date;
      page = await api(`/v1/recaps/days?${scope}&limit=1&before=${before}`, 200, schemas.days);
    }
  }
});
for (const query of [
  '',
  `project=${missing}&workstream=${missing}`,
  `project=${missing}&limit=0`,
  `project=${missing}&before=bad`,
  `project=${missing}&tz=841`,
  `project=${missing}&tz=1.5`,
])
  check(`recap days invalid ${query}`, () => api('/v1/recaps/days?' + query, 400));
check('hooks accepted', () =>
  api('/v1/hooks/claude/SessionStart', 202, undefined, {
    method: 'POST',
    token: agent,
    body: { synthetic: true },
  }),
);
for (const [name, path, body] of [
  ['engine', '/v1/hooks/bad/SessionStart', {}],
  ['event', '/v1/hooks/claude/1bad', {}],
  ['payload', '/v1/hooks/claude/SessionStart', []],
])
  check(`hooks invalid ${name}`, () =>
    api(path, 400, undefined, { method: 'POST', token: agent, body }),
  );
check('hooks payload bound', () =>
  api('/v1/hooks/claude/SessionStart', 400, undefined, {
    method: 'POST',
    token: agent,
    body: { synthetic: 'x'.repeat(1024 * 1024) },
  }),
);
function upgrade(path, token) {
  return new Promise((resolve, reject) => {
    const req = httpRequest(new URL(path, base), {
      headers: {
        Connection: 'Upgrade',
        Upgrade: 'websocket',
        'Sec-WebSocket-Version': '13',
        'Sec-WebSocket-Key': randomBytes(16).toString('base64'),
        'Sec-WebSocket-Protocol': token ? `pitcrew.v1, pitcrew.bearer.${token}` : 'pitcrew.v1',
      },
    });
    req.setTimeout(5000, () => req.destroy(new Error('Upgrade timeout')));
    req.on('error', reject);
    req.on('upgrade', (res, socket) => {
      socket.destroy();
      resolve({ status: res.statusCode });
    });
    req.on('response', (res) => {
      let text = '';
      res.on('data', (chunk) => (text += chunk));
      res.on('end', () => {
        let data;
        try {
          data = JSON.parse(text);
        } catch {
          data = text;
        }
        resolve({ status: res.statusCode, data });
      });
    });
    req.end();
  });
}
async function refusedUpgrade(path, status, token = person) {
  const result = await upgrade(path, token);
  if (result.status !== status) throw new StatusMismatch(result.status, status);
  schemas.error(result.data);
  assert.equal(result.data.code, statuses[status]);
}
check('stream needs upgrade', () => api('/v1/stream', 400));
check('stream upgrade no token', () => refusedUpgrade('/v1/stream', 401, null));
check('stream upgrade agent forbidden', () => refusedUpgrade('/v1/stream', 403, agent));
check('stream query token refused', () =>
  refusedUpgrade('/v1/stream?token=synthetic-invalid-token', 401, null),
);
check('terminal needs upgrade', () => api(`/v1/sessions/${context.sessions[0].id}/terminal`, 400));
check('terminal unknown refused', () =>
  refusedUpgrade(`/v1/sessions/${missing}/terminal?cols=80&rows=24`, 404),
);
check('terminal no token refused', () =>
  refusedUpgrade(`/v1/sessions/${missing}/terminal?cols=80&rows=24`, 401, null),
);
check('terminal agent refused', () =>
  refusedUpgrade(`/v1/sessions/${missing}/terminal?cols=80&rows=24`, 403, agent),
);
check('terminal invalid size refused', () =>
  refusedUpgrade(`/v1/sessions/${context.sessions[0].id}/terminal?cols=0&rows=24`, 400),
);
async function stream(since, token = person) {
  const url = new URL('/v1/stream', base);
  url.protocol = 'ws:';
  if (since !== undefined) url.searchParams.set('since', since);
  const ws = new WebSocket(url, ['pitcrew.v1', `pitcrew.bearer.${token}`]);
  const frames = [];
  const waiters = [];
  let failure;
  ws.addEventListener('message', (event) => {
    try {
      const frame = JSON.parse(event.data);
      const waiter = waiters.shift();
      if (waiter) waiter.resolve(frame);
      else frames.push(frame);
    } catch (error) {
      failure = error;
    }
  });
  ws.addEventListener('error', () => {
    failure = new Error('Stream failed');
    for (const waiter of waiters.splice(0)) waiter.reject(failure);
  });
  const next = () =>
    frames.length
      ? Promise.resolve(frames.shift())
      : failure
        ? Promise.reject(failure)
        : new Promise((resolve, reject) => {
            const waiter = {
              resolve: (frame) => {
                clearTimeout(timer);
                resolve(frame);
              },
              reject: (error) => {
                clearTimeout(timer);
                reject(error);
              },
            };
            const timer = setTimeout(() => {
              const i = waiters.indexOf(waiter);
              if (i >= 0) waiters.splice(i, 1);
              reject(new Error('Frame timeout'));
            }, 5000);
            waiters.push(waiter);
          });
  return { ws, next };
}
check('stream hello/API mutation/replay', async () => {
  const first = await stream();
  let replay;
  try {
    const hello = await first.next();
    assert.equal(hello.type, 'hello');
    assert.ok(Number.isSafeInteger(hello.rev));
    assert.equal(typeof hello.log, 'string');
    assert.equal(first.ws.protocol, 'pitcrew.v1');
    const event = await api(`/v1/tasks/${context.task.id}/comments`, 201, schemas.event, {
      method: 'POST',
      body: { text: 'Synthetic streamed comment', mentions: [] },
    });
    const received = [];
    let end;
    for (let n = 0; n < 20; n++) {
      const frame = await first.next();
      if (frame.type === 'ping') continue;
      assert.equal(frame.type, 'events');
      schemas.events({ ...frame, at_start: false });
      received.push(...frame.events);
      end = frame.to_rev;
      if (received.some((e) => e.id === event.id)) break;
    }
    assert.ok(received.some((e) => e.id === event.id));
    first.ws.close();
    replay = await stream(hello.rev);
    const greeting = await replay.next();
    assert.equal(greeting.type, 'hello');
    assert.equal(greeting.log, hello.log);
    const missed = [];
    let cursor = hello.rev;
    while (cursor < end) {
      const frame = await replay.next();
      if (frame.type === 'ping') continue;
      assert.equal(frame.type, 'events');
      assert.equal(frame.from_rev, cursor + 1);
      missed.push(...frame.events);
      cursor = frame.to_rev;
    }
    assert.equal(missed.filter((e) => e.id === event.id).length, 1);
    assert.deepEqual(
      missed.slice(0, received.length).map((e) => e.id),
      received.map((e) => e.id),
    );
  } finally {
    first.ws.close();
    replay?.ws.close();
  }
});
for (const [path, keys] of [
  ['/v1/workstreams', ['project']],
  ['/v1/tasks', ['project', 'workstream', 'assignee']],
  ['/v1/sessions', ['machine', 'workstream', 'task']],
  ['/v1/asks', ['to']],
  ['/v1/events', ['project', 'workstream', 'task', 'session']],
  ['/v1/recaps/blocks', ['session', 'task', 'workstream', 'project']],
])
  for (const key of keys)
    check(`malformed query ${path} ${key}`, () => api(`${path}?${key}=bad`, 400));
for (const path of [
  '/v1/tasks?status=invalid',
  '/v1/sessions?state=invalid',
  '/v1/asks?state=invalid',
])
  check(`invalid enum query ${path}`, () => api(path, 400));
check('session commands validate bodies', async () => {
  const id = context.sessions.find((s) => s.state === 'working')?.id ?? context.sessions[0].id;
  for (const [verb, body] of [
    ['send', { text: 123 }],
    ['keys', { keys: ['invalid'] }],
    ['end', { mode: 'invalid' }],
  ])
    await api(`/v1/sessions/${id}/${verb}`, 400, undefined, { method: 'POST', body });
});
check('task assign key required', () =>
  api(`/v1/tasks/${context.task.id}/assign`, 400, undefined, { method: 'POST', body: {} }),
);
check('task blocked-by cycle', async () => {
  const other = await api('/v1/tasks', 201, schemas.task, {
    method: 'POST',
    body: { project: context.project.id, title: 'Synthetic second task' },
  });
  await api(`/v1/tasks/${other.id}`, 200, schemas.task, {
    method: 'PATCH',
    body: { blocked_by: [context.task.id] },
  });
  await api(`/v1/tasks/${context.task.id}`, 409, undefined, {
    method: 'PATCH',
    body: { blocked_by: [other.id] },
  });
});
check('prefixed query ids normalize like Rust', async () => {
  for (const [path, key, prefix, value] of [
    ['/v1/workstreams', 'project', 'prj', context.project.id],
    ['/v1/tasks', 'project', 'prj', context.project.id],
    ['/v1/sessions', 'machine', 'mch', context.local.id],
    ['/v1/asks', 'to', 'mem', context.agentMe.id],
    ['/v1/events', 'task', 'tsk', context.task.id],
  ]) {
    const bare = await raw(`${path}?${key}=${value}`);
    const prefixed = await raw(`${path}?${key}=${prefix}_${value.toLowerCase()}`);
    assert.equal(prefixed.status, bare.status);
    assert.deepEqual(prefixed.data, bare.data);
  }
});
for (const [path, method] of [
  ['/v1/projects', 'POST'],
  ['/v1/workstreams', 'POST'],
  ['/v1/tasks', 'POST'],
  ['/v1/sessions', 'POST'],
])
  check(`agent forbidden write ${path}`, () =>
    api(path, 403, undefined, { method, body: {}, token: agent }),
  );
// The other person-only writes, on things that exist, each with a body a person's request could
// carry, so only the token's scope refuses them.
for (const [method, route, path, body] of [
  ['PATCH', '/v1/tasks/{id}', () => `/v1/tasks/${context.task.id}`, { title: 'Synthetic' }],
  [
    'POST',
    '/v1/tasks/{id}/assign',
    () => `/v1/tasks/${context.task.id}/assign`,
    () => ({ assignee: context.agentMe.id }),
  ],
  [
    'POST',
    '/v1/tasks/{id}/dispatch',
    () => `/v1/tasks/${context.task.id}/dispatch`,
    () => ({ agent: context.agentMe.id }),
  ],
  [
    'PATCH',
    '/v1/workstreams/{id}',
    () => `/v1/workstreams/${context.workstream.id}`,
    { status: 'active' },
  ],
  [
    'POST',
    '/v1/sessions/{id}/send',
    () => `/v1/sessions/${context.sessions[0].id}/send`,
    { text: 'Synthetic' },
  ],
  [
    'POST',
    '/v1/sessions/{id}/keys',
    () => `/v1/sessions/${context.sessions[0].id}/keys`,
    { keys: ['enter'] },
  ],
  [
    'POST',
    '/v1/sessions/{id}/interrupt',
    () => `/v1/sessions/${context.sessions[0].id}/interrupt`,
    undefined,
  ],
  [
    'POST',
    '/v1/sessions/{id}/end',
    () => `/v1/sessions/${context.sessions[0].id}/end`,
    { mode: 'graceful' },
  ],
  [
    'POST',
    '/v1/sessions/{id}/link',
    () => `/v1/sessions/${context.sessions[0].id}/link`,
    () => ({ workstream: context.workstream.id }),
  ],
  [
    'PUT',
    '/v1/briefs/{kind}/{id}',
    () => `/v1/briefs/project/${context.project.id}`,
    { text: 'Synthetic' },
  ],
])
  check(`agent forbidden write ${method} ${route}`, () =>
    api(path(), 403, undefined, {
      method,
      body: typeof body === 'function' ? body() : body,
      token: agent,
    }),
  );
check('agent plan keeps human lines and stamps attribution', async () => {
  const t = await api('/v1/tasks', 201, schemas.task, {
    method: 'POST',
    body: {
      project: context.project.id,
      title: 'Synthetic agent-owned task',
      assignee: context.agentMe.id,
    },
  });
  const human = { id: missing, text: 'Keep human line', done: false, source: { kind: 'human' } };
  await api(`/v1/tasks/${t.id}/subtasks`, 200, schemas.task, { method: 'PUT', body: [human] });
  const own = {
    id: '01J00000000000000000000001',
    text: 'Synthetic agent plan',
    done: false,
    source: { kind: 'agent_plan', agent: context.agentMe.id },
  };
  const replaced = await api(`/v1/tasks/${t.id}/subtasks`, 200, schemas.task, {
    method: 'PUT',
    body: [own],
    token: agent,
  });
  assert.deepEqual(replaced.subtasks, [human, own]);
  const event = await api(`/v1/tasks/${t.id}/comments`, 201, schemas.event, {
    method: 'POST',
    token: agent,
    body: { text: 'Synthetic agent comment', mentions: [], author: missing, on_behalf_of: missing },
  });
  assert.equal(event.author, context.agentMe.id);
  assert.equal(event.on_behalf_of, context.me.id);
});

check('cursor metadata is private in live/replay and does not consume activity pages', async () => {
  const second = process.env.PITCREW_CONFORMANCE_SECOND_PERSON;
  const owner = await stream();
  const other = await stream(undefined, second);
  let replay;
  try {
    const hello = await owner.next();
    await other.next();
    const comments = [];
    for (let i = 0; i < 3; i++) {
      await api('/v1/me/cursors/workspace', 200, undefined, { method: 'PUT', body: { rev: hello.rev + i * 2 } });
      comments.push(await api(`/v1/tasks/${context.task.id}/comments`, 201, schemas.event, {
        method: 'POST', body: { text: `Synthetic privacy barrier ${i}`, mentions: [] },
      }));
    }
    const until = async (client) => {
      const seen = [];
      for (;;) {
        const frame = await client.next();
        if (frame.type !== 'events') continue;
        assert.equal(frame.events.length, frame.to_rev - frame.from_rev + 1);
        seen.push(...frame.events);
        if (seen.some((e) => e.id === comments[2].id)) return seen;
      }
    };
    const mine = await until(owner);
    assert.equal(mine.filter((e) => e.body.type === 'cursor_moved').length, 3);
    assert.equal((await until(other)).filter((e) => e.body.type === 'cursor_moved').length, 0);
    replay = await stream(hello.rev, second);
    await replay.next();
    assert.equal((await until(replay)).filter((e) => e.body.type === 'cursor_moved').length, 0);
    for (const route of ['/v1/events', '/v1/activity']) {
      // Other conformance files write concurrently; read the privacy barrier snapshot.
      const page = await api(`${route}?limit=3&before=${hello.rev + 7}`);
      assert.deepEqual(page.events.map((e) => e.id), comments.map((e) => e.id));
      assert.deepEqual(page.revisions, [hello.rev + 2, hello.rev + 4, hello.rev + 6]);
      assert.equal(page.from_rev, page.revisions[0]);
      assert.equal(page.to_rev, page.revisions[2]);
      assert.equal(page.at_start, false);
      const all = await api(`${route}?limit=500`);
      assert.ok(all.events.every((e) => e.body.type !== 'cursor_moved'));
      await api(route, 403, undefined, { token: agent });
    }
  } finally {
    owner.ws.close(); other.ws.close(); replay?.ws.close();
  }
});

check('session launch options are machine-scoped and person-only', async () => {
  const machines = await api('/v1/machines');
  const local = machines.find((m) => m.kind === 'local');
  assert.ok(local);
  const path = `/v1/machines/${local.id}/session-options`;
  await api(path, 403, undefined, { token: agent });
  await api(`/v1/machines/${missing}/session-options`, 404);
  const remote = machines.find((m) => m.kind === 'ssh');
  if (remote) await api(`/v1/machines/${remote.id}/session-options`, 503);
  const options = await api(path);
  assert.ok(['unix', 'windows'].includes(options.platform));
  assert.ok(options.engines.some((e) => e.engine === 'claude'));
  for (const e of options.engines) {
    assert.ok(['claude', 'codex', 'opencode'].includes(e.engine));
    assert.ok(e.permission_modes.includes('default'));
    assert.ok(Array.isArray(e.first_prompt_forbidden));
    assert.ok(e.first_prompt_forbidden.every((c) => typeof c === 'string' && [...c].length === 1));
    assert.ok(!e.permission_modes.includes('bypass_permissions'));
    if (e.engine === 'codex') assert.ok(!e.permission_modes.includes('plan'));
    if (e.engine === 'opencode') assert.deepEqual(e.permission_modes, ['default']);
  }
});

check('unnamed start returns a terminal without waiting for a transcript', async () => {
  const machines = await api('/v1/machines');
  const machine = machines.find((m) => m.kind === 'local').id;
  const cwd = process.env.PITCREW_FILES_ROOT;
  assert.ok(cwd);
  await api('/v1/sessions', 400, undefined, { method: 'POST', body: {machine, engine:'claude', cwd, title:'   '} });
  const session = await api('/v1/sessions', 202, schemas.session, { method:'POST', body: { machine, engine:'claude', cwd, title:'Synthetic start' } });
  assert.equal(session.title, 'Synthetic start');
  assert.ok(session.terminal);
  const found = await api(`/v1/sessions/${session.id}`, 200, schemas.session);
  assert.equal(found.terminal, session.terminal);
  await api(`/v1/sessions/${session.id}/end`, 204, undefined, { method:'POST', body:{mode:'kill'} });
});

check('invalid launch preflight leaves no sessions and saved safety is applied', async () => {
  const machines = await api('/v1/machines');
  const machine = machines.find((m) => m.kind === 'local').id;
  const cwd = process.env.PITCREW_FILES_ROOT;
  const before = (await api('/v1/sessions')).map((s) => s.id).sort();
  for (const body of [
    { machine, cwd, engine: 'codex', permission_mode: 'plan' },
    { machine, cwd, engine: 'opencode', permission_mode: 'accept_edits' },
    { machine, cwd, engine: 'claude', permission_mode: 'bypass_permissions' },
    ...['\nTitle', 'Title\r', '\tTitle', 'Title\u0085'].map((title) => ({ machine, cwd, engine: 'claude', title })),
  ]) await api('/v1/sessions', 400, undefined, { method: 'POST', body });
  const remote = machines.find((m) => m.kind !== 'local');
  if (remote) await api('/v1/sessions', 503, undefined, { method: 'POST', body: { machine: remote.id, engine: 'claude', cwd } });
  const original = await api('/v1/safety');
  const { saved: _saved, ...settings } = original;
  try {
    await api('/v1/safety', 200, undefined, { method: 'PUT', body: { ...settings, permission_mode: 'plan' } });
    await api('/v1/sessions', 400, undefined, { method: 'POST', body: { machine, cwd, engine: 'codex' } });
  } finally {
    await api('/v1/safety', 200, undefined, { method: 'PUT', body: settings });
  }
  assert.deepEqual((await api('/v1/sessions')).map((s) => s.id).sort(), before);
});

check('two person starts without prompts share a folder and have different terminals', async () => {
  const machine = (await api('/v1/machines')).find((m) => m.kind === 'local').id;
  const body = { machine, cwd: process.env.PITCREW_FILES_ROOT, engine: 'codex', permission_mode: 'default' };
  const started = [];
  try {
    for (let i = 0; i < 2; i++) started.push(await api('/v1/sessions', 202, schemas.session, { method: 'POST', body }));
    assert.notEqual(started[0].id, started[1].id);
    assert.notEqual(started[0].terminal, started[1].terminal);
  } finally {
    for (const session of started) await api(`/v1/sessions/${session.id}/end`, 204, undefined, { method: 'POST', body: { mode: 'kill' } });
  }
});

check('a workstream start is linked before transcript discovery on both hubs', async () => {
  const machine = (await api('/v1/machines')).find((m) => m.kind === 'local').id;
  const workstream = await api('/v1/workstreams', 201, schemas.workstream, { method: 'POST', body: {
    project: context.project.id, name: 'Synthetic start link', locations: [],
  } });
  const session = await api('/v1/sessions', 202, schemas.session, { method: 'POST', body: {
    machine, engine: 'claude', cwd: process.env.PITCREW_FILES_ROOT, workstream: workstream.id, permission_mode: 'default',
  } });
  try {
    assert.equal(session.workstream, workstream.id);
    assert.equal(session.link_basis, 'manual');
    const found = await api(`/v1/sessions/${session.id}`, 200, schemas.session);
    assert.equal(found.workstream, workstream.id);
    assert.equal(found.link_basis, 'manual');
  } finally {
    await api(`/v1/sessions/${session.id}/end`, 204, undefined, { method: 'POST', body: { mode: 'kill' } });
  }
});

check('directory creation and editing are device-only, validated, event-backed and atomic', async () => {
  const recipe = { name: '  Synthetic author  ', engine: 'claude', model: 'demo-model', instructions: 'Synthetic\nexamples.', permission_mode: 'plan' };
  for (const [path, body] of [['/v1/personas', recipe], ['/v1/teams', { name: 'Synthetic crew', lead: missing, members: [] }]]) {
    await api(path, 401, undefined, { method: 'POST', token: '', body });
    await api(path, 403, undefined, { method: 'POST', token: agent, body });
  }
  const rev = (await api('/v1/workspace')).rev;
  await api('/v1/personas', 400, undefined, { method: 'POST', body: { ...recipe, name: ' ' } });
  await api('/v1/personas', 400, undefined, { method: 'POST', body: { ...recipe, engine: 'invalid' } });
  await api('/v1/personas', 400, undefined, { method: 'POST', body: { ...recipe, model: 'x'.repeat(201) } });
  await api('/v1/personas', 400, undefined, { method: 'POST', body: { ...recipe, instructions: 'x'.repeat(32001) } });
  assert.equal((await api('/v1/workspace')).rev, rev);
  const persona = await api('/v1/personas', 201, undefined, { method: 'POST', body: { ...recipe, id: missing, author: missing } });
  assert.equal(persona.name, 'Synthetic author'); assert.notEqual(persona.id, missing);
  schemas.personas([persona]);
  let members = await api('/v1/members');
  const member = members.find((m) => m.persona === persona.id);
  const me = await api('/v1/me');
  assert.equal(member.kind, 'agent'); assert.equal(member.owner, me.id);
  const second = process.env.PITCREW_CONFORMANCE_SECOND_PERSON;
  assert.ok(second, 'second person fixture token');
  const beforeEdit = (await api('/v1/workspace')).rev;
  await api(`/v1/personas/${persona.id}`, 403, undefined, { method: 'PUT', token: second, body: { ...recipe, name: 'Stolen' } });
  for (const change of [{ permission_mode: 'bypass_permissions' }, { model: '--dangerously-skip-permissions' }]) {
    await api('/v1/personas', 400, undefined, { method: 'POST', body: { ...recipe, ...change } });
    await api(`/v1/personas/${persona.id}`, 400, undefined, { method: 'PUT', body: { ...recipe, ...change } });
  }
  assert.equal((await api('/v1/workspace')).rev, beforeEdit);
  assert.equal((await api('/v1/personas')).find((p) => p.id === persona.id).name, 'Synthetic author');
  assert.equal((await api('/v1/members')).find((m) => m.id === member.id).name, 'Synthetic author');

  const renamed = await api(`/v1/personas/per_${persona.id.replace(/^per_/, '')}`, 200, undefined, { method: 'PUT', body: { ...recipe, name: 'Renamed author' } });
  assert.equal(renamed.id, persona.id);
  members = await api('/v1/members');
  assert.equal(members.find((m) => m.id === member.id).name, 'Renamed author');
  const dispatchTask = await api('/v1/tasks', 201, schemas.task, { method: 'POST', body: { project: context.project.id, title: 'Dispatch new agent' } });
  const serviceActors = members.filter((m) => m.kind === 'agent' && m.owner === me.id && m.persona === undefined);
  assert.ok(serviceActors.length > 0, 'owned service actor fixture');
  const beforeServiceDispatch = (await api('/v1/workspace')).rev;
  for (const actor of serviceActors) {
    await api(`/v1/tasks/${dispatchTask.id}/dispatch`, 400, undefined, { method: 'POST', body: { agent: actor.id } });
  }
  assert.equal((await api('/v1/workspace')).rev, beforeServiceDispatch);
  context.directoryDispatch = { task: dispatchTask.id, agent: member.id };
  const teamBody = { name: 'Synthetic crew', lead: me.id, members: [member.id, member.id] };
  const teamRev = (await api('/v1/workspace')).rev;
  await api('/v1/teams', 400, undefined, { method: 'POST', body: { ...teamBody, members: [missing] } });
  await api('/v1/teams', 400, undefined, { method: 'POST', body: { ...teamBody, lead: missing } });
  await api('/v1/teams', 400, undefined, { method: 'POST', body: { ...teamBody, members: Array(257).fill(member.id) } });
  assert.equal((await api('/v1/workspace')).rev, teamRev);
  const team = await api('/v1/teams', 201, undefined, { method: 'POST', body: teamBody });
  schemas.teams([team]); assert.deepEqual(team.members, [me.id, member.id]);
  const updated = await api(`/v1/teams/team_${team.id.replace(/^team_/, '')}`, 200, undefined, { method: 'PUT', body: { ...teamBody, name: 'Renamed crew', lead: member.id, members: [] } });
  assert.deepEqual(updated.members, [member.id]); assert.equal(updated.id, team.id);
  for (const path of [`/v1/personas/${persona.id}`, `/v1/teams/${team.id}`]) {
    await api(path, 403, undefined, { method: 'PUT', token: agent, body: {} });
  }
  for (const path of [`/v1/personas/${missing}`, `/v1/teams/${missing}`, '/v1/personas/bad', '/v1/teams/bad']) {
    await api(path, 404, undefined, { method: 'PUT', body: {} });
  }
  const events = (await api('/v1/events?limit=100')).events;
  assert.ok(events.some((e) => e.body.type === 'persona_saved' && e.body.data.persona.id === persona.id && e.author === me.id));
  assert.ok(events.some((e) => e.body.type === 'team_saved' && e.body.data.team.id === team.id && e.author === me.id));
});

check('project and optional first workstream are created together or neither is created', async () => {
  const key = 'ATOMIC';
  const body = { key, name: 'Atomic project', first_workstream: '  ' };
  const rev = (await api('/v1/workspace')).rev;
  await api('/v1/projects', 400, undefined, { method: 'POST', body });
  assert.equal((await api('/v1/workspace')).rev, rev);
  assert.ok(!(await api('/v1/projects')).some((p) => p.key === key));
  const project = await api('/v1/projects', 201, schemas.project, { method: 'POST', body: { ...body, first_workstream: 'First stream' } });
  const streams = await api(`/v1/workstreams?project=${project.id}`);
  assert.equal(streams.length, 1); assert.equal(streams[0].name, 'First stream');
  const after = (await api('/v1/workspace')).rev;
  await api('/v1/projects', 409, undefined, { method: 'POST', body: { ...body, first_workstream: 'Second stream' } });
  assert.equal((await api('/v1/workspace')).rev, after);
  assert.equal((await api(`/v1/workstreams?project=${project.id}`)).length, 1);
});

// A successful dispatch emits runner events asynchronously. Keep it after exact-revision
// refusal checks so unrelated launch events cannot look like mutations by refused writes.
check('the newly created directory agent can dispatch a task', async () => {
  const { task, agent: member } = context.directoryDispatch;
  const reply = await raw(`/v1/tasks/${task}/dispatch`, { method: 'POST', body: { agent: member } });
  assert.equal(reply.status, 202, reply.data?.message);
  schemas.dispatch(reply.data);
  assert.equal(reply.data.task, task);
});
