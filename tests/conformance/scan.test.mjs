// API v1 "Machine scan": `POST /v1/machines/{id}/scan`, against both targets. Its own file, so the
// shared cases in api.test.mjs stay as they are. The concurrent case needs a scan that lasts a
// moment: the mock's takes about a second, and run.mjs starts the daemon with `--scan-hold-ms`.
import assert from 'node:assert/strict';
import { before, test } from 'node:test';
import { bool, enumeration, integer, list, object, schemas, tagged, text } from './schema.mjs';

const base = process.env.PITCREW_CONFORMANCE_URL;
const person = process.env.PITCREW_CONFORMANCE_PERSON;
const agent = process.env.PITCREW_CONFORMANCE_AGENT;
assert.ok(base && person && agent, 'Set PITCREW_CONFORMANCE_URL, _PERSON and _AGENT');
assert.ok(
  ['127.0.0.1', 'localhost', '[::1]'].includes(new URL(base).hostname),
  'Conformance changes synthetic local state only',
);
const missing = '01J00000000000000000000000';
const codes = { 401: 'unauthorized', 403: 'forbidden', 404: 'not_found', 409: 'conflict' };

const engine = enumeration('claude', 'codex', 'opencode');
const workstreamSuggestion = object({
  id: text,
  name: text,
  'branch?': text,
  session_count: integer,
  recent_30d: integer,
  recent_90d: integer,
});
const suggestion = object({
  id: text,
  name: text,
  path: text,
  is_git: bool,
  session_count: integer,
  recent_30d: integer,
  recent_90d: integer,
  workstreams: list(workstreamSuggestion),
});
const month = (v) => {
  text(v);
  assert.match(v, /^\d{4}-\d{2}$/);
};
const report = object({
  counts: object({
    sessions: integer,
    subagent_sessions: integer,
    by_engine: list(object({ engine, count: integer })),
    by_home: list(object({ engine, home: text, count: integer })),
    by_folder: list(object({ path: text, count: integer })),
    by_month: list(object({ month, count: integer })),
    'first_activity?': integer,
    'last_activity?': integer,
  }),
  suggestions: list(suggestion),
  unreadable: integer,
  'partial?': (v) => assert.equal(typeof v, 'boolean'),
});
const frame = tagged('type', {
  progress: object({ scanned: integer, 'total?': integer, 'path?': text }),
  done: object({ report }),
  error: schemas.error,
});

/** What the contract promises of any report, beyond its shape. */
function consistent(r) {
  const sum = (items) => items.reduce((n, item) => n + item.count, 0);
  for (const key of ['by_engine', 'by_home'])
    assert.equal(sum(r.counts[key]), r.counts.sessions, `${key} adds up to sessions`);
  for (const key of ['by_folder', 'by_month'])
    assert.ok(sum(r.counts[key]) <= r.counts.sessions, `${key} excludes missing facts`);
  const folders = r.counts.by_folder.map((f) => f.count);
  assert.deepEqual(folders, [...folders].sort((a, b) => b - a), 'by_folder is busiest first');
  const months = r.counts.by_month.map((m) => m.month);
  assert.deepEqual(months, [...months].sort().reverse(), 'by_month is most recent first');
  for (const s of r.suggestions) {
    assert.equal(s.id, s.path);
    assert.ok(s.recent_30d <= s.recent_90d && s.recent_90d <= s.session_count);
    const ids = s.workstreams.map((w) => w.id);
    assert.equal(new Set(ids).size, ids.length, 'workstream ids are unique in a project');
    for (const w of s.workstreams) {
      assert.ok(w.recent_30d <= w.recent_90d && w.recent_90d <= w.session_count);
      assert.ok(w.session_count <= s.session_count);
    }
  }
}

const path = (machine) => `/v1/machines/${machine}/scan`;

function post(machine, token = person, signal = AbortSignal.timeout(15000)) {
  return fetch(new URL(path(machine), base), {
    method: 'POST',
    headers: token ? { Authorization: `Bearer ${token}` } : {},
    signal,
  });
}

/** Reads a scan's frames one at a time, as they arrive. */
function frames(response) {
  const reader = response.body.getReader();
  const decoder = new TextDecoder();
  let buffer = '';
  let ended = false;
  return {
    async next() {
      for (;;) {
        const at = buffer.indexOf('\n');
        if (at !== -1) {
          const line = buffer.slice(0, at);
          buffer = buffer.slice(at + 1);
          const parsed = JSON.parse(line);
          frame(parsed);
          return parsed;
        }
        if (ended) {
          assert.equal(buffer, '', 'every frame ends its line');
          return undefined;
        }
        const { done, value } = await reader.read();
        if (done) ended = true;
        else buffer += decoder.decode(value, { stream: true });
      }
    },
    async rest() {
      const all = [];
      for (let f = await this.next(); f !== undefined; f = await this.next()) all.push(f);
      return all;
    },
  };
}

/** A refusal, with its `ApiError` body. */
async function refused(response, status) {
  const body = await response.json();
  assert.equal(response.status, status, JSON.stringify(body));
  schemas.error(body);
  assert.equal(body.code, codes[status]);
  assert.ok(body.message.length);
  return body;
}

/** The frames after the first, which must end with the one report. */
function reportOf(first, later) {
  assert.deepEqual(first, { type: 'progress', scanned: 0 });
  const last = later.at(-1);
  assert.equal(last?.type, 'done', JSON.stringify(later));
  const ticks = later.slice(0, -1);
  assert.ok(ticks.length >= 1, 'the walk ticks at least once');
  assert.ok(ticks.every((f) => f.type === 'progress'));
  const scanned = ticks.map((f) => f.scanned);
  assert.deepEqual(scanned, [...scanned].sort((a, b) => a - b), 'progress only grows');
  assert.equal(ticks.at(-1).scanned, ticks.at(-1).total, 'the last tick has scanned == total');
  consistent(last.report);
  return last.report;
}

let own, other;
before(async () => {
  const response = await fetch(new URL('/v1/machines', base), {
    headers: { Authorization: `Bearer ${person}` },
    signal: AbortSignal.timeout(5000),
  });
  const machines = await response.json();
  own = machines.find((m) => m.kind === 'local');
  other = machines.find((m) => m.id !== own?.id);
  assert.ok(own && other, 'Use a seeded demo server: a local machine and another one');
});

test('scan no auth', { timeout: 30000 }, async () => {
  await refused(await post(own.id, null), 401);
  await refused(await post(own.id, 'synthetic-invalid-token'), 401);
  const query = await fetch(new URL(`${path(own.id)}?token=synthetic-invalid-token`, base), {
    method: 'POST',
    signal: AbortSignal.timeout(5000),
  });
  await refused(query, 401);
});

test('scan agent forbidden', { timeout: 30000 }, async () => {
  await refused(await post(own.id, agent), 403);
});

test('scan unknown machine', { timeout: 30000 }, async () => {
  await refused(await post(missing), 404);
});

test('scan another machine conflict', { timeout: 30000 }, async () => {
  await refused(await post(other.id), 409);
});

test('scan streams progress then the report', { timeout: 30000 }, async () => {
  const response = await post(own.id);
  assert.equal(response.status, 200);
  assert.match(response.headers.get('content-type') ?? '', /^application\/x-ndjson\b/);
  const scan = frames(response);
  const first = await scan.next();
  reportOf(first, await scan.rest());
});

test('scan concurrent conflict', { timeout: 30000 }, async () => {
  const response = await post(own.id);
  assert.equal(response.status, 200);
  const scan = frames(response);
  const first = await scan.next();
  // The first scan is under way: a second one now is refused, and the first is unharmed.
  const second = await refused(await post(own.id), 409);
  assert.ok(second.message.length);
  reportOf(first, await scan.rest());
});
