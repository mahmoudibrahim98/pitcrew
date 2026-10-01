// @vitest-environment happy-dom
// @vitest-environment-options {"url": "http://localhost:5173/"}

import { fireEvent, screen, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import {
  appendRecord,
  assistantText,
  cannedTranscripts,
  transcriptPage,
  userPrompt,
  type ItemDraft,
  type TranscriptRecord,
} from '../../../../mock-hub/src/transcripts.ts';
import type { Session } from '../../data/index.ts';
import { ChatView } from '../chat-view.tsx';
import {
  eventually,
  ID,
  renderWithHub,
  scrollTo,
  startHub,
  stubLayout,
  unmountAndSettle,
  type HubProcess,
  type Logged,
} from './harness.tsx';

const SYNTHETIC = '01JB0000000000000000SYNTH1';

function syntheticSession(): Session {
  return {
    id: SYNTHETIC,
    engine: 'claude',
    native_id: 'synthetic',
    machine: ID.laptop,
    cwd: '/work/synthetic',
    title: 'Synthetic',
    state: 'idle',
    started: 0,
    last_activity: 0,
  };
}

const json = (body: unknown) =>
  new Response(JSON.stringify(body), { status: 200, headers: { 'Content-Type': 'application/json' } });

/** Serves one made-up session and its transcript with the mock hub's own paging; the rest goes to the hub. */
function serveSynthetic(records: TranscriptRecord[]): typeof fetch {
  const session = syntheticSession();
  return (input, init) => {
    const url = new URL(String(input));
    if (url.pathname === `/v1/sessions/${SYNTHETIC}/transcript`) {
      const before = url.searchParams.get('before');
      const limit = url.searchParams.get('limit');
      return Promise.resolve(
        json(transcriptPage(records, before === null ? undefined : Number(before), limit === null ? 200 : Number(limit))),
      );
    }
    if (url.pathname === `/v1/sessions/${SYNTHETIC}`) return Promise.resolve(json(session));
    return fetch(input, init);
  };
}

const transcriptCalls = (requests: Logged[], session: string) =>
  requests.filter((r) => r.path === `/v1/sessions/${session}/transcript`);

const rowsOfType = (type: string) => document.querySelectorAll(`[data-row="${type}"]`);

/** Every row's type by index, scrolling through the whole list (only rows in view are drawn). */
function collectRows(scroller: HTMLElement): Map<number, string> {
  const seen = new Map<number, string>();
  for (let top = 0; top <= scroller.scrollHeight; top += 20) {
    scrollTo(scroller, top);
    for (const row of document.querySelectorAll<HTMLElement>('[data-index]')) {
      seen.set(Number(row.dataset.index), row.dataset.row ?? '');
    }
  }
  return new Map([...seen.entries()].sort(([a], [b]) => a - b));
}

/** Waits until a programmatic scroll (and TanStack Virtual's settling after it) is over. */
async function settled(scroller: HTMLElement): Promise<void> {
  let last = -1;
  let still = 0;
  while (still < 4) {
    await new Promise((r) => setTimeout(r, 50));
    still = scroller.scrollTop === last ? still + 1 : 0;
    last = scroller.scrollTop;
  }
}

async function expandAll(): Promise<void> {
  for (const button of screen.queryAllByRole('button', { expanded: false })) fireEvent.click(button);
}

describe('ChatView against the mock hub', () => {
  let hub: HubProcess | undefined;
  let unstub: () => void = () => {};

  beforeEach(() => {
    unstub = stubLayout({ viewport: 20_000, row: 40 });
  });

  afterEach(async () => {
    await unmountAndSettle();
    unstub();
    await hub?.close();
    hub = undefined;
  });

  it('renders every TranscriptItem kind from SES0001', async () => {
    hub = await startHub();
    renderWithHub(hub, <ChatView sessionId={ID.ses1} />);
    await screen.findByText('Start of the transcript');

    for (const type of ['prompt', 'text', 'plan', 'tool', 'edit', 'question', 'turn']) {
      expect(rowsOfType(type).length, type).toBeGreaterThan(0);
    }
    // Prompts and markdown.
    expect(screen.getByText(/Draft section 3 \(Method\)/)).toBeTruthy();
    await eventually(() => expect(document.querySelector('[data-markdown="pending"]')).toBeNull());
    expect(screen.getByText('§3.1 is written. I will compare both schedules in §3.2 next.')).toBeTruthy();

    // Tool calls paired with their results by call_id; the question's own call is folded into it.
    const tools = [...document.querySelectorAll<HTMLElement>('[data-tool]')].map((t) => t.dataset.tool);
    expect(tools).toEqual(['Read', 'Edit', 'Bash', 'Edit']);
    const bash = document.querySelector<HTMLElement>('[data-tool="Bash"]') as HTMLElement;
    expect(bash.textContent).toContain('latexmk -pdf main.tex');
    expect(bash.textContent).toContain('Output written on main.pdf (9 pages).');
    fireEvent.click(within(bash).getByRole('button'));
    expect(within(bash).getByText(/"description": "Build the paper"/)).toBeTruthy();
    // SES0001 is working (once its own query is in) and the last Edit has no result yet.
    await eventually(() => {
      const edits = document.querySelectorAll<HTMLElement>('[data-tool="Edit"] [data-status]');
      expect(edits[1]?.dataset.status).toBe('running');
    });

    // The file edit's diff, with the changed lines.
    const edit = rowsOfType('edit')[0] as HTMLElement;
    expect(edit.textContent).toContain('method.tex');
    expect(edit.textContent).toContain('+2');
    fireEvent.click(within(edit).getByRole('button'));
    await eventually(() => expect(edit.querySelector('[data-diff="ready"]')).not.toBeNull());
    expect(edit.querySelectorAll('[data-line="add"]')).toHaveLength(2);
    expect(edit.querySelectorAll('[data-line="del"]')).toHaveLength(1);

    // Plans as checklists; the plan bar shows the newest.
    const statuses = [...document.querySelectorAll<HTMLElement>('[data-plan-status]')].map((p) => p.dataset.planStatus);
    expect(statuses).toContain('completed');
    expect(statuses).toContain('in_progress');
    expect(screen.getByRole('button', { name: /Plan\s*2\/4\s*Write §3\.2 Noise schedule/ })).toBeTruthy();

    // The question, answered in the transcript.
    const question = await screen.findByRole('region', { name: /Question: Should §3.2 compare/ });
    expect(within(question).getByTestId('answer').textContent).toBe('Answered: Compare both');
    expect(within(question).getByRole('button', { name: 'Compare both' })).toHaveProperty('disabled', true);

    // Turn ends, and the live status line.
    expect(rowsOfType('turn')).toHaveLength(1);
    expect((await screen.findByRole('status')).textContent).toContain('Editing method.tex (§3.2)');
  });

  it('loads older pages on scroll-up, newest first, until the start', async () => {
    unstub();
    unstub = stubLayout({ viewport: 120, row: 40 });
    hub = await startHub();
    const { requests } = renderWithHub(hub, <ChatView sessionId={ID.ses1} pageSize={3} />);
    await screen.findByText(/Go ahead with §3.2/);
    const first = transcriptCalls(requests, ID.ses1)[0];
    expect(first?.query.get('before')).toBeNull();
    expect(first?.query.get('limit')).toBe('3');
    expect(screen.queryByText(/Draft section 3/)).toBeNull();

    const scroller = document.querySelector<HTMLElement>('[data-virtual-scroller]') as HTMLElement;
    for (let i = 0; i < 20 && screen.queryByText('Start of the transcript') === null; i++) {
      scrollTo(scroller, 0);
      await new Promise((r) => setTimeout(r, 50));
    }
    await screen.findByText('Start of the transcript');

    // Each older page was asked for with the `from` of the page after it.
    const calls = transcriptCalls(requests, ID.ses1);
    const befores = calls.map((c) => c.query.get('before')).filter((b) => b !== null).map(Number);
    expect(befores.length).toBeGreaterThanOrEqual(4);
    expect([...befores].sort((a, b) => b - a)).toEqual(befores);
    expect(new Set(befores).size).toBe(befores.length);

    // Everything is there once, in order: the same rows as the whole transcript in one page.
    const rows = [...collectRows(scroller).values()];
    expect(rows).toEqual([
      'start',
      'prompt',
      'text',
      'plan',
      'tool',
      'tool',
      'edit',
      'tool',
      'question',
      'text',
      'turn',
      'prompt',
      'plan',
      'tool',
    ]);
    // 18 items: the results fold into their calls, the question's call into the question.
    expect(cannedTranscripts().get(ID.ses1)?.flatMap((r) => r.items)).toHaveLength(18);
  }, 15_000);

  it('paints the newest page of a 5,000-item transcript first and keeps the view put when older items load', async () => {
    unstub();
    unstub = stubLayout({ viewport: 600, row: 40 });
    hub = await startHub();
    const records: TranscriptRecord[] = [];
    for (let i = 0; i < 5_000; i++) {
      appendRecord(records, [i % 2 === 0 ? userPrompt(i, `prompt ${i}`) : assistantText(i, `reply ${i}`)]);
    }
    const started = performance.now();
    const { requests } = renderWithHub(hub, <ChatView sessionId={SYNTHETIC} />, { fetch: serveSynthetic(records) });
    await screen.findByText('reply 4999');
    const elapsed = performance.now() - started;
    // Reported, not asserted: wall-clock time on a shared test machine is noise. What is asserted
    // is the order: one request, for the newest page, and only its rows drawn.
    console.info(`5,000 items: newest page painted ${Math.round(elapsed)} ms after mount (mock hub, happy-dom)`);

    const calls = transcriptCalls(requests, SYNTHETIC);
    expect(calls).toHaveLength(1);
    expect(calls[0]?.query.has('before')).toBe(false);
    expect(document.body.textContent).not.toContain('prompt 4798');
    const drawn = document.querySelectorAll('[data-row]').length;
    expect(drawn).toBeGreaterThan(5);
    expect(drawn).toBeLessThan(60);

    // Scroll to the top of the newest page: the page before loads, and the row that was at the top
    // stays where it was on screen.
    const scroller = document.querySelector<HTMLElement>('[data-virtual-scroller]') as HTMLElement;
    await settled(scroller);
    expect(scroller.scrollTop).toBeGreaterThan(1_000);
    scrollTo(scroller, 0);
    const rowOf = (el: HTMLElement) => el.closest<HTMLElement>('[data-index]') as HTMLElement;
    const y = (el: HTMLElement) => Number(/translateY\((-?[\d.]+)px\)/.exec(el.style.transform)?.[1]);
    const before = y(rowOf(screen.getByText('prompt 4800'))) - scroller.scrollTop;
    expect(before).toBeLessThan(40);

    await eventually(() => expect(transcriptCalls(requests, SYNTHETIC).length).toBeGreaterThan(1));
    const olderCall = transcriptCalls(requests, SYNTHETIC)[1];
    expect(Number(olderCall?.query.get('before'))).toBe(records[4_800]?.offset);
    await eventually(() => {
      const again = rowOf(screen.getByText('prompt 4800'));
      expect(Number(again.dataset.index)).toBe(200);
      expect(scroller.scrollTop).toBeGreaterThan(1_000);
      expect(Math.abs(y(again) - scroller.scrollTop - before)).toBeLessThanOrEqual(1);
    });
    expect(document.querySelectorAll('[data-row]').length).toBeLessThan(60);
  }, 15_000);
});

// ─── XSS ────────────────────────────────────────────────────────────────────────────────────────

const PAYLOADS = [
  '<img src=x onerror="globalThis.__pwned=1">',
  '<script>globalThis.__pwned=1</script>',
  '[click me](javascript:globalThis.__pwned=1)',
  '[spaced]( JaVaScRiPt:globalThis.__pwned=1 )',
  '[data](data:text/html;base64,PHNjcmlwdD5hbGVydCgxKTwvc2NyaXB0Pg==)',
  '<a href="javascript:globalThis.__pwned=1">raw anchor</a>',
  '<iframe src="https://evil.test/"></iframe>',
  '<javascript:globalThis.__pwned=1>',
  '![pixel](https://tracker.test/p.png)',
  '<div style="position:fixed;inset:0" onclick="globalThis.__pwned=1">overlay</div>',
  '[fine](https://example.com/safe)',
].join('\n\n');

function hostileTranscript(): TranscriptRecord[] {
  const records: TranscriptRecord[] = [];
  const at = 1_790_000_000_000;
  const drafts: ItemDraft[][] = [
    [userPrompt(at, PAYLOADS)],
    [assistantText(at, PAYLOADS)],
    [{ kind: 'tool_use', at, call_id: 'c1', tool: '<b onmouseover=alert(1)>Bash</b>', target: PAYLOADS, input: { command: PAYLOADS } }],
    [{ kind: 'tool_result', at, call_id: 'c1', is_error: true, summary: PAYLOADS }],
    [
      {
        kind: 'file_edit',
        at,
        path: '<img src=x onerror=alert(1)>.ts',
        added: 1,
        removed: 1,
        diff: `@@ -1 +1 @@\n-<script>old()</script>\n+<img src=x onerror="globalThis.__pwned=1">\n`,
      },
    ],
    [{ kind: 'plan_updated', at, items: [{ text: PAYLOADS, status: 'in_progress' }] }],
    [{ kind: 'question', at, text: PAYLOADS, options: ['<img src=x onerror=alert(1)>', '[x](javascript:alert(1))'] }],
    [{ kind: 'turn_ended', at }],
  ];
  for (const items of drafts) appendRecord(records, items);
  return records;
}

describe('ChatView with hostile transcript content', () => {
  let hub: HubProcess | undefined;
  let unstub: () => void = () => {};

  beforeEach(() => {
    unstub = stubLayout({ viewport: 20_000, row: 40 });
    Reflect.deleteProperty(globalThis, '__pwned');
  });

  afterEach(async () => {
    await unmountAndSettle();
    unstub();
    await hub?.close();
    hub = undefined;
  });

  it('renders raw HTML, scripts and javascript: links as text or inert markup', async () => {
    hub = await startHub();
    renderWithHub(hub, <ChatView sessionId={SYNTHETIC} />, { fetch: serveSynthetic(hostileTranscript()) });
    await screen.findByText('Start of the transcript');
    await expandAll();
    await eventually(() => {
      expect(document.querySelector('[data-markdown="pending"]')).toBeNull();
      expect(document.querySelector('[data-diff="pending"]')).toBeNull();
    });
    expect(document.querySelectorAll('[data-diff="ready"]')).toHaveLength(1);

    // No element the payloads describe exists, and no handler attribute anywhere.
    expect(document.querySelectorAll('script, iframe, img, object, embed, frame, [style*="fixed"]')).toHaveLength(0);
    for (const element of document.body.querySelectorAll('*')) {
      for (const attribute of element.getAttributeNames()) {
        expect(attribute.startsWith('on'), `${element.tagName} ${attribute}`).toBe(false);
      }
    }
    // Only the http(s) links are links, and they open outside the app.
    const anchors = [...document.querySelectorAll('a')];
    expect(anchors.length).toBeGreaterThan(0);
    for (const anchor of anchors) {
      expect(anchor.getAttribute('href')).toMatch(/^https:\/\//);
      expect(anchor.getAttribute('rel')).toBe('noopener noreferrer');
      expect(anchor.getAttribute('target')).toBe('_blank');
    }
    expect(anchors.map((a) => a.getAttribute('href'))).toContain('https://example.com/safe');
    // The image is offered as a link, never loaded.
    expect(anchors.map((a) => a.textContent)).toContain('[image: pixel]');

    // The payloads are there as text.
    const text = document.body.textContent ?? '';
    expect(text).toContain('<img src=x onerror="globalThis.__pwned=1">');
    expect(text).toContain('<script>globalThis.__pwned=1</script>');
    expect(text).toContain('<img src=x onerror=alert(1)>.ts');
    expect(text).toContain('<b onmouseover=alert(1)>Bash</b>');
    expect(Reflect.get(globalThis, '__pwned')).toBeUndefined();
  });
});
