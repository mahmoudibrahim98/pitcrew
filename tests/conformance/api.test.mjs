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
let context = {};
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
check('session link manual', async () => {
  const s = await api(`/v1/sessions/${context.sessions[0].id}/link`, 200, schemas.session, {
    method: 'POST',
    body: { workstream: context.workstream.id },
  });
  assert.equal(s.workstream, context.workstream.id);
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
  const newest = await api('/v1/events', 200, schemas.events);
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
async function stream(since) {
  const url = new URL('/v1/stream', base);
  url.protocol = 'ws:';
  if (since !== undefined) url.searchParams.set('since', since);
  const ws = new WebSocket(url, ['pitcrew.v1', `pitcrew.bearer.${person}`]);
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
