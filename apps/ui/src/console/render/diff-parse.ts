// Unified diffs to numbered lines, with the changed words of each edited line marked.

export type DiffLineType = 'add' | 'del' | 'ctx' | 'hunk' | 'file' | 'note';

/** A character range `[start, end)` within a line's text. */
export type Mark = [number, number];

export interface DiffLine {
  type: DiffLineType;
  /** The line without its `+`, `-` or space prefix (whole for headers). */
  text: string;
  old?: number;
  new?: number;
  marks?: Mark[];
}

export interface ParsedDiff {
  lines: DiffLine[];
  added: number;
  removed: number;
}

const HUNK = /^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@/;
/** Word diffs beyond this many token pairs are skipped; the lines stay coloured. */
const MAX_WORK = 40_000;

export function parseDiff(source: string): ParsedDiff {
  const raw = source.replace(/\r\n?/g, '\n').split('\n');
  if (raw.at(-1) === '') raw.pop();
  const lines: DiffLine[] = [];
  let oldNo = 0;
  let newNo = 0;
  let oldLeft = 0;
  let newLeft = 0;
  let added = 0;
  let removed = 0;
  for (const text of raw) {
    const inHunk = oldLeft > 0 || newLeft > 0;
    const hunk = inHunk ? null : HUNK.exec(text);
    if (hunk !== null) {
      oldNo = Number(hunk[1]);
      newNo = Number(hunk[3]);
      oldLeft = hunk[2] === undefined ? 1 : Number(hunk[2]);
      newLeft = hunk[4] === undefined ? 1 : Number(hunk[4]);
      lines.push({ type: 'hunk', text });
    } else if (!inHunk) {
      lines.push({ type: text.startsWith('\\') ? 'note' : 'file', text });
    } else if (text.startsWith('+')) {
      lines.push({ type: 'add', text: text.slice(1), new: newNo++ });
      newLeft -= 1;
      added += 1;
    } else if (text.startsWith('-')) {
      lines.push({ type: 'del', text: text.slice(1), old: oldNo++ });
      oldLeft -= 1;
      removed += 1;
    } else if (text.startsWith('\\')) {
      lines.push({ type: 'note', text });
    } else {
      lines.push({ type: 'ctx', text: text.slice(1), old: oldNo++, new: newNo++ });
      oldLeft -= 1;
      newLeft -= 1;
    }
  }
  markWords(lines);
  return { lines, added, removed };
}

/** Pairs each run of removed lines with the added lines after it and marks what changed. */
function markWords(lines: DiffLine[]): void {
  let i = 0;
  while (i < lines.length) {
    if (lines[i]?.type !== 'del') {
      i += 1;
      continue;
    }
    const delStart = i;
    while (lines[i]?.type === 'del') i += 1;
    const addStart = i;
    while (lines[i]?.type === 'add') i += 1;
    const pairs = Math.min(addStart - delStart, i - addStart);
    for (let k = 0; k < pairs; k++) {
      const del = lines[delStart + k];
      const add = lines[addStart + k];
      if (del === undefined || add === undefined) continue;
      const marks = changedRanges(del.text, add.text);
      if (marks !== undefined) {
        del.marks = marks[0];
        add.marks = marks[1];
      }
    }
  }
}

function tokenize(line: string): string[] {
  return line.match(/[\p{L}\p{N}_]+|\s+|[^\p{L}\p{N}_\s]/gu) ?? [];
}

function pushMark(marks: Mark[], start: number, end: number): void {
  const last = marks.at(-1);
  if (last !== undefined && last[1] === start) last[1] = end;
  else marks.push([start, end]);
}

/** The changed ranges of two versions of a line, or undefined if they share nothing worth showing. */
export function changedRanges(a: string, b: string): [Mark[], Mark[]] | undefined {
  const ta = tokenize(a);
  const tb = tokenize(b);
  const n = ta.length;
  const m = tb.length;
  if (n * m > MAX_WORK || n === 0 || m === 0) return undefined;
  // lcs[i][j]: the longest common subsequence of ta[i..] and tb[j..].
  const lcs = Array.from({ length: n + 1 }, () => new Uint16Array(m + 1));
  for (let i = n - 1; i >= 0; i--) {
    const row = lcs[i] as Uint16Array;
    const below = lcs[i + 1] as Uint16Array;
    for (let j = m - 1; j >= 0; j--) {
      row[j] = ta[i] === tb[j] ? (below[j + 1] ?? 0) + 1 : Math.max(below[j] ?? 0, row[j + 1] ?? 0);
    }
  }
  const marksA: Mark[] = [];
  const marksB: Mark[] = [];
  let i = 0;
  let j = 0;
  let pa = 0;
  let pb = 0;
  let shared = 0;
  while (i < n || j < m) {
    const x = ta[i];
    const y = tb[j];
    if (x !== undefined && y !== undefined && x === y) {
      if (x.trim() !== '') shared += x.length;
      pa += x.length;
      pb += y.length;
      i += 1;
      j += 1;
    } else if (y !== undefined && (x === undefined || (lcs[i]?.[j + 1] ?? 0) >= (lcs[i + 1]?.[j] ?? 0))) {
      pushMark(marksB, pb, pb + y.length);
      pb += y.length;
      j += 1;
    } else if (x !== undefined) {
      pushMark(marksA, pa, pa + x.length);
      pa += x.length;
      i += 1;
    }
  }
  return shared === 0 ? undefined : [marksA, marksB];
}
