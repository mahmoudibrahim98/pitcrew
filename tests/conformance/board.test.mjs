// Board drafts (api-v1.md, "Board drafts"), against either target. Run serially after the shared
// suite: a draft starts an agent's CLI (a stand-in on the daemon) and creates tasks.
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { test } from 'node:test';
import { setTimeout as delay } from 'node:timers/promises';
import { list, schemas } from './schema.mjs';

const base = process.env.PITCREW_CONFORMANCE_URL;
const person = process.env.PITCREW_CONFORMANCE_PERSON;
const agent = process.env.PITCREW_CONFORMANCE_AGENT;
const sessionTokens = process.env.PITCREW_CONFORMANCE_SESSION_TOKENS;
assert.ok(base && person && agent && sessionTokens, 'Set PITCREW_CONFORMANCE_URL, _PERSON, _AGENT and _SESSION_TOKENS');
assert.ok(['127.0.0.1', 'localhost', '[::1]'].includes(new URL(base).hostname));
const missing = '01J00000000000000000000000';
const codes = { 400: 'invalid', 401: 'unauthorized', 403: 'forbidden', 404: 'not_found', 409: 'conflict' };

/** The session token a draft's CLI was given, as the target hands it to the suite. Never printed. */
async function sessionToken(session) {
  for (let i = 0; i < 300; i += 1) {
    const token = await readFile(join(sessionTokens, `${session}.token`), 'utf8').catch(() => undefined);
    if (token !== undefined && token.trim() !== '') return token.trim();
    await delay(50);
  }
  throw new Error('The draft\'s session token never reached the suite.');
}

async function call(method, path, body, token = person) {
  const response = await fetch(base + path, {
    method,
    headers: { Authorization: `Bearer ${token}`, 'Content-Type': 'application/json' },
    ...(body === undefined ? {} : { body: typeof body === 'string' ? body : JSON.stringify(body) }),
    signal: AbortSignal.timeout(15000),
  });
  const text = await response.text();
  return { status: response.status, body: text === '' ? undefined : JSON.parse(text) };
}

async function expect(status, method, path, body, token, schema) {
  const response = await call(method, path, body, token);
  assert.equal(response.status, status, `${method} ${path}: ${JSON.stringify(response.body)}`);
  if (status >= 400) {
    schemas.error(response.body);
    assert.equal(response.body.code, codes[status]);
  } else if (schema) schema(response.body);
  return response.body;
}

const proposal = {
  tasks: [
    { title: 'Synthetic open work', status: 'todo' },
    { title: 'Synthetic rejected idea', status: 'backlog' },
    { title: 'Synthetic finished work', status: 'done', description: 'Synthetic description.' },
  ],
  note: 'Synthetic note.',
};

test('board drafts: preview first, only the draft\'s session token proposes, nothing created until reviewed', async () => {
  const me = await expect(200, 'GET', '/v1/me', undefined, person, schemas.member);
  const agentMe = await expect(200, 'GET', '/v1/me', undefined, agent, schemas.member);
  const project = await expect(201, 'POST', '/v1/projects', { key: 'DRFT', name: 'Draft conformance' }, person, schemas.project);
  const workstream = await expect(201, 'POST', '/v1/workstreams', { project: project.id, name: 'Drafted stream' }, person, schemas.workstream);
  const previewPath = `/v1/workstreams/${workstream.id}/board-draft`;
  const startPath = `/v1/workstreams/${workstream.id}/board-drafts`;

  // People only; unknown workstreams are 404.
  await expect(403, 'GET', previewPath, undefined, agent);
  await expect(403, 'POST', startPath, { digest: 'x' }, agent);
  await expect(403, 'GET', '/v1/board-drafts', undefined, agent);
  await expect(404, 'GET', `/v1/workstreams/${missing}/board-draft`);
  await expect(404, 'POST', `/v1/workstreams/${missing}/board-drafts`, { digest: 'x' });
  await expect(404, 'GET', `/v1/board-drafts/${missing}`);

  // The preview: what would be sent, its size and the estimate. It stores nothing.
  const rev = (await expect(200, 'GET', '/v1/workspace', undefined, person, schemas.workspace)).rev;
  const empty = await expect(200, 'GET', previewPath, undefined, person, schemas.draftPreview);
  assert.equal(empty.workstream, workstream.id);
  assert.equal(empty.prompt, 'draft-board/v1');
  assert.match(empty.digest, /^[0-9a-f]{64}$/);
  assert.equal(empty.cost.sessions, 0);
  assert.equal(empty.cost.tasks, 0);
  assert.equal(empty.cost.summary_bytes, Buffer.byteLength(empty.summary));
  assert.ok(empty.cost.prompt_bytes > empty.cost.summary_bytes);
  assert.equal(empty.cost.estimate.input_tokens, 15000 + Math.ceil(empty.cost.prompt_bytes / 4));
  assert.equal(empty.cost.estimate.output_tokens, 8192);
  assert.equal((await expect(200, 'GET', '/v1/workspace', undefined, person, schemas.workspace)).rev, rev);

  // The workstream changes: the old preview no longer starts anything.
  const existing = await expect(201, 'POST', '/v1/tasks', { project: project.id, workstream: workstream.id, title: 'Synthetic existing task' }, person, schemas.task);
  const shown = await expect(200, 'GET', previewPath, undefined, person, schemas.draftPreview);
  assert.equal(shown.cost.tasks, 1);
  assert.notEqual(shown.digest, empty.digest);
  assert.ok(shown.summary.includes(existing.key));
  await expect(409, 'POST', startPath, { agent: agentMe.id, digest: empty.digest });
  await expect(400, 'POST', startPath, { agent: agentMe.id });
  await expect(400, 'POST', startPath, { agent: me.id, digest: shown.digest });
  await expect(400, 'POST', startPath, { agent: missing, digest: shown.digest });
  await expect(400, 'POST', startPath, [1]);

  const draft = await expect(202, 'POST', startPath, { agent: agentMe.id, digest: shown.digest }, person, schemas.boardDraft);
  assert.equal(draft.state, 'running');
  assert.equal(draft.workstream, workstream.id);
  assert.equal(draft.agent, agentMe.id);
  assert.equal(draft.by, me.id);
  assert.equal(draft.prompt, 'draft-board/v1');
  assert.deepEqual(draft.cost, shown.cost);
  await expect(409, 'POST', startPath, { agent: agentMe.id, digest: shown.digest });
  const listed = await expect(200, 'GET', `/v1/board-drafts?workstream=${workstream.id}`, undefined, person, list(schemas.boardDraft));
  assert.deepEqual(listed.map((d) => d.id), [draft.id]);
  const one = await expect(200, 'GET', `/v1/board-drafts/${draft.id}`, undefined, person, schemas.boardDraft);
  assert.equal(one.id, draft.id);

  // Only the draft's own session token proposes, and only within the bounds; it does nothing
  // else, and its agent's own token does not propose.
  const proposalPath = `/v1/board-drafts/${draft.id}/proposal`;
  const drafter = await sessionToken(draft.session);
  assert.match(drafter, /^pcs_/);
  await expect(403, 'POST', proposalPath, proposal, person);
  await expect(403, 'POST', proposalPath, proposal, agent);
  await expect(403, 'GET', '/v1/me', undefined, drafter);
  await expect(403, 'GET', '/v1/tasks', undefined, drafter);
  await expect(403, 'GET', '/v1/board-drafts', undefined, drafter);
  await expect(404, 'POST', `/v1/board-drafts/${missing}/proposal`, proposal, drafter);
  const demoSession = (await expect(200, 'GET', '/v1/sessions', undefined, person, list(schemas.session))).find(
    (s) => s.workstream !== workstream.id,
  );
  assert.ok(demoSession, 'the seeded demo has sessions');
  for (const bad of [
    { tasks: [{ title: 'Synthetic', status: 'todo', evidence: [demoSession.id] }] },
    { tasks: [{ title: 'Synthetic', status: 'canceled' }] },
    { tasks: [{ title: ' ', status: 'todo' }] },
    { tasks: [{ title: 'Synthetic', status: 'todo', description: 'd'.repeat(33 * 1024) }] },
    { note: 'no tasks' },
  ]) {
    await expect(400, 'POST', proposalPath, bad, drafter);
  }
  const before = await expect(200, 'GET', `/v1/tasks?workstream=${workstream.id}`, undefined, person, list(schemas.task));
  const proposed = await expect(201, 'POST', proposalPath, proposal, drafter, schemas.boardDraft);
  assert.equal(proposed.state, 'proposed');
  assert.deepEqual(proposed.proposal.tasks.map((t) => t.title), proposal.tasks.map((t) => t.title));
  // Its token stops once it has proposed.
  await expect(401, 'POST', proposalPath, proposal, drafter);
  // A proposal creates nothing.
  const still = await expect(200, 'GET', `/v1/tasks?workstream=${workstream.id}`, undefined, person, list(schemas.task));
  assert.equal(still.length, before.length);

  // The review: agents never; bad indexes; then the accepted items become tasks, and only they.
  const reviewPath = `/v1/board-drafts/${draft.id}/review`;
  await expect(403, 'POST', reviewPath, { accept: [0] }, agent);
  await expect(404, 'POST', `/v1/board-drafts/${missing}/review`, { accept: [] });
  await expect(400, 'POST', reviewPath, { accept: [3] });
  await expect(400, 'POST', reviewPath, { accept: [0, 0] });
  await expect(400, 'POST', reviewPath, {});
  const reviewed = await expect(200, 'POST', reviewPath, { accept: [2, 0] }, person, schemas.draftReviewed);
  assert.equal(reviewed.draft.state, 'reviewed');
  assert.deepEqual(reviewed.draft.rejected, [1]);
  assert.deepEqual(reviewed.draft.accepted.map((a) => a.item), [0, 2]);
  assert.deepEqual(
    reviewed.tasks.map((t) => [t.title, t.status, t.labels, t.workstream, t.project]),
    [
      ['Synthetic open work', 'todo', ['drafted'], workstream.id, project.id],
      ['Synthetic finished work', 'done', ['drafted'], workstream.id, project.id],
    ],
  );
  assert.equal(reviewed.tasks[1].description, 'Synthetic description.');
  const after = await expect(200, 'GET', `/v1/tasks?workstream=${workstream.id}`, undefined, person, list(schemas.task));
  assert.equal(after.length, before.length + 2);
  assert.ok(!after.some((t) => t.title === 'Synthetic rejected idea'));
  await expect(409, 'POST', reviewPath, { accept: [] });

  // The events are in the activity feed, in their shapes.
  const page = await expect(200, 'GET', '/v1/events?limit=100', undefined, person, schemas.events);
  const types = page.events.map((e) => e.body.type);
  for (const type of ['board_draft_started', 'board_proposed', 'board_draft_reviewed']) {
    assert.ok(types.includes(type), `${type} in ${types.join(', ')}`);
  }
});
