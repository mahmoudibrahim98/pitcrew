// `POST /v1/machines/{id}/scan` (api-v1.md, "Machine scan"): the hub's own machine only, one scan
// at a time, and an answer of newline-delimited frames ending with the report.

import assert from 'node:assert/strict';
import { describe, it } from 'node:test';
import { scanReport } from '../src/scan.ts';
import type { RunningServer } from '../src/server.ts';
import type { ApiError, ScanFrame, ScanReport } from '../src/types.ts';
import { AGENT, DEVICE, call, sleep, withServer } from './helpers.ts';

/** The demo's own machine ("This laptop") and a machine it cannot scan (the cluster). */
const LAPTOP = '01JB000000000000000MCH0001';
const CLUSTER = '01JB000000000000000MCH0002';

const FAST = { delays: { scan: 40 } };

function scanPath(machine: string): string {
  return `/v1/machines/${machine}/scan`;
}

/** A whole scan: its status, content type and frames. */
async function scan(
  server: RunningServer,
  machine = LAPTOP,
  token = DEVICE,
): Promise<{ status: number; type: string | null; frames: ScanFrame[]; error?: ApiError }> {
  const res = await fetch(server.url + scanPath(machine), {
    method: 'POST',
    headers: { authorization: `Bearer ${token}` },
  });
  const text = await res.text();
  if (res.status !== 200) {
    return { status: res.status, type: res.headers.get('content-type'), frames: [], error: JSON.parse(text) as ApiError };
  }
  assert.ok(text.endsWith('\n'), 'every frame ends its line');
  const frames = text
    .slice(0, -1)
    .split('\n')
    .map((line) => JSON.parse(line) as ScanFrame);
  return { status: res.status, type: res.headers.get('content-type'), frames };
}

function refused(res: { status: number; error?: ApiError }, status: number, code: string): void {
  assert.equal(res.status, status);
  assert.equal(res.error?.code, code);
  assert.ok((res.error?.message ?? '').length > 0);
}

describe('the synthetic report', () => {
  it('adds up', () => {
    const report: ScanReport = scanReport();
    const { counts } = report;
    const sum = (list: { count: number }[]): number => list.reduce((n, x) => n + x.count, 0);
    assert.equal(sum(counts.by_engine), counts.sessions);
    assert.equal(sum(counts.by_home), counts.sessions);
    assert.equal(sum(counts.by_folder), counts.sessions);
    assert.equal(sum(counts.by_month), counts.sessions);
    assert.equal(
      report.suggestions.reduce((n, s) => n + s.session_count, 0),
      counts.sessions,
      'every session is in one project',
    );
    const ids = report.suggestions.flatMap((s) => [s.id, ...s.workstreams.map((w) => w.id)]);
    assert.equal(new Set(ids).size, ids.length, 'ids are unique');
    for (const s of report.suggestions) {
      assert.equal(s.id, s.path);
      for (const w of s.workstreams) {
        assert.ok(w.session_count <= s.session_count);
        assert.equal(w.id, w.branch === undefined ? `${s.path}/${w.name}` : `${s.path}#${w.branch}`);
      }
    }
    const busiest = counts.by_folder.map((f) => f.count);
    assert.deepEqual(busiest, [...busiest].sort((a, b) => b - a), 'by_folder is busiest first');
    const months = counts.by_month.map((m) => m.month);
    assert.deepEqual(months, [...months].sort().reverse(), 'by_month is most recent first');
    // A fresh copy each time.
    report.suggestions.length = 0;
    assert.equal(scanReport().suggestions.length, 3);
  });
});

describe('POST /v1/machines/{id}/scan', () => {
  it('streams progress, then the report, for the hub’s own machine', () =>
    withServer(async (server) => {
      const res = await scan(server);
      assert.equal(res.status, 200);
      assert.equal(res.type, 'application/x-ndjson');
      const [first, ...rest] = res.frames;
      assert.deepEqual(first, { type: 'progress', scanned: 0 });
      const last = rest.at(-1);
      assert.equal(last?.type, 'done');
      const progress = rest.filter((f) => f.type === 'progress');
      assert.ok(progress.length > 0);
      const lastTick = progress.at(-1);
      assert.ok(lastTick?.type === 'progress' && lastTick.scanned === lastTick.total);
      const scanned = progress.map((f) => (f.type === 'progress' ? f.scanned : -1));
      assert.deepEqual(scanned, [...scanned].sort((a, b) => a - b), 'progress only grows');
      assert.equal(rest.filter((f) => f.type !== 'progress').length, 1, 'one last frame');
      assert.deepEqual(last.type === 'done' ? last.report : undefined, scanReport());
    }, FAST));

  it('refuses a second scan while one runs, even when its client went away', () =>
    withServer(async (server) => {
      const first = new AbortController();
      const running = await fetch(server.url + scanPath(LAPTOP), {
        method: 'POST',
        headers: { authorization: `Bearer ${DEVICE}` },
        signal: first.signal,
      });
      assert.equal(running.status, 200);
      refused(await scan(server), 409, 'conflict');
      first.abort();
      await running.body?.cancel().catch(() => undefined);
      // The walk goes on to its end: still taken.
      refused(await scan(server), 409, 'conflict');
      await sleep(300);
      const again = await scan(server);
      assert.equal(again.status, 200);
      assert.equal(again.frames.at(-1)?.type, 'done');
    }, { delays: { scan: 200 } }));

  it('is a person’s route: 401 without a token, 403 for an agent', () =>
    withServer(async (server) => {
      const none = await call<ApiError>(server, 'POST', scanPath(LAPTOP));
      assert.equal(none.status, 401);
      assert.equal(none.body.code, 'unauthorized');
      refused(await scan(server, LAPTOP, AGENT), 403, 'forbidden');
    }, FAST));

  it('answers 404 for an unknown machine and 409 for one it cannot scan', () =>
    withServer(async (server) => {
      refused(await scan(server, '01J00000000000000000000000'), 404, 'not_found');
      refused(await scan(server, 'not-an-id'), 404, 'not_found');
      const cluster = await scan(server, CLUSTER);
      refused(cluster, 409, 'conflict');
      assert.match(cluster.error?.message ?? '', /not supported yet/);
    }, FAST));

  it('has no machine to scan before setup, and the new one after', () =>
    withServer(async (server) => {
      refused(await scan(server, LAPTOP), 404, 'not_found');
      const setup = await call<{ machine: { id: string } }>(server, 'POST', '/v1/setup', {
        token: DEVICE,
        json: { workspace_name: 'Demo Lab', person: { name: 'Sam Rivera', handle: '@sam' }, machine_name: 'This laptop' },
      });
      assert.equal(setup.status, 200);
      const res = await scan(server, setup.body.machine.id);
      assert.equal(res.status, 200);
      assert.equal(res.frames.at(-1)?.type, 'done');
    }, { ...FAST, fresh: true }));
});
