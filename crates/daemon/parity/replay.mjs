#!/usr/bin/env node
// Parity check: replays the requests and assertions of apps/mock-hub/test/http.test.ts (and the
// first test of each route in edits.test.ts) against the mock hub and against a real pitcrewd,
// each test on a fresh server (as the mock's `withServer` does), and prints every check that
// fails on either side.
//
//   node crates/daemon/parity/replay.mjs --pitcrewd <path to pitcrewd> [--no-office] [--json <report.json>]
//
// The daemon runs `serve --demo --listen tcp:127.0.0.1:0` on a new temporary state directory per
// test, and its tokens come from `device.token` (the demo's @sam) and `demo-agent.token`
// (@writer), standing in for the mock's `dev-device-token` and `dev-agent-token`. Checks marked
// "extra" are not in the mock's tests: they restate a revision-dependent check relative to the
// server's own log, so the two servers can be compared although their logs differ in length.
//
// The mock hub has no back office. The daemon runs one by default, and after a write it may
// append its own events (the demo's asks are days old by the wall clock, so it reminds @sam of
// them), racing a check that reads the newest event. `--no-office` passes `--no-office` to the
// daemon, to compare the hub alone with the mock.
//
// Needs Node 24 (it imports the mock hub's TypeScript directly). Exits 0 when it ran, whatever
// the differences; it is a report, not a test.

import { spawn } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { request } from 'node:http';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { isDeepStrictEqual, parseArgs } from 'node:util';
import { startServer } from '../../../apps/mock-hub/src/server.ts';

const ID = {
  sam: '01JB000000000000000MEM0001',
  writer: '01JB000000000000000MEM0002',
  paper: '01JB000000000000000PRJ0001',
  tooling: '01JB000000000000000PRJ0002',
  submission: '01JB000000000000000WST0001',
  parsers: '01JB000000000000000WST0003',
  pap1: '01JB000000000000000TSK0001',
  pap4: '01JB000000000000000TSK0004',
};

// ─── Servers ────────────────────────────────────────────────────────────────────────────────────

async function startMock() {
  const server = await startServer({ port: 0 });
  return { url: server.url, device: 'dev-device-token', agent: 'dev-agent-token', close: () => server.close() };
}

function startDaemon(binary, { office }) {
  return async () => {
    const state = mkdtempSync(join(tmpdir(), 'pitcrew-parity-'));
    const dir = join(state, 'state');
    const args = ['--state-dir', dir, 'serve', '--demo', '--listen', 'tcp:127.0.0.1:0', ...(office ? [] : ['--no-office'])];
    // A script stands in for the binary when checking this file itself.
    const [command, argv] = /\.m?js$/.test(binary) ? [process.execPath, [binary, ...args]] : [binary, args];
    const child = spawn(command, argv, {
      stdio: ['ignore', 'pipe', 'pipe'],
      env: { ...process.env, PITCREW_LOG: process.env.PITCREW_LOG ?? 'warn' },
    });
    let stderr = '';
    child.stderr.on('data', (chunk) => (stderr += chunk));
    const exited = new Promise((done) => child.once('exit', done));
    const url = await new Promise((done, fail) => {
      let out = '';
      const timer = setTimeout(() => fail(new Error(`pitcrewd was not ready in 60 s:\n${stderr}`)), 60_000);
      child.stdout.on('data', (chunk) => {
        out += chunk;
        const match = /pitcrewd listening on (http:\/\/\S+)/.exec(out);
        if (match) {
          clearTimeout(timer);
          done(match[1]);
        }
      });
      child.once('exit', (code) => {
        clearTimeout(timer);
        fail(new Error(`pitcrewd exited (${code}):\n${stderr}`));
      });
    });
    const token = (name) => readFileSync(join(dir, name), 'utf8').trim();
    return {
      url,
      device: token('device.token'),
      agent: token('demo-agent.token'),
      close: async () => {
        child.kill('SIGTERM');
        await exited;
        rmSync(state, { recursive: true, force: true });
      },
    };
  };
}

// ─── A recording client and soft checks ─────────────────────────────────────────────────────────

class Run {
  constructor(server) {
    this.server = server;
    this.checks = [];
    this.calls = [];
  }

  /** As the mock tests' `call`; `token` is 'device', 'agent', or a literal token. */
  async call(method, path, options = {}) {
    const headers = { ...options.headers };
    const token = options.token === 'device' ? this.server.device : options.token === 'agent' ? this.server.agent : options.token;
    if (token !== undefined) headers.authorization = `Bearer ${token}`;
    let body = options.raw;
    if (options.json !== undefined) {
      headers['content-type'] = 'application/json';
      body = JSON.stringify(options.json);
    }
    const res = await fetch(this.server.url + path, { method, headers, body });
    const text = await res.text();
    let parsed;
    try {
      parsed = text === '' ? undefined : JSON.parse(text);
    } catch {
      parsed = text;
    }
    this.calls.push({ request: `${method} ${path}${options.token ? ` (${options.token === 'device' || options.token === 'agent' ? options.token : 'other'} token)` : ''}`, status: res.status, code: parsed?.code });
    return { status: res.status, headers: res.headers, body: parsed };
  }

  check(name, actual, expected) {
    this.checks.push({ name, actual, expected, pass: isDeepStrictEqual(actual, expected) });
  }

  ok(name, condition, actual) {
    this.checks.push({ name, actual, expected: true, pass: Boolean(condition) });
  }
}

const keys = (tasks) => (Array.isArray(tasks) ? tasks.map((t) => t.key) : tasks);

// ─── The mock's HTTP tests, as soft checks ──────────────────────────────────────────────────────

const TESTS = {
  'host info without a token': async (r) => {
    const res = await r.call('GET', '/v1/host/info');
    r.check('status', res.status, 200);
    r.check('name', res.body?.name, 'pitcrewd');
    r.ok('protocol_min <= 1 <= protocol', res.body?.protocol_min <= 1 && 1 <= res.body?.protocol, [res.body?.protocol_min, res.body?.protocol]);
    r.check('roles', res.body?.roles, ['hub', 'runner']);
    r.check('x-pitcrew-mock-hub header equals version', res.headers.get('x-pitcrew-mock-hub'), res.body?.version);
  },

  '401 without a token or with an unknown one': async (r) => {
    const none = await r.call('GET', '/v1/tasks');
    r.check('none: status', none.status, 401);
    r.check('none: code', none.body?.code, 'unauthorized');
    r.check('none: www-authenticate', none.headers.get('www-authenticate'), 'Bearer');
    const unknown = await r.call('GET', '/v1/tasks', { token: 'nope' });
    r.check('unknown: status', unknown.status, 401);
    r.check('unknown: code', unknown.body?.code, 'unauthorized');
  },

  'agents forbidden on device routes, allowed on agent routes': async (r) => {
    const machines = await r.call('GET', '/v1/machines', { token: 'agent' });
    r.check('machines: status', machines.status, 403);
    r.check('machines: code', machines.body?.code, 'forbidden');
    const create = await r.call('POST', '/v1/tasks', { token: 'agent', json: { project: ID.paper, title: 'Sneaky' } });
    r.check('create: status', create.status, 403);
    const me = await r.call('GET', '/v1/me', { token: 'agent' });
    r.check('me: status', me.status, 200);
    r.check('me: handle', me.body?.handle, '@writer');
  },

  'unknown routes are 404 ApiErrors': async (r) => {
    const res = await r.call('GET', '/v1/nowhere', { token: 'device' });
    r.check('status', res.status, 404);
    r.check('code', res.body?.code, 'not_found');
    r.check('message is text', typeof res.body?.message, 'string');
  },

  'hooks from agents; malformed ones rejected': async (r) => {
    const ok = await r.call('POST', '/v1/hooks/claude/Stop', { token: 'agent', json: { session_id: 'abc', hook_event_name: 'Stop' } });
    r.check('ok: status', ok.status, 202);
    const engine = await r.call('POST', '/v1/hooks/emacs/Stop', { token: 'agent', json: {} });
    r.check('unknown engine: status', engine.status, 400);
    const event = await r.call('POST', '/v1/hooks/codex/%3Bbad', { token: 'agent', json: {} });
    r.check('bad event name: status', event.status, 400);
    const noToken = await r.call('POST', '/v1/hooks/claude/Stop', { json: {} });
    r.check('no token: status', noToken.status, 401);
  },

  'lists tasks and applies every filter': async (r) => {
    const get = async (query) => {
      const res = await r.call('GET', `/v1/tasks${query}`, { token: 'device' });
      r.check(`${query || '(none)'}: status`, res.status, 200);
      return keys(res.body);
    };
    r.check('all: count', (await get('')).length, 10);
    r.check('project=tooling', await get(`?project=${ID.tooling}`), ['TL-1', 'TL-2', 'TL-3']);
    r.check('workstream=submission', await get(`?workstream=${ID.submission}`), ['PAP-1', 'PAP-2', 'PAP-3', 'PAP-7']);
    r.check('assignee=writer', await get(`?assignee=${ID.writer}`), ['PAP-1', 'PAP-2', 'PAP-3']);
    r.check('status=todo&status=backlog', await get('?status=todo&status=backlog'), ['PAP-2', 'PAP-5', 'PAP-6', 'TL-2']);
    r.check('project=paper&status=in_progress', await get(`?project=${ID.paper}&status=in_progress`), ['PAP-1', 'PAP-4']);
  },

  'serde defaults the fixture leaves out': async (r) => {
    const res = await r.call('GET', '/v1/tasks/PAP-6', { token: 'device' });
    r.check('description', res.body?.description, '');
    r.check('subtasks', res.body?.subtasks, []);
  },

  'unknown status values are 400': async (r) => {
    const res = await r.call('GET', '/v1/tasks?status=finished', { token: 'device' });
    r.check('status', res.status, 400);
    r.check('code', res.body?.code, 'invalid');
  },

  'an agent reads everything but writes only its own tasks': async (r) => {
    const list = await r.call('GET', '/v1/tasks', { token: 'agent' });
    r.check('list: count', list.body?.length, 10);
    const other = await r.call('GET', '/v1/tasks/PAP-4', { token: 'agent' });
    r.check('PAP-4: status', other.status, 200);
    r.check('PAP-4: key', other.body?.key, 'PAP-4');
    const writes = [
      ['move', await r.call('POST', '/v1/tasks/PAP-4/move', { token: 'agent', json: { to: 'review' } })],
      ['comment', await r.call('POST', '/v1/tasks/PAP-4/comments', { token: 'agent', json: { text: 'Looks good', mentions: [] } })],
      ['subtasks', await r.call('PUT', '/v1/tasks/PAP-4/subtasks', { token: 'agent', json: [] })],
    ];
    for (const [what, res] of writes) {
      r.check(`${what}: status`, res.status, 403);
      r.check(`${what}: code`, res.body?.code, 'forbidden');
    }
    const unchanged = await r.call('GET', '/v1/tasks/PAP-4', { token: 'device' });
    r.check('PAP-4 unchanged', unchanged.body?.status, 'in_progress');
  },

  'an agent comments on its own task (201, the event)': async (r) => {
    const res = await r.call('POST', '/v1/tasks/PAP-1/comments', { token: 'agent', json: { text: '§3.2 is drafted.', mentions: [ID.sam] } });
    r.check('status', res.status, 201);
    r.check('author', res.body?.author, ID.writer);
    r.check('on_behalf_of', res.body?.on_behalf_of, ID.sam);
    r.check('body', res.body?.body, { type: 'comment_posted', data: { task: ID.pap1, text: '§3.2 is drafted.', mentions: [ID.sam] } });
  },

  'a task by key, by id and by prefixed id': async (r) => {
    for (const ref of ['PAP-4', ID.pap4, `tsk_${ID.pap4}`]) {
      const res = await r.call('GET', `/v1/tasks/${ref}`, { token: 'device' });
      r.check(`${ref}: status`, res.status, 200);
      r.check(`${ref}: id`, res.body?.id, ID.pap4);
      r.check(`${ref}: key`, res.body?.key, 'PAP-4');
    }
    const missing = await r.call('GET', '/v1/tasks/PAP-99', { token: 'device' });
    r.check('PAP-99: status', missing.status, 404);
    r.check('PAP-99: code', missing.body?.code, 'not_found');
  },

  'an agent moves its own task forward, stamped with author and owner': async (r) => {
    const base = (await r.call('GET', '/v1/events?limit=1', { token: 'device' })).body?.to_rev;
    const res = await r.call('POST', '/v1/tasks/PAP-2/move', { token: 'agent', json: { to: 'in_progress' } });
    r.check('status', res.status, 200);
    r.check('task status', res.body?.status, 'in_progress');
    const activity = await r.call('GET', '/v1/events?limit=1', { token: 'device' });
    const [event] = activity.body?.events ?? [];
    r.check('to_rev', activity.body?.to_rev, 16);
    r.check('extra: to_rev is the log before the move + 1', activity.body?.to_rev, base + 1);
    r.check('author', event?.author, ID.writer);
    r.check('on_behalf_of', event?.on_behalf_of, ID.sam);
    r.check('body', event?.body, {
      type: 'task_moved',
      data: { task: '01JB000000000000000TSK0002', from: 'todo', to: 'in_progress', mover: { kind: 'agent', on_own_task: true } },
    });
  },

  'moves the rules do not allow are 409': async (r) => {
    const move = (ref, to, token) => r.call('POST', `/v1/tasks/${ref}/move`, { token, json: { to } });
    const done = await move('PAP-1', 'done', 'agent');
    r.check('agent to done: status', done.status, 409);
    r.check('agent to done: code', done.body?.code, 'conflict');
    r.check('agent PAP-3 to in_progress: status', (await move('PAP-3', 'in_progress', 'agent')).status, 409);
    r.check('same status: status', (await move('PAP-4', 'in_progress', 'device')).status, 409);
    const task = await r.call('GET', '/v1/tasks/PAP-1', { token: 'device' });
    r.check('PAP-1 unchanged', task.body?.status, 'in_progress');
  },

  'a person makes any move': async (r) => {
    const res = await r.call('POST', '/v1/tasks/PAP-7/move', { token: 'device', json: { to: 'todo' } });
    r.check('status', res.status, 200);
    r.check('task status', res.body?.status, 'todo');
    const activity = await r.call('GET', '/v1/events?limit=1', { token: 'device' });
    const [event] = activity.body?.events ?? [];
    r.check('author', event?.author, ID.sam);
    r.check('on_behalf_of', event?.on_behalf_of, undefined);
    r.check('type', event?.body?.type, 'task_moved');
    r.check('mover', event?.body?.data?.mover, { kind: 'person' });
  },

  'move bodies are validated': async (r) => {
    const bad = await r.call('POST', '/v1/tasks/PAP-2/move', { token: 'device', json: { to: 'finished' } });
    r.check('unknown status: status', bad.status, 400);
    r.check('unknown status: code', bad.body?.code, 'invalid');
    const empty = await r.call('POST', '/v1/tasks/PAP-2/move', { token: 'device' });
    r.check('empty body: status', empty.status, 400);
  },

  'new tasks get the next key in their project': async (r) => {
    const create = (json) => r.call('POST', '/v1/tasks', { token: 'device', json });
    const first = await create({ project: ID.paper, title: 'Write the abstract' });
    r.check('first: status', first.status, 201);
    r.check('first: key', first.body?.key, 'PAP-8');
    r.check('first: status field', first.body?.status, 'todo');
    r.check('first: priority', first.body?.priority, 'none');
    r.check('first: id length', first.body?.id?.length, 26);
    r.check('second: key', (await create({ project: ID.paper, title: 'Check references' })).body?.key, 'PAP-9');
    const tooling = await create({
      project: ID.tooling,
      workstream: ID.parsers,
      title: 'Benchmark OpenCode parsing',
      status: 'backlog',
      priority: 'low',
      labels: ['performance'],
      due: '2026-10-31',
    });
    r.check('tooling: key', tooling.body?.key, 'TL-4');
    r.check('tooling: workstream', tooling.body?.workstream, ID.parsers);
    const fetched = await r.call('GET', '/v1/tasks/TL-4', { token: 'device' });
    r.check('fetched equals created', fetched.body, tooling.body);
    const activity = await r.call('GET', '/v1/events?limit=1', { token: 'device' });
    r.check('activity: type', activity.body?.events?.[0]?.body?.type, 'task_created');
  },

  'malformed new tasks are 400': async (r) => {
    for (const json of [
      { title: 'No project' },
      { project: ID.paper },
      { project: ID.paper, title: 'x', status: 'finished' },
      { project: ID.paper, title: 'x', due: '2026-13-01' },
      { project: ID.paper, workstream: ID.parsers, title: 'Wrong project' },
      { project: '01JB000000000000000PRJ0099', title: 'Unknown project' },
    ]) {
      const res = await r.call('POST', '/v1/tasks', { token: 'device', json });
      r.check(`${JSON.stringify(json)}: status`, res.status, 400);
      r.check(`${JSON.stringify(json)}: code`, res.body?.code, 'invalid');
    }
  },

  'an open ask is answered once': async (r) => {
    const open = async () => (await r.call('GET', `/v1/asks?to=${ID.sam}&state=open`, { token: 'device' })).body?.map((a) => a.id) ?? [];
    r.check('open asks', (await open()).length, 3);
    const res = await r.call('POST', '/v1/asks/01JB000000000000000ASK0002/answer', { token: 'device', json: { option: 1 } });
    r.check('answer: status', res.status, 200);
    r.check('answer: state', res.body?.state, 'answered');
    r.check('answer: by', res.body?.answer?.by, ID.sam);
    r.check('answer: option', res.body?.answer?.option, 1);
    r.check('no longer open', (await open()).includes('01JB000000000000000ASK0002'), false);
    const again = await r.call('POST', '/v1/asks/01JB000000000000000ASK0002/answer', { token: 'device', json: { text: 'Changed my mind' } });
    r.check('again: status', again.status, 409);
    const activity = await r.call('GET', '/v1/events?limit=1', { token: 'device' });
    r.check('activity: type', activity.body?.events?.[0]?.body?.type, 'ask_answered');
  },

  'answers are validated': async (r) => {
    const answer = (json, token = 'device') => r.call('POST', '/v1/asks/01JB000000000000000ASK0001/answer', { token, json });
    r.check('option 5: status', (await answer({ option: 5 })).status, 400);
    r.check('empty: status', (await answer({})).status, 400);
    r.check('agent: status', (await answer({ option: 0 }, 'agent')).status, 403);
    const missing = await r.call('POST', '/v1/asks/01JB000000000000000ASK0099/answer', { token: 'device', json: { option: 0 } });
    r.check('unknown ask: status', missing.status, 404);
  },

  'an agent answers only questions and mentions addressed to itself': async (r) => {
    const raise = async (kind) => {
      const res = await r.call('POST', '/v1/asks', { token: 'device', json: { kind, to: ID.writer, title: `A ${kind} for @writer`, options: ['Yes', 'No'] } });
      r.check(`raise ${kind}: status`, res.status, 201);
      return res.body?.id;
    };
    const answer = (id, token) => r.call('POST', `/v1/asks/${id}/answer`, { token, json: { option: 0 } });
    const inbox = await r.call('GET', `/v1/asks?to=${ID.writer}&state=open`, { token: 'agent' });
    r.check('inbox: status', inbox.status, 200);
    r.check('inbox: count', inbox.body?.length, 0);
    const question = await raise('question');
    const answered = await answer(question, 'agent');
    r.check('question: status', answered.status, 200);
    r.check('question: by', answered.body?.answer?.by, ID.writer);
    for (const kind of ['decision', 'approval', 'review']) {
      const id = await raise(kind);
      r.check(`${kind} by agent: status`, (await answer(id, 'agent')).status, 403);
      r.check(`${kind} by device: status`, (await answer(id, 'device')).status, 200);
    }
  },

  'activity pages by revision, newest last': async (r) => {
    const get = async (query) => (await r.call('GET', `/v1/events${query}`, { token: 'device' })).body;
    const latest = await get('?limit=5');
    r.check('latest: [from, to, at_start]', [latest?.from_rev, latest?.to_rev, latest?.at_start], [11, 15, false]);
    r.check('latest: id suffixes', latest?.events?.map((e) => e.id.slice(-4)), ['0011', '0012', '0013', '0014', '0015']);
    const older = await get(`?limit=5&before=${latest?.from_rev}`);
    r.check('older: [from, to, at_start]', [older?.from_rev, older?.to_rev, older?.at_start], [6, 10, false]);
    const oldest = await get(`?limit=5&before=${older?.from_rev}`);
    r.check('oldest: [from, to, at_start]', [oldest?.from_rev, oldest?.to_rev, oldest?.at_start], [1, 5, true]);
    const empty = await get('?before=1');
    r.check('before=1', empty, { events: [], from_rev: 0, to_rev: 0, at_start: true });
    // Relative to each server's own log.
    r.check('extra: latest page is the 5 newest', latest?.to_rev - latest?.from_rev, 4);
    r.check('extra: the page before ends just below', older?.to_rev, latest?.from_rev - 1);
    r.check('extra: the newest 5 are the demo slice', latest?.events?.map((e) => e.id.slice(-4)), ['0011', '0012', '0013', '0014', '0015']);
  },

  'activity filters by what events are about, parents included': async (r) => {
    const res = await r.call('GET', `/v1/events?task=${ID.pap1}`, { token: 'device' });
    r.check('task=pap1: status', res.status, 200);
    r.check('task=pap1: types', res.body?.events?.map((e) => e.body.type), ['dispatch_started', 'task_moved', 'subtasks_replaced', 'file_edited']);
    r.check('task=pap1: [from, to, at_start]', [res.body?.from_rev, res.body?.to_rev, res.body?.at_start], [4, 7, true]);
    const newestTwo = await r.call('GET', `/v1/events?task=${ID.pap1}&limit=2`, { token: 'device' });
    r.check('task=pap1&limit=2: [from, to, at_start]', [newestTwo.body?.from_rev, newestTwo.body?.to_rev, newestTwo.body?.at_start], [6, 7, false]);
    const tooling = await r.call('GET', `/v1/events?project=${ID.tooling}`, { token: 'device' });
    r.check('project=tooling: status', tooling.status, 200);
    r.check('project=tooling: id suffixes', tooling.body?.events?.map((e) => e.id.slice(-4)), ['0008', '0013', '0014']);
  },

  // The mock's test runs with `scanWindow: 5`, an option of the mock only; this is its part that
  // holds at the default window.
  'filtered paging until at_start finds every match (default scan window)': async (r) => {
    const get = async (query) => (await r.call('GET', `/v1/events${query}`, { token: 'device' })).body;
    const seen = [];
    let page = await get(`?task=${ID.pap1}&limit=1`);
    const pages = [page];
    while (page && !page.at_start && pages.length < 20) {
      page = await get(`?task=${ID.pap1}&limit=1&before=${page.from_rev}`);
      pages.push(page);
    }
    for (const p of pages) seen.unshift(...(p?.events ?? []).map((e) => e.id.slice(-4)));
    r.check('every match', seen, ['0004', '0005', '0006', '0007']);
    r.check('only the last page is at_start', pages.map((p) => p?.at_start), [...pages.slice(1).map(() => false), true]);
    r.check('no match', await get('?task=01JB000000000000000TSK0099'), { events: [], from_rev: 0, to_rev: 0, at_start: true });
  },

  // The work-edits contract (apps/mock-hub/test/edits.test.ts): the first test of each route.
  'edits: POST /v1/projects creates a project with the defaults': async (r) => {
    const res = await r.call('POST', '/v1/projects', { token: 'device', json: { key: 'THS', name: 'Thesis' } });
    r.check('status', res.status, 201);
    r.check('body', res.body, { id: res.body?.id, key: 'THS', name: 'Thesis', status: 'in_progress', lead: ID.sam, members: [ID.sam], external: [] });
    const activity = await r.call('GET', '/v1/events?limit=1', { token: 'device' });
    r.check('event', activity.body?.events?.[0]?.body, { type: 'project_created', data: { project: res.body } });
    const task = await r.call('POST', '/v1/tasks', { token: 'device', json: { project: res.body?.id, title: 'Outline chapter 1' } });
    r.check('first task key', task.body?.key, 'THS-1');
  },

  'edits: POST /v1/workstreams creates an active, on-track workstream': async (r) => {
    const res = await r.call('POST', '/v1/workstreams', { token: 'device', json: { project: ID.paper, name: 'Figures' } });
    r.check('status', res.status, 201);
    r.check('body', res.body, { id: res.body?.id, project: ID.paper, name: 'Figures', status: 'active', health: 'on_track', locations: [], external: [] });
    const activity = await r.call('GET', '/v1/events?limit=1', { token: 'device' });
    r.check('event', activity.body?.events?.[0]?.body, { type: 'workstream_created', data: { workstream: res.body } });
  },

  'edits: PATCH /v1/tasks emits task_updated with only what changed': async (r) => {
    const res = await r.call('PATCH', '/v1/tasks/PAP-2', {
      token: 'device',
      json: { title: '  Make figure 3  ', priority: 'medium', labels: [' figures ', 'paper', 'figures'], accept_auto: false },
    });
    r.check('status', res.status, 200);
    r.check('title', res.body?.title, 'Make figure 3');
    r.check('labels', res.body?.labels, ['figures', 'paper']);
    const activity = await r.call('GET', '/v1/events?limit=1', { token: 'device' });
    r.check('event', activity.body?.events?.[0]?.body, {
      type: 'task_updated',
      data: { task: '01JB000000000000000TSK0002', patch: { title: 'Make figure 3', labels: ['figures', 'paper'] } },
    });
  },

  'edits: PUT /v1/briefs stores next, and brief_accepted carries it': async (r) => {
    const res = await r.call('PUT', `/v1/briefs/workstream/${ID.submission}`, {
      token: 'device',
      json: { text: '§3.2 is drafted.', next: 'Send it to the co-authors.', pinned: false },
    });
    r.check('status', res.status, 200);
    r.check('next', res.body?.next, 'Send it to the co-authors.');
    r.check('source', res.body?.source, 'person');
    const activity = await r.call('GET', '/v1/events?limit=1', { token: 'device' });
    r.check('event', activity.body?.events?.[0]?.body, {
      type: 'brief_accepted',
      data: { target: { kind: 'workstream', id: ID.submission }, text: '§3.2 is drafted.', next: 'Send it to the co-authors.', pinned: false },
    });
  },

  'CORS preflights for local origins only': async (r) => {
    const preflight = (origin) =>
      r.call('OPTIONS', '/v1/tasks', { headers: { origin, 'access-control-request-method': 'GET', 'access-control-request-headers': 'authorization' } });
    for (const origin of ['http://localhost:5173', 'http://127.0.0.1:1420', 'tauri://localhost', 'http://tauri.localhost', 'https://tauri.localhost']) {
      const ok = await preflight(origin);
      r.check(`${origin}: status`, ok.status, 204);
      r.check(`${origin}: allow-origin`, ok.headers.get('access-control-allow-origin'), origin);
      r.ok(`${origin}: allow-headers has authorization`, /authorization/i.test(ok.headers.get('access-control-allow-headers') ?? ''), ok.headers.get('access-control-allow-headers'));
    }
    for (const origin of ['https://evil.example', 'http://localhost.evil.example', 'https://localhost:5173']) {
      const refused = await preflight(origin);
      r.check(`${origin}: status`, refused.status, 403);
      r.check(`${origin}: allow-origin`, refused.headers.get('access-control-allow-origin'), null);
    }
  },

  'malformed JSON and bodies over 1 MiB are 400': async (r) => {
    const malformed = await r.call('POST', '/v1/tasks', { token: 'device', raw: '{"title":' });
    r.check('malformed: status', malformed.status, 400);
    const huge = await r.call('POST', '/v1/tasks', { token: 'device', json: { project: ID.paper, title: 'x'.repeat(1024 * 1024) } });
    r.check('huge: status', huge.status, 400);
    r.ok('huge: message says "larger than"', /larger than/.test(huge.body?.message ?? ''), huge.body?.message);
  },

  'requests addressed to other host names are refused': async (r) => {
    const status = await new Promise((done, fail) => {
      const req = request(`${r.server.url}/v1/host/info`, { headers: { host: 'rebind.example' } }, (res) => {
        res.resume();
        done(res.statusCode);
      });
      req.on('error', fail);
      req.end();
    });
    r.check('status', status, 403);
  },
};

// ─── Run and report ─────────────────────────────────────────────────────────────────────────────

async function runAll(start) {
  const results = {};
  for (const [name, test] of Object.entries(TESTS)) {
    const server = await start();
    const run = new Run(server);
    try {
      await test(run);
    } catch (error) {
      run.checks.push({ name: 'threw', actual: String(error), expected: 'no error', pass: false });
    } finally {
      await server.close();
    }
    results[name] = run;
  }
  return results;
}

const show = (value) => (value === undefined ? 'undefined' : JSON.stringify(value)).slice(0, 160);

async function main() {
  const { values } = parseArgs({
    options: { pitcrewd: { type: 'string' }, 'no-office': { type: 'boolean' }, json: { type: 'string' } },
  });
  if (!values.pitcrewd) {
    console.error('usage: node crates/daemon/parity/replay.mjs --pitcrewd <path> [--no-office] [--json <file>]');
    process.exit(2);
  }
  const office = !values['no-office'];
  const mock = await runAll(startMock);
  const daemon = await runAll(startDaemon(values.pitcrewd, { office }));

  const rows = [];
  let checks = 0;
  for (const name of Object.keys(TESTS)) {
    const a = mock[name].checks;
    const b = daemon[name].checks;
    const count = Math.max(a.length, b.length);
    for (let i = 0; i < count; i++) {
      checks++;
      const m = a[i];
      const d = b[i];
      if (m?.pass && d?.pass && m.name === d.name) continue;
      rows.push({
        test: name,
        check: d?.name ?? m?.name,
        expected: show(d?.expected ?? m?.expected),
        mock: m ? `${m.pass ? 'pass' : 'FAIL'} ${show(m.actual)}` : 'missing',
        pitcrewd: d ? `${d.pass ? 'pass' : 'FAIL'} ${show(d.actual)}` : 'missing',
      });
    }
    // Requests whose status differs between the servers.
    const ca = mock[name].calls;
    const cb = daemon[name].calls;
    for (let i = 0; i < Math.max(ca.length, cb.length); i++) {
      if (ca[i]?.status !== cb[i]?.status) {
        rows.push({
          test: name,
          check: `status of ${cb[i]?.request ?? ca[i]?.request}`,
          expected: 'same on both',
          mock: show(ca[i] && [ca[i].status, ca[i].code]),
          pitcrewd: show(cb[i] && [cb[i].status, cb[i].code]),
        });
      }
    }
  }
  console.log(`# Parity: mock hub vs pitcrewd (back office ${office ? 'on' : 'off'})\n`);
  console.log(`${Object.keys(TESTS).length} tests, ${checks} checks; ${rows.length} rows differ or fail.\n`);
  console.log('| Test | Check | Expected | Mock hub | pitcrewd |');
  console.log('|---|---|---|---|---|');
  for (const row of rows) {
    const cell = (s) => String(s).replaceAll('|', '\\|');
    console.log(`| ${cell(row.test)} | ${cell(row.check)} | ${cell(row.expected)} | ${cell(row.mock)} | ${cell(row.pitcrewd)} |`);
  }
  if (values.json) {
    // Checks and calls only: the servers (and their tokens) stay out of the file.
    const strip = (results) =>
      Object.fromEntries(Object.entries(results).map(([name, run]) => [name, { checks: run.checks, calls: run.calls }]));
    writeFileSync(values.json, JSON.stringify({ rows, mock: strip(mock), daemon: strip(daemon) }, null, 2));
  }
}

await main();
