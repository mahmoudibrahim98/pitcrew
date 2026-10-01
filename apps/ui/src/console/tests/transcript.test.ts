/// <reference types="node" />
// The window and rows against the mock hub's own paging, so the semantics match the API's.

import { describe, expect, it } from 'vitest';
import {
  appendRecord,
  assistantText,
  cannedTranscripts,
  transcriptPage,
  userPrompt,
  type TranscriptRecord,
} from '../../../../mock-hub/src/transcripts.ts';
import type { Ask } from '../../data/index.ts';
import {
  askForQuestion,
  buildRows,
  deliveredPrompts,
  TranscriptWindow,
  type ChatRow,
} from '../transcript.ts';
import type { TranscriptItem, TranscriptPage } from '../../data/index.ts';

const SES1 = '01JB000000000000000SES0001';
const SES3 = '01JB000000000000000SES0003';

/** A transcript of `n` one-item records: prompt 0, reply 1, prompt 2, … */
function numbered(n: number): TranscriptRecord[] {
  const records: TranscriptRecord[] = [];
  for (let i = 0; i < n; i++) {
    appendRecord(records, [i % 2 === 0 ? userPrompt(i, `prompt ${i}`) : assistantText(i, `reply ${i}`)]);
  }
  return records;
}

const page = (records: TranscriptRecord[], before: number | undefined, limit: number) =>
  transcriptPage(records, before, limit) as TranscriptPage;

const texts = (items: readonly TranscriptItem[]) =>
  items.map((item) => ('text' in item ? item.text : item.kind));

describe('TranscriptWindow', () => {
  it('shows the newest page, then older pages in order until the start', () => {
    const records = numbered(10);
    const window = new TranscriptWindow();
    window.addTail(page(records, undefined, 4));
    expect(texts(window.view.items)).toEqual(['prompt 6', 'reply 7', 'prompt 8', 'reply 9']);
    expect(window.view.atStart).toBe(false);

    for (let guard = 0; !window.view.atStart && guard < 10; guard++) {
      const before = window.view.nextBefore;
      if (before === undefined) throw new Error('no next page');
      window.addOlder(before, page(records, before, 4));
    }
    expect(window.view.items).toHaveLength(10);
    expect(texts(window.view.items)[0]).toBe('prompt 0');
    expect(window.view.nextBefore).toBeUndefined();
    expect(window.view.hasGap).toBe(false);
  });

  it('keeps what was loaded when the tail moves on', () => {
    const records = numbered(6);
    const window = new TranscriptWindow();
    window.addTail(page(records, undefined, 4));
    appendRecord(records, [userPrompt(6, 'prompt 6')]);
    appendRecord(records, [assistantText(7, 'reply 7')]);
    window.addTail(page(records, undefined, 4));
    expect(texts(window.view.items)).toEqual(['prompt 2', 'reply 3', 'prompt 4', 'reply 5', 'prompt 6', 'reply 7']);
    expect(window.view.hasGap).toBe(false);
  });

  it('reports a gap when more than a page arrived at once, and fills it', () => {
    const records = numbered(4);
    const window = new TranscriptWindow();
    window.addTail(page(records, undefined, 2));
    for (let i = 4; i < 12; i++) appendRecord(records, [assistantText(i, `reply ${i}`)]);
    window.addTail(page(records, undefined, 2));
    expect(window.view.hasGap).toBe(true);
    const tailFrom = records[10]?.offset;
    expect(window.view.nextBefore).toBe(tailFrom);
    expect(window.view.gapsBefore.has(tailFrom ?? -1)).toBe(true);

    while (window.view.hasGap) {
      const before = window.view.nextBefore ?? 0;
      window.addOlder(before, page(records, before, 2));
    }
    expect(texts(window.view.items)).toEqual([
      'prompt 2',
      'reply 3',
      ...Array.from({ length: 8 }, (_, k) => `reply ${k + 4}`),
    ]);
  });

  it('starts over when the transcript is another one', () => {
    const window = new TranscriptWindow();
    window.addTail(page(numbered(6), undefined, 3));
    const other: TranscriptRecord[] = [];
    appendRecord(other, [userPrompt(99, 'fresh')]);
    expect(window.addTail(page(other, undefined, 3))).toBe(false);
    expect(texts(window.view.items)).toEqual(['fresh']);
    expect(window.view.atStart).toBe(true);
  });

  it('pages the Claude fixture (SES0001) by whole records', () => {
    const records = cannedTranscripts().get(SES1) ?? [];
    const window = new TranscriptWindow();
    window.addTail(page(records, undefined, 3));
    while (!window.view.atStart) {
      const before = window.view.nextBefore ?? 0;
      window.addOlder(before, page(records, before, 3));
    }
    expect(window.view.items).toEqual(records.flatMap((r) => r.items));
  });
});

describe('buildRows', () => {
  const view = (items: TranscriptItem[], atStart = true) => ({ items, gapsBefore: new Set<number>(), atStart });

  it('pairs tool calls with results and folds a question with the call that asked it', () => {
    const items = (cannedTranscripts().get(SES1) ?? []).flatMap((r) => r.items) as TranscriptItem[];
    const rows = buildRows(view(items));
    const types = rows.map((r) => r.type);
    expect(types[0]).toBe('start');
    for (const type of ['prompt', 'text', 'plan', 'tool', 'edit', 'question', 'turn'] as const) {
      expect(types).toContain(type);
    }
    const tools = rows.filter((r): r is Extract<ChatRow, { type: 'tool' }> => r.type === 'tool');
    expect(tools.map((t) => [t.use?.tool, t.result?.summary.slice(0, 18)])).toEqual([
      ['Read', '# Method outline\n-'],
      ['Edit', 'The file /home/sam'],
      ['Bash', 'Output written on '],
      ['Edit', undefined],
    ]);
    const question = rows.find((r) => r.type === 'question');
    expect(question).toMatchObject({ answer: 'User answered: Compare both', open: false });
    expect(new Set(rows.map((r) => r.key)).size).toBe(rows.length);
  });

  it('shows a result whose call is on an older page, and an open last question', () => {
    const items = (cannedTranscripts().get(SES3) ?? []).flatMap((r) => r.items) as TranscriptItem[];
    const rows = buildRows(view(items.slice(2), false));
    expect(rows[0]).toMatchObject({ type: 'tool', use: undefined, result: { call_id: 'toolu_rev_0001' } });
    expect(rows.at(-1)).toMatchObject({ type: 'question', open: true, answer: undefined });
  });

  it('marks gaps and appends pending prompts', () => {
    const items: TranscriptItem[] = [
      { kind: 'user_prompt', at: 1, text: 'a', offset: 0 },
      { kind: 'assistant_text', at: 2, text: 'b', offset: 500 },
    ];
    const rows = buildRows({ items, gapsBefore: new Set([500]), atStart: false }, [
      { id: 'p1', text: 'c', sentAt: 3 },
    ]);
    expect(rows.map((r) => r.type)).toEqual(['prompt', 'gap', 'text', 'pending']);
  });
});

describe('askForQuestion and deliveredPrompts', () => {
  it('finds the ask whose receipt points at the question', () => {
    const question = { kind: 'question' as const, at: 0, text: 'Merge?', options: [], offset: 48213 };
    const ask = {
      id: 'ASK1',
      session: SES3,
      receipts: [{ kind: 'transcript', session: SES3, offset: 48213 }],
    } as unknown as Ask;
    const other = { ...ask, id: 'ASK2', receipts: [{ kind: 'transcript', session: SES3, offset: 1 }] } as Ask;
    expect(askForQuestion([other, ask], SES3, question)?.id).toBe('ASK1');
    expect(askForQuestion([other], SES3, question)).toBeUndefined();
  });

  it('matches each sent prompt to one new user prompt', () => {
    const items: TranscriptItem[] = [
      { kind: 'user_prompt', at: 1_000, text: 'hi', offset: 0 },
      { kind: 'user_prompt', at: 200_000, text: 'hi ', offset: 10 },
    ];
    const delivered = deliveredPrompts(
      [
        { id: 'old', text: 'hi', sentAt: 190_000 },
        { id: 'again', text: 'hi', sentAt: 195_000 },
      ],
      items,
    );
    expect([...delivered]).toEqual(['old']);
  });
});
