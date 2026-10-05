// The Orchestrator (api-v1.md, "Orchestrator") and the reader token it gives its CLI: a question
// starts a session of the person's back office, the answer cites what the hub knows and suggests
// without acting, follow-ups type into the live session, and the reader reads and writes nothing.

import assert from 'node:assert/strict';
import { describe, it } from 'node:test';
import { cleanQuestion, scan, typed } from '../src/orchestrator.ts';
import type { RunningServer } from '../src/server.ts';
import type { Conversation, Orchestrator, Session, Task, TranscriptPage } from '../src/types.ts';
import { AGENT, DEVICE, ID, call, sleep, withServer } from './helpers.ts';

const FAST = { delays: { start: 20, reply: 40 } };
const READER = 'dev-reader-token';
/** The demo's own machine ("This laptop"). */
const LAPTOP = '01JB000000000000000MCH0001';
const SECOND = 'dev-second-device-token';
const OFFICE = '01JB000000000000000MEM0006';

async function state(server: RunningServer, token = DEVICE): Promise<Orchestrator> {
  const res = await call<Orchestrator>(server, 'GET', '/v1/orchestrator', { token });
  assert.equal(res.status, 200);
  return res.body;
}

async function ask(server: RunningServer, json: unknown, token = DEVICE) {
  return call<Conversation>(server, 'POST', '/v1/orchestrator/questions', { token, json });
}

async function answered(server: RunningServer, index = 0): Promise<Conversation> {
  for (let i = 0; i < 100; i += 1) {
    const now = await state(server);
    const turn = now.conversations[0]?.turns[index];
    if (turn !== undefined && turn.state !== 'answering') return now.conversations[0]!;
    await sleep(20);
  }
  throw new Error('never answered');
}

describe('the orchestrator', () => {
  it('answers a question in a session of the back office, with links and suggestions', () =>
    withServer(async (server) => {
      const empty = await state(server);
      assert.deepEqual(empty.engines, [
        { engine: 'claude', installed: true },
        { engine: 'codex', installed: true },
        { engine: 'opencode', installed: false },
      ]);
      assert.deepEqual(empty.conversations, []);
      assert.equal(empty.engine, undefined);
      assert.equal(empty.limits.question_chars, 4000);

      const res = await ask(server, { text: '  What did my agents do today?\u202E ' });
      assert.equal(res.status, 202);
      assert.equal(res.body.agent, OFFICE);
      assert.equal(res.body.engine, 'claude');
      const turn = res.body.turns[0]!;
      assert.equal(turn.question, 'What did my agents do today?');
      assert.equal(turn.state, 'answering');
      assert.equal(turn.usage, undefined);
      const session = (await call<Session>(server, 'GET', `/v1/sessions/${turn.session}`, { token: DEVICE })).body;
      assert.equal(session.title, 'Orchestrator');
      assert.equal(session.agent, OFFICE);
      assert.equal(session.workstream, undefined);
      const page = (await call<TranscriptPage>(server, 'GET', `/v1/sessions/${turn.session}/transcript`, { token: DEVICE })).body;
      const prompt = page.items.find((i) => i.kind === 'user_prompt');
      assert.ok(prompt !== undefined && 'text' in prompt && prompt.text.includes('pitcrew session list'));
      assert.ok(prompt !== undefined && 'text' in prompt && prompt.text.endsWith('What did my agents do today?\n'));

      const done = await answered(server);
      const answer = done.turns[0]!;
      assert.equal(answer.state, 'answered');
      assert.ok(answer.answer.includes(`ses_${ID.ses1}`), answer.answer);
      assert.ok(!answer.answer.includes('Suggestion:'));
      assert.ok(answer.usage !== undefined && answer.usage.answer_bytes === Buffer.byteLength(answer.answer));
      const kinds = answer.references.map((r) => r.target.kind);
      assert.ok(kinds.includes('session') && kinds.includes('task') && kinds.includes('recap'), JSON.stringify(kinds));
      for (const reference of answer.references) {
        assert.ok(answer.answer.includes(reference.text));
      }
      assert.deepEqual(answer.suggestions.map((s) => s.kind), ['move_task', 'open']);
      // A suggestion moves nothing.
      const pap1 = (await call<Task>(server, 'GET', '/v1/tasks/PAP-1', { token: DEVICE })).body;
      assert.equal(pap1.status, 'in_progress');
      assert.equal((await state(server)).engine, 'claude', 'remembered');
    }, FAST));

  it('types follow-ups into the live session, and a new conversation ends the old one', () =>
    withServer(async (server) => {
      const first = await ask(server, { text: 'What is blocked?', engine: 'codex' });
      assert.equal(first.status, 202);
      await answered(server);
      const follow = await ask(server, { text: '/clear\nnow', conversation: `cnv_${first.body.id}`, engine: 'claude' });
      assert.equal(follow.status, 202);
      assert.equal(follow.body.engine, 'codex', 'a follow-up keeps its engine');
      assert.equal(follow.body.turns[1]!.question, '/clear now');
      assert.equal(follow.body.turns[1]!.session, first.body.turns[0]!.session);
      const page = (await call<TranscriptPage>(server, 'GET', `/v1/sessions/${first.body.turns[0]!.session}/transcript`, { token: DEVICE })).body;
      assert.ok(page.items.some((i) => i.kind === 'user_prompt' && i.text === 'Q: /clear now'), 'typed as a question');
      await answered(server, 1);

      const second = await ask(server, { text: 'And the cluster?' });
      assert.equal(second.status, 202);
      assert.equal(second.body.engine, 'codex', 'the remembered engine');
      const old = (await call<Session>(server, 'GET', `/v1/sessions/${first.body.turns[0]!.session}`, { token: DEVICE })).body;
      assert.equal(old.state, 'ended', 'one Orchestrator session at a time');
      const now = await state(server);
      assert.equal(now.conversations[0]!.id, second.body.id, 'newest first');
      assert.equal(now.conversations[1]!.session, undefined);
    }, FAST));

  it('refuses what the contract refuses, and cancels and clears', () =>
    withServer(async (server) => {
      for (const json of [{ text: ' \u0007 ' }, { text: 'x'.repeat(4001) }, {}, [1], { text: 'Hi', engine: 'gpt' }, { text: 'Hi', conversation: 'nope' }]) {
        assert.equal((await ask(server, json)).status, 400, JSON.stringify(json).slice(0, 40));
      }
      assert.equal((await ask(server, { text: 'Hi', conversation: '01J00000000000000000000000' })).status, 404);
      assert.equal((await ask(server, { text: 'Hi', engine: 'opencode' })).status, 409, 'not installed');
      assert.equal((await ask(server, { text: 'Hi', agent: ID.sam })).status, 400, 'a person');
      assert.equal((await ask(server, { text: 'Hi' }, SECOND)).status, 400, 'no back office');
      assert.equal((await ask(server, { text: 'Hi', agent: OFFICE }, SECOND)).status, 403, 'not theirs');
      for (const token of [AGENT, READER]) {
        assert.equal((await call(server, 'GET', '/v1/orchestrator', { token })).status, 403);
        assert.equal((await ask(server, { text: 'Hi' }, token)).status, 403);
        assert.equal((await call(server, 'DELETE', '/v1/orchestrator/conversations', { token })).status, 403);
      }
      const res = await ask(server, { text: 'x'.repeat(4000) });
      assert.equal(res.status, 202);
      assert.equal((await ask(server, { text: 'Again?' })).status, 409, 'one answer at a time');
      assert.equal((await state(server, SECOND)).conversations.length, 0, 'another person sees none');
      const cancel = `/v1/orchestrator/conversations/${res.body.id}/cancel`;
      assert.equal((await call(server, 'POST', cancel, { token: SECOND })).status, 404);
      const canceled = await call<Conversation>(server, 'POST', cancel, { token: DEVICE });
      assert.equal(canceled.status, 200);
      assert.equal(canceled.body.turns[0]!.state, 'canceled');
      assert.equal((await call(server, 'POST', cancel, { token: DEVICE })).status, 409);
      await sleep(80);
      assert.equal((await state(server)).conversations[0]!.turns[0]!.answer, '', 'a canceled answer stays empty');

      const cleared = await call(server, 'DELETE', '/v1/orchestrator/conversations', { token: DEVICE });
      assert.equal(cleared.status, 204);
      const after = await state(server);
      assert.deepEqual(after.conversations, []);
      assert.equal(after.engine, 'claude');
      const session = (await call<Session>(server, 'GET', `/v1/sessions/${res.body.turns[0]!.session}`, { token: DEVICE })).body;
      assert.equal(session.state, 'ended');
    }, FAST));
});

describe('a reader token', () => {
  it('reads what is marked read or agent, and nothing else', () =>
    withServer(async (server) => {
      for (const path of ['/v1/me', '/v1/members', '/v1/workspace', '/v1/machines', '/v1/projects', '/v1/workstreams',
        `/v1/workstreams/${ID.submission}`, '/v1/tasks', '/v1/tasks/PAP-1', '/v1/sessions', `/v1/sessions/${ID.ses1}`,
        '/v1/asks', '/v1/briefs', '/v1/events?limit=5', '/v1/recaps/blocks?limit=5', `/v1/recaps/days?workstream=${ID.submission}`]) {
        assert.equal((await call(server, 'GET', path, { token: READER })).status, 200, path);
      }
      for (const path of ['/v1/me/cursors', '/v1/safety', '/v1/import', '/v1/board-drafts', `/v1/sessions/${ID.ses1}/transcript`,
        // Integrations, outward writes and machine setup: device-only reads.
        '/v1/integrations', '/v1/writes', `/v1/machines/${LAPTOP}/check`, `/v1/machines/${LAPTOP}/agents`,
        `/v1/machines/${LAPTOP}/agents/claude/sign-in`]) {
        assert.equal((await call(server, 'GET', path, { token: READER })).status, 403, path);
      }
      for (const [method, path] of [
        ['POST', '/v1/tasks/PAP-1/move'], ['PUT', '/v1/tasks/PAP-1/subtasks'], ['POST', '/v1/tasks/PAP-1/comments'],
        ['POST', '/v1/asks'], ['POST', '/v1/asks/01JB000000000000000ASK0001/answer'], ['POST', '/v1/hooks/claude/Stop'],
        ['POST', '/v1/tasks'], ['PATCH', '/v1/tasks/PAP-1'], ['POST', '/v1/projects'], ['POST', '/v1/sessions'],
        ['POST', `/v1/sessions/${ID.ses1}/send`], ['PUT', '/v1/safety'], ['DELETE', '/v1/orchestrator/conversations'],
        ['POST', '/v1/integrations'], ['POST', '/v1/writes'], ['POST', `/v1/machines/${LAPTOP}/agents/claude/sign-in`],
      ] as const) {
        const res = await call<{ code: string }>(server, method, path, { token: READER, json: {} });
        assert.equal(res.status, 403, `${method} ${path}`);
        assert.equal(res.body.code, 'forbidden');
      }
      assert.equal((await call(server, 'GET', '/v1/sessions', { token: AGENT })).status, 403, 'an agent does not');
      const pap1 = (await call<Task>(server, 'GET', '/v1/tasks/PAP-1', { token: DEVICE })).body;
      assert.equal(pap1.status, 'in_progress');
    }));
});

describe('what an answer says', () => {
  it('finds references and suggestions as the hub does, and cleans questions', () => {
    const found = scan(
      `**PAP-1** moved in ses_${ID.ses1}. See recap:wst_${ID.submission}@2026-09-30 and SHA-256.\n- Suggestion: move PAP-1 to In Progress\nSuggestion: open wst_${ID.submission}\nSuggestion: dance\n`,
    );
    assert.deepEqual(found.references.map((r) => r.text), ['PAP-1', `ses_${ID.ses1}`, `recap:wst_${ID.submission}@2026-09-30`, 'SHA-256']);
    assert.deepEqual(found.suggestions.map((s) => s.kind), ['move', 'open']);
    assert.ok(found.text.endsWith('Suggestion: dance'));
    assert.equal(cleanQuestion('  a\u202E b\r\nc\u0007\t', false), 'a b\nc');
    assert.equal(cleanQuestion('a\nb\u2028c', true), 'a b c');
    assert.equal(typed('!rm'), 'Q: !rm');
    assert.equal(typed('What?'), 'What?');
  });
});
