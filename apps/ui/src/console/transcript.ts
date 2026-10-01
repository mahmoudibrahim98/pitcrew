// The loaded part of a transcript, and the rows the chat view draws from it. No React.
//
// Pages hold whole records and the transcript only grows at the end, so a page fetched with
// `before = b` holds every record in `[page.from, b)`, and the newest page every record from
// `page.from` on (at the time). `TranscriptWindow` keeps every record it has seen and which
// ranges are complete, so a newer tail never drops what the user was reading; where two ranges
// do not meet it reports a gap, which the next fetch (`before` = the start of the range above it)
// fills.

import {
  TRANSCRIPT_KINDS,
  type Ask,
  type TranscriptItem,
  type TranscriptItemOf,
  type TranscriptPage,
} from '../data/index.ts';

const KNOWN = new Set<string>(TRANSCRIPT_KINDS);

/** A byte range in which every record is loaded. */
interface Span {
  from: number;
  to: number;
}

export interface WindowView {
  /** Every loaded item, oldest first. */
  items: readonly TranscriptItem[];
  /** Offsets of records that follow missing records. */
  gapsBefore: ReadonlySet<number>;
  /** Nothing older than the first item exists. */
  atStart: boolean;
  /** The `before` of the next fetch: into the gap nearest the tail, else older than everything. */
  nextBefore: number | undefined;
  hasGap: boolean;
  /** A newest page has arrived. */
  loaded: boolean;
}

export const EMPTY_VIEW: WindowView = {
  items: [],
  gapsBefore: new Set(),
  atStart: false,
  nextBefore: undefined,
  hasGap: false,
  loaded: false,
};

function normalize(spans: Span[]): Span[] {
  const sorted = spans.filter((s) => s.to > s.from).sort((a, b) => a.from - b.from);
  const merged: Span[] = [];
  for (const span of sorted) {
    const last = merged.at(-1);
    if (last !== undefined && span.from <= last.to) last.to = Math.max(last.to, span.to);
    else merged.push({ ...span });
  }
  return merged;
}

export class TranscriptWindow {
  #records = new Map<number, TranscriptItem[]>();
  #spans: Span[] = [];
  #tail: Span | undefined;
  /** The offset of the transcript's first record, once a page has said `at_start`. */
  #start: number | undefined;
  #view: WindowView = EMPTY_VIEW;

  get view(): WindowView {
    return this.#view;
  }

  clear(): void {
    this.#records.clear();
    this.#spans = [];
    this.#tail = undefined;
    this.#start = undefined;
    this.#view = EMPTY_VIEW;
  }

  /**
   * Merges a newest page. A page that contradicts what is loaded (a shorter transcript, or other
   * items at a known offset) means another transcript: everything else is dropped first. Returns
   * false in that case.
   */
  addTail(page: TranscriptPage): boolean {
    let consistent = true;
    if (this.#tail !== undefined && (page.to < this.#tail.to || this.#conflicts(page))) {
      this.clear();
      consistent = false;
    }
    // The previous tail stays complete up to where it ended; anything later is in this one.
    if (this.#tail !== undefined) this.#addSpan({ from: this.#tail.from, to: this.#tail.to });
    this.#tail = { from: page.from, to: page.to };
    this.#put(page);
    if (page.at_start) this.#atStart(page.from);
    this.#view = this.#compute();
    return consistent;
  }

  /** Merges an older page, fetched with `before`. Ignored until a newest page has arrived. */
  addOlder(before: number, page: TranscriptPage): void {
    if (this.#tail === undefined) return;
    this.#put(page);
    if (page.items.length > 0) this.#addSpan({ from: page.from, to: before });
    if (page.at_start) this.#atStart(page.items.length > 0 ? page.from : before);
    this.#view = this.#compute();
  }

  #addSpan(span: Span): void {
    this.#spans = normalize([...this.#spans, span]);
  }

  #atStart(offset: number): void {
    this.#start = this.#start === undefined ? offset : Math.min(this.#start, offset);
  }

  #conflicts(page: TranscriptPage): boolean {
    for (const item of page.items) {
      const known = this.#records.get(item.offset)?.[0];
      if (known !== undefined && (known.kind !== item.kind || known.at !== item.at)) return true;
    }
    return false;
  }

  #put(page: TranscriptPage): void {
    let offset: number | undefined;
    let record: TranscriptItem[] = [];
    const flush = () => {
      if (offset !== undefined && record.length > 0) this.#records.set(offset, record);
    };
    for (const item of page.items) {
      if (item.offset !== offset) {
        flush();
        offset = item.offset;
        record = [];
      }
      if (KNOWN.has(item.kind)) record.push(item);
    }
    flush();
  }

  #compute(): WindowView {
    if (this.#tail === undefined) return EMPTY_VIEW;
    const spans = normalize([...this.#spans, { from: this.#tail.from, to: Infinity }]);
    const offsets = [...this.#records.keys()].sort((a, b) => a - b);
    const items = offsets.flatMap((offset) => this.#records.get(offset) ?? []);
    const gapsBefore = new Set<number>();
    for (const span of spans.slice(1)) {
      const first = offsets.find((offset) => offset >= span.from);
      if (first !== undefined) gapsBefore.add(first);
    }
    const lowest = spans[0]?.from ?? this.#tail.from;
    const atStart = this.#start !== undefined && lowest <= this.#start;
    const hasGap = spans.length > 1;
    const tailSpan = spans.at(-1);
    return {
      items,
      gapsBefore,
      atStart,
      nextBefore: hasGap ? tailSpan?.from : atStart ? undefined : lowest,
      hasGap,
      loaded: true,
    };
  }
}

// ─── Rows ───────────────────────────────────────────────────────────────────────────────────────

export interface PendingPrompt {
  id: string;
  text: string;
  sentAt: number;
}

export type ChatRow =
  | { type: 'start'; key: string }
  | { type: 'gap'; key: string; before: number }
  | { type: 'prompt'; key: string; item: TranscriptItemOf<'user_prompt'> }
  | { type: 'pending'; key: string; prompt: PendingPrompt }
  | { type: 'text'; key: string; item: TranscriptItemOf<'assistant_text'> }
  | {
      type: 'tool';
      key: string;
      use: TranscriptItemOf<'tool_use'> | undefined;
      result: TranscriptItemOf<'tool_result'> | undefined;
    }
  | { type: 'edit'; key: string; item: TranscriptItemOf<'file_edit'> }
  | { type: 'plan'; key: string; item: TranscriptItemOf<'plan_updated'> }
  | {
      type: 'question';
      key: string;
      item: TranscriptItemOf<'question'>;
      /** The answer the transcript records, if any. */
      answer: string | undefined;
      /** The last thing in the transcript and not answered yet. */
      open: boolean;
    }
  | { type: 'turn'; key: string; item: TranscriptItemOf<'turn_ended'> };

export type ChatRowType = ChatRow['type'];

/**
 * Turns items into rows: tool calls paired with their results by `call_id`; a question and the
 * tool call that asked it (same record) become one question row, answered by that call's result.
 */
export function buildRows(
  view: Pick<WindowView, 'items' | 'gapsBefore' | 'atStart'>,
  pending: readonly PendingPrompt[] = [],
): ChatRow[] {
  const { items } = view;
  const results = new Map<string, TranscriptItemOf<'tool_result'>>();
  const uses = new Set<string>();
  const questionOffsets = new Set<number>();
  for (const item of items) {
    if (item.kind === 'tool_result' && !results.has(item.call_id)) results.set(item.call_id, item);
    else if (item.kind === 'tool_use') uses.add(item.call_id);
    else if (item.kind === 'question') questionOffsets.add(item.offset);
  }
  // The tool call in a question's record is how the CLI asked it; its result is the answer.
  const askedBy = new Map<number, string>();
  for (const item of items) {
    if (item.kind === 'tool_use' && questionOffsets.has(item.offset) && !askedBy.has(item.offset)) {
      askedBy.set(item.offset, item.call_id);
    }
  }
  const questionCalls = new Set(askedBy.values());
  const lastOffset = items.at(-1)?.offset;

  const rows: ChatRow[] = [];
  if (view.atStart) rows.push({ type: 'start', key: 'start' });
  let offset: number | undefined;
  let index = 0;
  for (const item of items) {
    if (item.offset !== offset) {
      offset = item.offset;
      index = 0;
      if (view.gapsBefore.has(offset)) rows.push({ type: 'gap', key: `gap:${offset}`, before: offset });
    } else {
      index += 1;
    }
    const key = `${item.offset}:${index}`;
    switch (item.kind) {
      case 'user_prompt':
        rows.push({ type: 'prompt', key, item });
        break;
      case 'assistant_text':
        rows.push({ type: 'text', key, item });
        break;
      case 'tool_use':
        if (!questionCalls.has(item.call_id)) {
          rows.push({ type: 'tool', key, use: item, result: results.get(item.call_id) });
        }
        break;
      case 'tool_result':
        // Shown with its call, unless the call is not loaded (it is on an older page).
        if (!uses.has(item.call_id) && !questionCalls.has(item.call_id)) {
          rows.push({ type: 'tool', key, use: undefined, result: item });
        }
        break;
      case 'file_edit':
        rows.push({ type: 'edit', key, item });
        break;
      case 'plan_updated':
        rows.push({ type: 'plan', key, item });
        break;
      case 'question': {
        const call = askedBy.get(item.offset);
        const answer = call === undefined ? undefined : results.get(call)?.summary;
        rows.push({ type: 'question', key, item, answer, open: answer === undefined && item.offset === lastOffset });
        break;
      }
      case 'turn_ended':
        rows.push({ type: 'turn', key, item });
        break;
    }
  }
  for (const prompt of pending) rows.push({ type: 'pending', key: `pending:${prompt.id}`, prompt });
  return rows;
}

/** The ask a transcript question was raised as: one whose receipt points at the question's record. */
export function askForQuestion(
  asks: readonly Ask[] | undefined,
  sessionId: string,
  question: TranscriptItemOf<'question'>,
): Ask | undefined {
  return asks?.find(
    (ask) =>
      ask.session === sessionId &&
      ask.receipts.some(
        (r) => r.kind === 'transcript' && r.session === sessionId && r.offset === question.offset,
      ),
  );
}

/** The newest plan in the loaded items. */
export function latestPlan(items: readonly TranscriptItem[]): TranscriptItemOf<'plan_updated'> | undefined {
  for (let i = items.length - 1; i >= 0; i--) {
    const item = items[i];
    if (item?.kind === 'plan_updated') return item;
  }
  return undefined;
}

/**
 * Prompts sent from this window that the transcript already shows. A prompt matches a
 * `user_prompt` with the same text written no earlier than two minutes before it was sent (clock
 * skew between machines), each item matching one prompt.
 */
export function deliveredPrompts(
  pending: readonly PendingPrompt[],
  items: readonly TranscriptItem[],
): Set<string> {
  const delivered = new Set<string>();
  const used = new Set<TranscriptItem>();
  for (const prompt of pending) {
    const text = prompt.text.trim();
    const match = items.find(
      (item) =>
        item.kind === 'user_prompt' &&
        !used.has(item) &&
        item.text.trim() === text &&
        item.at >= prompt.sentAt - 120_000,
    );
    if (match !== undefined) {
      used.add(match);
      delivered.add(prompt.id);
    }
  }
  return delivered;
}
