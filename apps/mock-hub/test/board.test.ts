// Board drafts (api-v1.md, "Board drafts"): the preview sends nothing, a start needs what the
// preview showed, only the drafting agent proposes, and nothing is created until a person reviews
// the proposal; rejected items create nothing.

import assert from 'node:assert/strict';
import { describe, it } from 'node:test';
import { redactLine } from '../src/board.ts';
import type { RunningServer } from '../src/server.ts';
import type { BoardDraft, DraftPreview, Event, Task } from '../src/types.ts';
import { AGENT, DEVICE, ID, call, sleep, withServer } from './helpers.ts';

const FAST = { delays: { start: 20, reply: 20 } };
const OFFICE = '01JB000000000000000MEM0006';

async function preview(server: RunningServer, workstream = ID.submission): Promise<DraftPreview> {
  const res = await call<DraftPreview>(server, 'GET', `/v1/workstreams/${workstream}/board-draft`, { token: DEVICE });
  assert.equal(res.status, 200);
  return res.body;
}

async function start(server: RunningServer, json: unknown, workstream = ID.submission) {
  return call<BoardDraft>(server, 'POST', `/v1/workstreams/${workstream}/board-drafts`, { token: DEVICE, json });
}

async function tasks(server: RunningServer): Promise<Task[]> {
  return (await call<Task[]>(server, 'GET', '/v1/tasks', { token: DEVICE })).body;
}

async function events(server: RunningServer): Promise<Event[]> {
  return (await call<{ events: Event[] }>(server, 'GET', '/v1/events?limit=500', { token: DEVICE })).body.events;
}

const PROPOSAL = {
  tasks: [
    { title: 'Finish the method section', status: 'in_progress', evidence: [ID.ses1] },
    { title: 'Ask for a second review', status: 'todo' },
    { title: 'Submit', status: 'done', description: 'It went out.' },
  ],
  note: 'Synthetic.',
};

describe('board drafts', () => {
  it('previews what would be sent, redacted and bounded, and stores nothing', () =>
    withServer(async (server) => {
      const before = (await events(server)).length;
      const shown = await preview(server);
      assert.equal(shown.workstream, ID.submission);
      assert.equal(shown.prompt, 'draft-board/v1');
      assert.match(shown.digest, /^[0-9a-f]{64}$/);
      assert.ok(shown.summary.includes(ID.ses1));
      assert.ok(shown.summary.includes('PAP-1'));
      assert.ok(!shown.summary.includes(ID.ses2), 'another workstream’s session');
      assert.equal(shown.cost.sessions, 2);
      assert.equal(shown.cost.tasks, 4);
      assert.equal(shown.cost.summary_bytes, Buffer.byteLength(shown.summary));
      assert.equal(shown.cost.estimate.output_tokens, 8192);
      assert.equal(shown.cost.estimate.input_tokens, 15000 + Math.ceil(shown.cost.prompt_bytes / 4));
      assert.deepEqual(await preview(server), shown);
      assert.equal((await events(server)).length, before);
      const agent = await call(server, 'GET', `/v1/workstreams/${ID.submission}/board-draft`, { token: AGENT });
      assert.equal(agent.status, 403);
      const missing = await call(server, 'GET', '/v1/workstreams/01J00000000000000000000000/board-draft', { token: DEVICE });
      assert.equal(missing.status, 404);
    }));

  it('creates nothing until the person accepts, and nothing for a rejected item', () =>
    withServer(async (server) => {
      const count = (await tasks(server)).length;
      const shown = await preview(server);
      assert.equal((await start(server, { agent: ID.writer, digest: '00' })).status, 409);
      const started = await start(server, { agent: ID.writer, digest: shown.digest });
      assert.equal(started.status, 202);
      const draft = started.body;
      assert.equal(draft.state, 'running');
      assert.equal(draft.agent, ID.writer);
      assert.equal((await start(server, { agent: ID.writer, digest: shown.digest })).status, 409, 'one at a time');
      assert.equal((await tasks(server)).length, count);

      const path = `/v1/board-drafts/${draft.id}/proposal`;
      assert.equal((await call(server, 'POST', path, { token: DEVICE, json: PROPOSAL })).status, 403);
      const bad = await call(server, 'POST', path, {
        token: AGENT,
        json: { tasks: [{ title: 'T', status: 'todo', evidence: [ID.ses2] }] },
      });
      assert.equal(bad.status, 400);
      const proposed = await call<BoardDraft>(server, 'POST', path, { token: AGENT, json: PROPOSAL });
      assert.equal(proposed.status, 201);
      assert.equal(proposed.body.state, 'proposed');
      assert.equal((await call(server, 'POST', path, { token: AGENT, json: PROPOSAL })).status, 409);
      assert.equal((await tasks(server)).length, count, 'a proposal creates nothing');

      const review = await call<{ draft: BoardDraft; tasks: Task[] }>(server, 'POST', `/v1/board-drafts/${draft.id}/review`, {
        token: DEVICE,
        json: { accept: [2, 0] },
      });
      assert.equal(review.status, 200);
      assert.deepEqual(review.body.tasks.map((t) => [t.title, t.status, t.labels, t.workstream]), [
        ['Finish the method section', 'in_progress', ['drafted'], ID.submission],
        ['Submit', 'done', ['drafted'], ID.submission],
      ]);
      assert.equal(review.body.draft.state, 'reviewed');
      assert.deepEqual(review.body.draft.rejected, [1]);
      const after = await tasks(server);
      assert.equal(after.length, count + 2);
      assert.ok(!after.some((t) => t.title === 'Ask for a second review'));
      const again = await call(server, 'POST', `/v1/board-drafts/${draft.id}/review`, { token: DEVICE, json: { accept: [] } });
      assert.equal(again.status, 409);
      const types = (await events(server))
        .map((e) => e.body.type)
        .filter((t) => t.startsWith('task_') || t.startsWith('board_'));
      assert.deepEqual(types.slice(-4), ['board_proposed', 'task_created', 'task_created', 'board_draft_reviewed']);
      const listed = await call<BoardDraft[]>(server, 'GET', `/v1/board-drafts?workstream=${ID.submission}`, { token: DEVICE });
      assert.deepEqual(listed.body.map((d) => d.id), [draft.id]);
    }));

  it('plays the back office for a draft run by @office', () =>
    withServer(async (server) => {
      const shown = await preview(server);
      const started = await start(server, { digest: shown.digest });
      assert.equal(started.status, 202);
      assert.equal(started.body.agent, OFFICE);
      let draft = started.body;
      for (let i = 0; i < 100 && draft.state === 'running'; i += 1) {
        await sleep(20);
        draft = (await call<BoardDraft>(server, 'GET', `/v1/board-drafts/${draft.id}`, { token: DEVICE })).body;
      }
      assert.equal(draft.state, 'proposed');
      assert.ok((draft.proposal?.tasks.length ?? 0) > 0);
      const none = await call<{ tasks: Task[] }>(server, 'POST', `/v1/board-drafts/${draft.id}/review`, {
        token: DEVICE,
        json: { accept: [] },
      });
      assert.deepEqual(none.body.tasks, []);
    }, FAST));

  it('redacts as the hub does', () => {
    // A failure names the case only: the texts hold synthetic secrets, which it must not print.
    for (const [text, want] of [
      ['push with ghp_16C7e42F292c6912E7710c838347Ae178B4a', 'push with [redacted]'],
      ["curl -H 'Authorization: Bearer abc.def.ghi' x", "curl -H 'Authorization: Bearer [redacted]' x"],
      ['password=hunter22 and more', 'password=[redacted] and more'],
      ['mail sam@example.com today', 'mail [email] today'],
      ['cd /home/sam/work/paper', 'cd ~/work/paper'],
      ['branch fix/sam@example.com', 'branch fix/[email]'],
      ['Draft the method section', 'Draft the method section'],
      ['uses sk-learn-tutorial', 'uses sk-learn-tutorial'],
    ] as const) {
      assert.ok(redactLine(text, 500).text === want, `case ${want}: not redacted as expected`);
    }
  });
});
