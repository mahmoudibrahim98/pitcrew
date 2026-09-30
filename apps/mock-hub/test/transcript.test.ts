import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { describe, it } from 'node:test';
import type { RunningServer } from '../src/server.ts';
import type { ApiError, TranscriptItem, TranscriptPage } from '../src/types.ts';
import { DEVICE, ID, call, withServer } from './helpers.ts';

const CLAUDE_FIXTURE = new URL(
  '../../../crates/fixtures/data/transcripts/claude/demo-session.jsonl',
  import.meta.url,
);

async function page(server: RunningServer, session: string, query = ''): Promise<TranscriptPage> {
  const res = await call<TranscriptPage>(server, 'GET', `/v1/sessions/${session}/transcript${query}`, {
    token: DEVICE,
  });
  assert.equal(res.status, 200);
  return res.body;
}

/** Walks a transcript from the newest page back to the start, `limit` items at a time. */
async function walk(server: RunningServer, session: string, limit: number): Promise<TranscriptPage[]> {
  const pages: TranscriptPage[] = [];
  let before: number | undefined;
  for (;;) {
    const query = before === undefined ? `?limit=${limit}` : `?limit=${limit}&before=${before}`;
    const current = await page(server, session, query);
    for (const item of current.items) {
      assert.ok(before === undefined || item.offset < before, 'a page only holds older items');
    }
    pages.unshift(current);
    if (current.at_start) {
      return pages;
    }
    assert.ok(pages.length < 100, 'paging must end');
    before = current.from;
  }
}

describe('transcripts', () => {
  it('serves the newest page first and pages back to the start with before', () =>
    withServer(async (server) => {
      const whole = await page(server, ID.ses1);
      assert.equal(whole.at_start, true);
      assert.equal(whole.from, 0);
      assert.ok(whole.items.length > 10);

      const newest = await page(server, ID.ses1, '?limit=4');
      assert.equal(newest.at_start, false);
      assert.ok(newest.items.length <= 4 && newest.items.length > 0);
      assert.deepEqual(newest.items.at(-1), whole.items.at(-1));
      assert.equal(newest.to, whole.to);

      const previous = await page(server, ID.ses1, `?limit=4&before=${newest.from}`);
      assert.ok(previous.items.every((item) => item.offset < newest.from));
      assert.equal(previous.to <= newest.from, true);

      const pages = await walk(server, ID.ses1, 4);
      assert.equal(pages[0]?.at_start, true);
      assert.equal(pages[0]?.from, 0);
      assert.deepEqual(
        pages.flatMap((p) => p.items),
        whole.items,
      );
    }));

  it('never splits the items of one record across pages', () =>
    withServer(async (server) => {
      const whole = await page(server, ID.ses1);
      const pages = await walk(server, ID.ses1, 1);
      assert.deepEqual(
        pages.flatMap((p) => p.items),
        whole.items,
      );
      for (const current of pages) {
        const offsets = new Set(current.items.map((item) => item.offset));
        assert.equal(offsets.size, 1, 'with limit 1, each page is one record');
      }
    }));

  it('places SES0001 items at the real record offsets of the Claude fixture', () =>
    withServer(async (server) => {
      const bytes = readFileSync(CLAUDE_FIXTURE);
      const lineStarts = new Set([0]);
      bytes.forEach((byte, i) => {
        if (byte === 0x0a) {
          lineStarts.add(i + 1);
        }
      });
      const whole = await page(server, ID.ses1);
      const fromFixture = whole.items.filter((item) => item.offset < bytes.length);
      assert.ok(fromFixture.length >= 12);
      for (const item of fromFixture) {
        assert.ok(lineStarts.has(item.offset), `${item.kind} at ${item.offset} is not a record start`);
      }
      const kinds = new Set(fromFixture.map((item) => item.kind));
      for (const kind of [
        'user_prompt',
        'assistant_text',
        'plan_updated',
        'tool_use',
        'tool_result',
        'file_edit',
        'question',
        'turn_ended',
      ] as const) {
        assert.ok(kinds.has(kind), `no ${kind}`);
      }
      // Every tool result answers a tool call in the same transcript.
      const calls = new Set(whole.items.flatMap((i) => (i.kind === 'tool_use' ? [i.call_id] : [])));
      for (const item of whole.items) {
        if (item.kind === 'tool_result') {
          assert.ok(calls.has(item.call_id), `result for unknown call ${item.call_id}`);
        }
      }
    }));

  it('holds the items the demo workspace receipts point to', () =>
    withServer(async (server) => {
      const at = async (session: string, offset: number): Promise<TranscriptItem[]> =>
        (await page(server, session, '?limit=1000')).items.filter((item) => item.offset === offset);
      assert.equal((await at(ID.ses3, 48213)).some((item) => item.kind === 'question'), true);
      assert.equal((await at(ID.ses2, 118220)).length > 0, true);
      assert.equal((await at(ID.ses2, 120544)).length > 0, true);
    }));

  it('checks its query and the session', () =>
    withServer(async (server) => {
      for (const query of ['?limit=0', '?limit=abc', '?before=-1', '?before=1.5']) {
        const res = await call<ApiError>(server, 'GET', `/v1/sessions/${ID.ses1}/transcript${query}`, {
          token: DEVICE,
        });
        assert.equal(res.status, 400, query);
        assert.equal(res.body.code, 'invalid');
      }
      const capped = await page(server, ID.ses1, '?limit=5000');
      assert.equal(capped.at_start, true);
      const missing = await call<ApiError>(server, 'GET', '/v1/sessions/01JB000000000000000SES0099/transcript', {
        token: DEVICE,
      });
      assert.equal(missing.status, 404);
    }));
});
