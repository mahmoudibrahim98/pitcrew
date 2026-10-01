import assert from 'node:assert/strict';
import { describe, it } from 'node:test';
import { daysPage } from '../src/recaps.ts';
import type { RunningServer } from '../src/server.ts';
import type { ApiError, BlocksPage, DayRecap, DaysPage, DemoRecaps, Span, Summary } from '../src/types.ts';
import { AGENT, DEVICE, ID, call, withServer } from './helpers.ts';

const SEED_RUNS = '01JB000000000000000WST0002';
const ABLATION = '01JB000000000000000WST0004';
const PAP5 = '01JB000000000000000TSK0005';

/** The last four characters of an id, e.g. `0014` for `…EVT0014`. */
const short = (id: string): string => id.slice(-4);

/** A clause of a summary: spans are UTF-8 byte ranges, not string indices. */
const clause = (summary: Summary, span: Span): string =>
  Buffer.from(summary.text, 'utf8').subarray(span.range.start, span.range.end).toString('utf8');

async function blocks(server: RunningServer, query = ''): Promise<BlocksPage> {
  const res = await call<BlocksPage>(server, 'GET', `/v1/recaps/blocks${query}`, { token: DEVICE });
  assert.equal(res.status, 200, query);
  return res.body;
}

async function days(server: RunningServer, query: string): Promise<DaysPage> {
  const res = await call<DaysPage>(server, 'GET', `/v1/recaps/days${query}`, { token: DEVICE });
  assert.equal(res.status, 200, query);
  return res.body;
}

const ids = (page: BlocksPage): string[] => page.blocks.map((b) => short(b.block.id));
const entries = (page: DaysPage): string[] =>
  page.days.map((d) => `${d.date} ${d.workstream === undefined ? '-' : short(d.workstream)}`);

describe('recap blocks', () => {
  it('serves every block newest first, each with its line', () =>
    withServer(async (server) => {
      const page = await blocks(server);
      assert.deepEqual(ids(page), ['0014', '0013', '0011', '0010', '0009', '0008', '0007', '0004', '0002', '0001']);
      assert.equal(page.at_start, true);
      const edit = page.blocks.find((b) => short(b.block.id) === '0007');
      assert.ok(edit !== undefined);
      assert.equal(edit.line.text, '@writer edited method.tex (+84 −12)');
      assert.deepEqual(edit.block.files.map((f) => [f.path, f.added, f.removed]), [['method.tex', 84, 12]]);
      // The minus sign is three bytes in UTF-8 and one UTF-16 unit: the span ends at byte 37.
      const span = edit.line.spans[0];
      assert.ok(span !== undefined);
      assert.deepEqual(span.range, { start: 0, end: 37 });
      assert.equal(edit.line.text.length, 35);
      assert.equal(clause(edit.line, span), edit.line.text);
      assert.deepEqual(span.receipts, [{ kind: 'event', id: edit.block.id }]);
      for (const { line } of page.blocks) {
        assert.ok(line.spans.length > 0, line.text);
        for (const s of line.spans) {
          assert.ok(clause(line, s).length > 0 && s.receipts.length > 0, line.text);
        }
      }
    }));

  it('pages with an exclusive before and a limit', () =>
    withServer(async (server) => {
      const first = await blocks(server, '?limit=4');
      assert.deepEqual(ids(first), ['0014', '0013', '0011', '0010']);
      assert.equal(first.at_start, false);
      const last = first.blocks.at(-1)?.block.id;
      const second = await blocks(server, `?limit=4&before=${last}`);
      assert.deepEqual(ids(second), ['0009', '0008', '0007', '0004']);
      assert.equal(second.at_start, false);
      const third = await blocks(server, `?limit=4&before=${second.blocks.at(-1)?.block.id}`);
      assert.deepEqual(ids(third), ['0002', '0001']);
      assert.equal(third.at_start, true);
      // Exactly the rest is at the start too.
      const exact = await blocks(server, `?limit=2&before=${second.blocks.at(-1)?.block.id}`);
      assert.deepEqual([ids(exact), exact.at_start], [['0002', '0001'], true]);
      // Any well-formed id is a cursor: EVT0012 is the last event of block 0011, not a block.
      const cursor = await blocks(server, '?limit=1&before=01JB000000000000000EVT0012');
      assert.deepEqual(ids(cursor), ['0011']);
      const prefixed = await blocks(server, '?limit=1&before=evt_01jb000000000000000evt0012');
      assert.deepEqual(ids(prefixed), ['0011']);
      const none = await blocks(server, '?before=01JB000000000000000EVT0001');
      assert.deepEqual(none, { blocks: [], at_start: true });
      // A limit over the maximum counts as the maximum.
      assert.equal((await blocks(server, '?limit=100000')).blocks.length, 10);
    }));

  it('filters by the links of each block, combined', () =>
    withServer(async (server) => {
      assert.deepEqual(ids(await blocks(server, `?session=${ID.ses2}`)), ['0010', '0009', '0001']);
      // PAP-1's blocks are its session's: the dispatch, move and plan, then the edit.
      assert.deepEqual(ids(await blocks(server, `?task=${ID.pap1}`)), ['0007', '0004']);
      // PAP-5 is named only by the ask raised in block 0010.
      assert.deepEqual(ids(await blocks(server, `?task=${PAP5}`)), ['0010']);
      assert.deepEqual(ids(await blocks(server, `?workstream=${SEED_RUNS}`)), ['0011', '0010', '0009', '0001']);
      assert.deepEqual(ids(await blocks(server, `?project=${ID.tooling}`)), ['0014', '0013', '0008']);
      assert.deepEqual(ids(await blocks(server, `?project=${ID.paper}&session=${ID.ses2}&limit=2`)), ['0010', '0009']);
      assert.deepEqual(await blocks(server, `?project=${ID.tooling}&session=${ID.ses2}`), {
        blocks: [],
        at_start: true,
      });
      // Prefixed and lower-case ids are the same id.
      assert.deepEqual(ids(await blocks(server, `?session=ses_${ID.ses2.toLowerCase()}`)), ['0010', '0009', '0001']);
      // An unknown id is an empty page, as in the activity route.
      assert.deepEqual(await blocks(server, `?workstream=${ABLATION}`), { blocks: [], at_start: true });
      assert.deepEqual(await blocks(server, '?task=01JB000000000000000TSK0099'), { blocks: [], at_start: true });
    }));

  it('answers 400 for a malformed id or limit', () =>
    withServer(async (server) => {
      for (const query of [
        '?session=nope',
        '?task=PAP-1',
        `?workstream=wst_${ID.paper.slice(1)}`,
        '?project=prj_',
        '?before=8ZZZZZZZZZZZZZZZZZZZZZZZZZ',
        '?before=1790761920000',
        '?limit=0',
        '?limit=-1',
        '?limit=ten',
      ]) {
        const res = await call<ApiError>(server, 'GET', `/v1/recaps/blocks${query}`, { token: DEVICE });
        assert.equal(res.status, 400, query);
        assert.equal(res.body.code, 'invalid', query);
      }
    }));
});

describe('recap days', () => {
  it("serves a project's days newest first, workstreams by id within a date", () =>
    withServer(async (server) => {
      const paper = await days(server, `?project=${ID.paper}`);
      assert.deepEqual(entries(paper), [
        '2026-09-30 0001',
        '2026-09-30 0002',
        '2026-09-29 0001',
        '2026-09-29 0002',
      ]);
      assert.equal(paper.at_start, true);
      const seeds = paper.days[1];
      assert.ok(seeds !== undefined);
      assert.deepEqual(seeds.blocks.map(short), ['0009', '0010', '0011']);
      assert.ok(seeds.summary.text.startsWith('3 bursts of work, 1 tool run, 1 ask raised.'), seeds.summary.text);
      for (const day of paper.days) {
        for (const span of day.summary.spans) {
          assert.ok(clause(day.summary, span).length > 0 && span.receipts.length > 0, day.summary.text);
        }
      }
      // Each entry's blocks are blocks of its workstream that the blocks route serves.
      const all = await blocks(server, `?project=${ID.paper}`);
      for (const day of paper.days) {
        for (const id of day.blocks) {
          assert.equal(all.blocks.find((b) => b.block.id === id)?.block.workstream, day.workstream);
        }
      }
      assert.deepEqual(entries(await days(server, `?project=${ID.tooling}`)), ['2026-09-30 0003']);
    }));

  it("serves a workstream's own entries, and nothing for a quiet or unknown one", () =>
    withServer(async (server) => {
      const seeds = await days(server, `?workstream=${SEED_RUNS}`);
      assert.deepEqual(entries(seeds), ['2026-09-30 0002', '2026-09-29 0002']);
      const paper = await days(server, `?project=${ID.paper}`);
      assert.deepEqual(
        seeds.days,
        paper.days.filter((d) => d.workstream === SEED_RUNS),
      );
      assert.deepEqual(await days(server, `?workstream=${ABLATION}`), { days: [], at_start: true });
      assert.deepEqual(await days(server, '?project=prj_01JB000000000000000PRJ0099'), { days: [], at_start: true });
    }));

  it('pages whole dates with an exclusive before and a limit in days', () =>
    withServer(async (server) => {
      const first = await days(server, `?project=${ID.paper}&limit=1`);
      assert.deepEqual(entries(first), ['2026-09-30 0001', '2026-09-30 0002']);
      assert.equal(first.at_start, false);
      const second = await days(server, `?project=${ID.paper}&limit=1&before=${first.days.at(-1)?.date}`);
      assert.deepEqual(entries(second), ['2026-09-29 0001', '2026-09-29 0002']);
      assert.equal(second.at_start, true);
      assert.deepEqual(await days(server, `?project=${ID.paper}&before=2026-09-29`), { days: [], at_start: true });
      assert.equal((await days(server, `?project=${ID.paper}&before=2026-10-01&limit=31`)).days.length, 4);
    }));

  it('serves only tz=0, and checks tz', () =>
    withServer(async (server) => {
      const utc = await days(server, `?workstream=${SEED_RUNS}&tz=0`);
      assert.deepEqual(utc, await days(server, `?workstream=${SEED_RUNS}`));
      assert.deepEqual(utc, await days(server, `?workstream=${SEED_RUNS}&tz=-0`));
      for (const tz of ['60', '-300', '840', '-840']) {
        const res = await call<ApiError>(server, 'GET', `/v1/recaps/days?workstream=${SEED_RUNS}&tz=${tz}`, {
          token: DEVICE,
        });
        assert.equal(res.status, 400, tz);
        assert.match(res.body.message, /tz=0 only/, tz);
      }
      for (const tz of ['841', '-841', '1.5', 'UTC', '%2B60', '60m']) {
        const res = await call<ApiError>(server, 'GET', `/v1/recaps/days?workstream=${SEED_RUNS}&tz=${tz}`, {
          token: DEVICE,
        });
        assert.equal(res.status, 400, tz);
        assert.match(res.body.message, /tz must be whole minutes/, tz);
      }
    }));

  it('answers 400 for a missing or doubled scope, a malformed id, date or limit', () =>
    withServer(async (server) => {
      for (const query of [
        '',
        '?tz=0',
        `?workstream=${SEED_RUNS}&project=${ID.paper}`,
        '?workstream=Seed%20runs',
        '?project=PAP',
        `?project=${ID.paper}&before=2026-13-01`,
        `?project=${ID.paper}&before=2026-9-30`,
        `?project=${ID.paper}&before=yesterday`,
        `?project=${ID.paper}&limit=0`,
      ]) {
        const res = await call<ApiError>(server, 'GET', `/v1/recaps/days${query}`, { token: DEVICE });
        assert.equal(res.status, 400, query);
        assert.equal(res.body.code, 'invalid', query);
      }
    }));

  it('orders the entry without a workstream first within a date', () => {
    const summary = { text: 'x', spans: [] };
    const day = (date: string, workstream?: string): DayRecap =>
      workstream === undefined ? { date, blocks: [], summary } : { workstream, date, blocks: [], summary };
    const recaps: DemoRecaps = {
      tz: 0,
      blocks: [],
      projects: [
        {
          project: ID.paper,
          // The engine's order: by date, then the one without a workstream, then by id.
          days: [
            day('2026-09-29', SEED_RUNS),
            day('2026-09-30'),
            day('2026-09-30', ID.submission),
            day('2026-09-30', SEED_RUNS),
          ],
        },
      ],
    };
    const page = daysPage(recaps, new URLSearchParams(`project=${ID.paper}`));
    assert.deepEqual(entries(page), ['2026-09-30 -', '2026-09-30 0001', '2026-09-30 0002', '2026-09-29 0002']);
    const wire = JSON.parse(JSON.stringify(page)) as DaysPage;
    assert.equal(Object.hasOwn(wire.days[0] ?? {}, 'workstream'), false, 'absent, not null');
  });
});

describe('recap access', () => {
  it('needs a device token', () =>
    withServer(async (server) => {
      for (const path of ['/v1/recaps/blocks', `/v1/recaps/days?project=${ID.paper}`]) {
        const agent = await call<ApiError>(server, 'GET', path, { token: AGENT });
        assert.equal(agent.status, 403, path);
        assert.equal(agent.body.code, 'forbidden', path);
        const none = await call<ApiError>(server, 'GET', path);
        assert.equal(none.status, 401, path);
      }
    }));

  it('serves the fixture whatever changes are made through the mock', () =>
    withServer(async (server) => {
      const before = await blocks(server);
      const moved = await call(server, 'POST', '/v1/tasks/PAP-2/move', { token: DEVICE, json: { to: 'in_progress' } });
      assert.equal(moved.status, 200);
      assert.deepEqual(await blocks(server), before);
    }));
});
