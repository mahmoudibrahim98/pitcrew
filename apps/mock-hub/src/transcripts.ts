// Canned transcripts for the demo sessions, and tail-first paging.
//
// A transcript is a list of records, as a JSONL file is a list of lines. Each record has the byte
// range it occupies and the items a source adapter would make from it (none, one or several; they
// all carry the record's offset). Paging never splits a record, so `before = page.from` always
// continues exactly where the previous page began.
//
// SES0001 is derived from `crates/fixtures/data/transcripts/claude/demo-session.jsonl`, and its
// offsets are that file's real line offsets (a test checks this). The other sessions are written
// to match the demo workspace, including the offsets its receipts point to.

import type { PlanStatus, SessionId, TranscriptItem, TranscriptPage } from './types.ts';

/** One record: the bytes `offset..end` of the transcript, and the items made from it. */
export interface TranscriptRecord {
  offset: number;
  end: number;
  items: TranscriptItem[];
}

type WithoutOffset<T> = T extends unknown ? Omit<T, 'offset'> : never;

/** A transcript item before it is placed in a record. */
export type ItemDraft = WithoutOffset<TranscriptItem>;

/** The demo session derived from the Claude fixture. */
export const CLAUDE_FIXTURE_SESSION: SessionId = '01JB000000000000000SES0001';

/**
 * The newest page of at most `limit` items that end before the record at offset `before` (or at
 * the end of the transcript). A record whose items would overflow the limit is left for the next
 * page, unless it is the only one, so every page makes progress.
 */
export function transcriptPage(
  records: readonly TranscriptRecord[],
  before: number | undefined,
  limit: number,
): TranscriptPage {
  const end = before === undefined ? records.length : firstIndexAtOrAfter(records, before);
  let start = end;
  let count = 0;
  while (start > 0 && count < limit) {
    const size = records[start - 1]?.items.length ?? 0;
    if (count > 0 && count + size > limit) {
      break;
    }
    start -= 1;
    count += size;
  }
  const page = records.slice(start, end);
  const edge = records[end]?.offset ?? records.at(-1)?.end ?? 0;
  return {
    items: page.flatMap((r) => r.items),
    from: page[0]?.offset ?? edge,
    to: page.at(-1)?.end ?? edge,
    at_start: start === 0,
  };
}

/** Appends a record after the last one. Its size stands in for the JSONL line a CLI would write. */
export function appendRecord(records: TranscriptRecord[], items: ItemDraft[]): TranscriptRecord {
  const offset = records.at(-1)?.end ?? 0;
  const size = Buffer.byteLength(JSON.stringify(items)) + 1;
  const appended = record(offset, offset + size, ...items);
  records.push(appended);
  return appended;
}

/** Fresh copies of every canned transcript, keyed by session id. */
export function cannedTranscripts(): Map<SessionId, TranscriptRecord[]> {
  return new Map([
    [CLAUDE_FIXTURE_SESSION, draftMethodSection()],
    ['01JB000000000000000SES0002', seedRuns()],
    ['01JB000000000000000SES0003', reviewBenchmarks()],
    ['01JB000000000000000SES0004', codexParser()],
    ['01JB000000000000000SES0005', cosineSchedule()],
    ['01JB000000000000000SES0006', coauthorResponses()],
  ]);
}

// ─── Item builders ──────────────────────────────────────────────────────────────────────────────

export const userPrompt = (at: number, text: string): ItemDraft => ({ kind: 'user_prompt', at, text });

export const assistantText = (at: number, text: string): ItemDraft => ({
  kind: 'assistant_text',
  at,
  text,
});

export const turnEnded = (at: number): ItemDraft => ({ kind: 'turn_ended', at });

const toolUse = (
  at: number,
  callId: string,
  tool: string,
  target: string,
  input?: unknown,
): ItemDraft =>
  input === undefined
    ? { kind: 'tool_use', at, call_id: callId, tool, target }
    : { kind: 'tool_use', at, call_id: callId, tool, target, input };

const toolResult = (at: number, callId: string, summary: string, isError = false): ItemDraft => ({
  kind: 'tool_result',
  at,
  call_id: callId,
  is_error: isError,
  summary,
});

const fileEdit = (
  at: number,
  path: string,
  added: number,
  removed: number,
  diff?: string,
): ItemDraft =>
  diff === undefined
    ? { kind: 'file_edit', at, path, added, removed }
    : { kind: 'file_edit', at, path, added, removed, diff };

const plan = (at: number, ...items: [string, PlanStatus][]): ItemDraft => ({
  kind: 'plan_updated',
  at,
  items: items.map(([text, status]) => ({ text, status })),
});

const question = (at: number, text: string, options: string[]): ItemDraft => ({
  kind: 'question',
  at,
  text,
  options,
});

function record(offset: number, end: number, ...items: ItemDraft[]): TranscriptRecord {
  return { offset, end, items: items.map((item) => ({ ...item, offset }) as TranscriptItem) };
}

function firstIndexAtOrAfter(records: readonly TranscriptRecord[], offset: number): number {
  const index = records.findIndex((r) => r.offset >= offset);
  return index === -1 ? records.length : index;
}

const t = (iso: string): number => Date.parse(iso);

// ─── The canned sessions ────────────────────────────────────────────────────────────────────────

const PAPER = '/home/sam/work/diffusion-paper/paper';
const RUNS = '/scratch/sam/diffusion-runs';

const METHOD_DIFF = [
  '--- a/method.tex',
  '+++ b/method.tex',
  '@@ -1,1 +1,2 @@',
  '-% TODO method',
  '+\\subsection{Model}',
  '+We use a four-level U-Net.',
  '',
].join('\n');

const RUNS_DIFF = [
  '--- a/runs.md',
  '+++ b/runs.md',
  '@@ -3,0 +4,1 @@',
  '+| 1-5 | 4815162-4815166 | submitted |',
  '',
].join('\n');

/** SES0001 (Claude, @writer): the Claude fixture, then the turn it is working on now. */
function draftMethodSection(): TranscriptRecord[] {
  return [
    record(
      0,
      382,
      userPrompt(
        t('2026-09-30T08:00:00Z'),
        'Draft section 3 (Method) from notes/method-outline.md. Keep it under two pages.',
      ),
    ),
    record(
      382,
      1428,
      assistantText(
        t('2026-09-30T08:00:04Z'),
        'I will read the outline first, then write the three subsections.',
      ),
      plan(
        t('2026-09-30T08:00:04Z'),
        ['Read notes/method-outline.md', 'in_progress'],
        ['Write §3.1 Model', 'pending'],
        ['Write §3.2 Noise schedule', 'pending'],
        ['Write §3.3 Training objective', 'pending'],
      ),
    ),
    // 1428..1840 is TodoWrite's bookkeeping result, which adapters drop.
    record(
      1840,
      2436,
      toolUse(t('2026-09-30T08:00:09Z'), 'toolu_demo_0002', 'Read', 'notes/method-outline.md', {
        file_path: `${PAPER}/notes/method-outline.md`,
      }),
    ),
    record(
      2436,
      2945,
      toolResult(
        t('2026-09-30T08:00:09.500Z'),
        'toolu_demo_0002',
        '# Method outline\n- 3.1 Model: U-Net, 4 levels\n- 3.2 Noise schedule: linear vs cosine\n- 3.3 Objective: epsilon prediction',
      ),
    ),
    record(
      2945,
      3623,
      toolUse(t('2026-09-30T08:45:00Z'), 'toolu_demo_0003', 'Edit', 'method.tex', {
        file_path: `${PAPER}/method.tex`,
        old_string: '% TODO method',
        new_string: '\\\\subsection{Model}\nWe use a four-level U-Net.',
      }),
    ),
    record(
      3623,
      4304,
      toolResult(
        t('2026-09-30T08:45:01Z'),
        'toolu_demo_0003',
        `The file ${PAPER}/method.tex has been updated.`,
      ),
      fileEdit(t('2026-09-30T08:45:01Z'), 'method.tex', 2, 1, METHOD_DIFF),
    ),
    record(
      4304,
      4891,
      toolUse(t('2026-09-30T08:50:00Z'), 'toolu_demo_0004', 'Bash', 'latexmk -pdf main.tex', {
        command: 'latexmk -pdf main.tex',
        description: 'Build the paper',
      }),
    ),
    record(
      4891,
      5319,
      toolResult(t('2026-09-30T08:50:20Z'), 'toolu_demo_0004', 'Output written on main.pdf (9 pages).'),
    ),
    record(
      5319,
      6113,
      toolUse(t('2026-09-30T08:51:00Z'), 'toolu_demo_0005', 'AskUserQuestion', 'Schedule', {
        questions: [
          {
            question: 'Should §3.2 compare linear and cosine schedules, or describe cosine only?',
            header: 'Schedule',
            multiSelect: false,
            options: [
              { label: 'Compare both', description: 'Adds half a page' },
              { label: 'Cosine only', description: 'Shorter' },
            ],
          },
        ],
      }),
      question(
        t('2026-09-30T08:51:00Z'),
        'Should §3.2 compare linear and cosine schedules, or describe cosine only?',
        ['Compare both', 'Cosine only'],
      ),
    ),
    record(
      6113,
      6514,
      toolResult(t('2026-09-30T09:10:00Z'), 'toolu_demo_0005', 'User answered: Compare both'),
    ),
    record(
      6514,
      7056,
      assistantText(
        t('2026-09-30T09:12:00Z'),
        '§3.1 is written. I will compare both schedules in §3.2 next.',
      ),
    ),
    record(7056, 7314, turnEnded(t('2026-09-30T09:12:00.500Z'))),
    // 7314..7528 holds the custom title and the summary, which carry no items. The fixture ends
    // there; the records below are the turn the session is working on now ("Editing method.tex").
    record(
      7528,
      7902,
      userPrompt(t('2026-09-30T09:20:00Z'), 'Go ahead with §3.2 and compare both schedules.'),
    ),
    record(
      7902,
      8961,
      plan(
        t('2026-09-30T09:20:06Z'),
        ['Read notes/method-outline.md', 'completed'],
        ['Write §3.1 Model', 'completed'],
        ['Write §3.2 Noise schedule', 'in_progress'],
        ['Write §3.3 Training objective', 'pending'],
      ),
    ),
    record(
      8961,
      9630,
      toolUse(t('2026-09-30T10:00:00Z'), 'toolu_demo_0006', 'Edit', 'method.tex', {
        file_path: `${PAPER}/method.tex`,
        old_string: '% TODO schedule',
        new_string: '\\subsection{Noise schedule}\nWe compare a linear and a cosine schedule.',
      }),
    ),
  ];
}

/**
 * SES0002 (Codex, @runner). The first records follow the Codex fixture's lines; a day of watching
 * is left out, and the later records sit at the offsets EVT0009 and ASK0002 cite.
 */
function seedRuns(): TranscriptRecord[] {
  const submit = 'for s in 1 2 3 4 5; do sbatch --export=SEED=$s jobs/train.sbatch; done';
  return [
    record(
      638,
      856,
      userPrompt(
        t('2026-09-29T08:00:01Z'),
        'Submit seeds 1-5 with configs/base.yaml and watch them to epoch 40.',
      ),
    ),
    record(
      1045,
      1397,
      plan(
        t('2026-09-29T08:00:05Z'),
        ['Submit seeds 1-5', 'in_progress'],
        ['Check first-epoch losses', 'pending'],
        ['Watch to epoch 40', 'pending'],
      ),
    ),
    record(
      1554,
      1861,
      toolUse(t('2026-09-29T08:00:09Z'), 'call_demo_0002', 'shell', submit, {
        command: ['bash', '-lc', submit],
        workdir: RUNS,
      }),
    ),
    record(
      1861,
      2229,
      toolResult(
        t('2026-09-29T08:00:11Z'),
        'call_demo_0002',
        [4815162, 4815163, 4815164, 4815165, 4815166].map((id) => `Submitted batch job ${id}`).join('\n'),
      ),
    ),
    record(
      2229,
      2511,
      toolUse(t('2026-09-29T08:00:14Z'), 'call_demo_0003', 'apply_patch', 'runs.md'),
      fileEdit(t('2026-09-29T08:00:14Z'), 'runs.md', 1, 0, RUNS_DIFF),
    ),
    record(
      2511,
      2784,
      toolResult(
        t('2026-09-29T08:00:14.200Z'),
        'call_demo_0003',
        'Success. Updated the following files:\nM runs.md',
      ),
    ),
    record(
      2983,
      3231,
      assistantText(
        t('2026-09-29T08:00:16Z'),
        'Submitted five jobs (4815162-4815166). I will check the first-epoch losses when they start.',
      ),
    ),
    record(3415, 3572, turnEnded(t('2026-09-29T08:00:16.200Z'))),
    record(
      118220,
      118468,
      toolUse(t('2026-09-30T09:15:00Z'), 'call_demo_0004', 'shell', 'squeue --me', {
        command: ['bash', '-lc', 'squeue --me'],
        workdir: RUNS,
      }),
    ),
    record(
      118468,
      118837,
      toolResult(
        t('2026-09-30T09:15:01Z'),
        'call_demo_0004',
        'JOBID    NAME   ST  TIME\n4815162  seed1  R   25:15:01\n4815163  seed2  R   25:15:01\n4815165  seed4  R   25:15:01\n4815166  seed5  R   25:15:01',
      ),
    ),
    record(
      120544,
      121012,
      assistantText(
        t('2026-09-30T09:40:00Z'),
        'Seed 3 (job 4815164) failed at epoch 9: the loss went to NaN at step 18,400. Seeds 1, 2, 4 and 5 are healthy at epoch 12.',
      ),
    ),
    record(
      121012,
      121390,
      toolUse(t('2026-09-30T09:58:00Z'), 'call_demo_0005', 'shell', 'tail -n 1 logs/seed1.out', {
        command: ['bash', '-lc', 'tail -n 1 logs/seed1.out'],
        workdir: RUNS,
      }),
    ),
    record(
      121390,
      121611,
      toolResult(t('2026-09-30T09:58:01Z'), 'call_demo_0005', 'epoch 12/40 · step 24,960 · loss 0.0412'),
    ),
  ];
}

/** SES0003 (Claude, @reviewer): waiting on the question ASK0001 cites at offset 48213. */
function reviewBenchmarks(): TranscriptRecord[] {
  const ask = 'Merge the benchmark change into parsers?';
  const options = ['Merge it', "Hold, I'll look"];
  return [
    record(
      0,
      402,
      userPrompt(
        t('2026-09-30T09:00:00Z'),
        'Check the parser benchmark numbers on the parsers branch before the PR opens.',
      ),
    ),
    record(
      402,
      1180,
      toolUse(t('2026-09-30T09:01:00Z'), 'toolu_rev_0001', 'Read', 'bench/results.md', {
        file_path: '/home/sam/work/lab-tools/bench/results.md',
      }),
    ),
    record(
      1180,
      45410,
      toolResult(
        t('2026-09-30T09:01:01Z'),
        'toolu_rev_0001',
        'incremental/400MB  0.41 s   (was 3.73 s, 9.1× faster)\ntiny_files         1.04 ms  (was 1.00 ms, 4% slower)',
      ),
    ),
    record(
      45410,
      48213,
      assistantText(
        t('2026-09-30T09:49:30Z'),
        'Incremental parsing is 9.1× faster on the 400 MB fixture. One benchmark, tiny_files, is 4% slower: the offset bookkeeping costs more than it saves on very small files.',
      ),
    ),
    record(
      48213,
      49020,
      toolUse(t('2026-09-30T09:50:00Z'), 'toolu_rev_0002', 'AskUserQuestion', 'Merge', {
        questions: [
          {
            question: ask,
            header: 'Merge',
            multiSelect: false,
            options: options.map((label) => ({ label })),
          },
        ],
      }),
      question(t('2026-09-30T09:50:00Z'), ask, options),
    ),
  ];
}

/** SES0004 (OpenCode, @builder): a finished turn that changed three files. */
function codexParser(): TranscriptRecord[] {
  return [
    record(
      0,
      310,
      userPrompt(
        t('2026-09-30T08:20:00Z'),
        'Parse Codex rollouts incrementally. Read from a byte offset; never re-read a file from the start.',
      ),
    ),
    record(310, 1720, fileEdit(t('2026-09-30T08:40:00Z'), 'src/codex/reader.rs', 48, 10)),
    record(1720, 2230, fileEdit(t('2026-09-30T08:41:00Z'), 'src/codex/mod.rs', 6, 2)),
    record(2230, 3610, fileEdit(t('2026-09-30T08:42:00Z'), 'tests/codex_incremental.rs', 31, 0)),
    record(
      3610,
      3980,
      toolUse(t('2026-09-30T09:25:00Z'), 'call_oc_0001', 'bash', 'cargo test codex', {
        command: 'cargo test codex',
      }),
    ),
    record(
      3980,
      4420,
      toolResult(t('2026-09-30T09:27:00Z'), 'call_oc_0001', 'test result: ok. 23 passed; 0 failed'),
    ),
    record(
      4420,
      4890,
      assistantText(
        t('2026-09-30T09:29:50Z'),
        'The Codex reader now keeps the last byte offset per file and resumes from it. Three files changed; all 23 tests pass.',
      ),
    ),
    record(4890, 5010, turnEnded(t('2026-09-30T09:30:00Z'))),
  ];
}

/** SES0005 (Claude on the unreachable GPU box): cut off mid-run. */
function cosineSchedule(): TranscriptRecord[] {
  const train = 'python train.py --config configs/small.yaml --schedule cosine --steps 50000';
  return [
    record(
      0,
      350,
      userPrompt(
        t('2026-09-28T08:00:00Z'),
        'Try a cosine noise schedule on the small model and compare FID at 50k steps.',
      ),
    ),
    record(
      350,
      900,
      assistantText(
        t('2026-09-28T08:00:10Z'),
        'I will train the small model for 50k steps with the cosine schedule, then compute FID against the linear baseline.',
      ),
    ),
    record(
      900,
      1480,
      toolUse(t('2026-09-28T08:01:00Z'), 'toolu_gpu_0001', 'Bash', train, {
        command: train,
        description: 'Train with the cosine schedule',
      }),
    ),
  ];
}

/** SES0006 (Claude, @writer): the finished co-author responses. */
function coauthorResponses(): TranscriptRecord[] {
  return [
    record(
      0,
      380,
      userPrompt(
        t('2026-09-29T09:00:00Z'),
        'Answer every comment in comments.md. Flag anything that changes a claim.',
      ),
    ),
    record(
      380,
      960,
      toolUse(t('2026-09-29T09:00:10Z'), 'toolu_co_0001', 'Read', 'comments.md', {
        file_path: `${PAPER}/comments.md`,
      }),
    ),
    record(
      960,
      5210,
      toolResult(
        t('2026-09-29T09:00:11Z'),
        'toolu_co_0001',
        '14 comments from two co-authors, on §1, §3 and §5.',
      ),
    ),
    record(
      5210,
      6120,
      toolUse(t('2026-09-29T10:15:00Z'), 'toolu_co_0002', 'Write', 'responses.md', {
        file_path: `${PAPER}/responses.md`,
      }),
    ),
    record(
      6120,
      6620,
      toolResult(
        t('2026-09-29T10:15:01Z'),
        'toolu_co_0002',
        `File created successfully at: ${PAPER}/responses.md`,
      ),
      fileEdit(t('2026-09-29T10:15:01Z'), 'responses.md', 96, 0),
    ),
    record(
      6620,
      7170,
      assistantText(
        t('2026-09-29T10:19:50Z'),
        'Drafted answers to all 14 comments; 2 need your call (comments 6 and 11).',
      ),
    ),
    record(7170, 7420, turnEnded(t('2026-09-29T10:20:00Z'))),
  ];
}
