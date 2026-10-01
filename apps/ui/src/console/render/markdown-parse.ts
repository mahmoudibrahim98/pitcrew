// Markdown to a plain-data tree, small enough to post from a worker. It covers what agents write:
// headings, paragraphs, fenced code, quotes, nested and task lists, GFM tables, rules, emphasis,
// strikethrough, code spans, links, autolinks and bare URLs. A newline is a line break, as in a
// terminal.
//
// Raw HTML is never interpreted: `<img onerror=…>` stays the literal text it is. Link targets are
// kept as written; the renderer decides which are safe to link (`safeHref`).

export type Inline =
  | { t: 'text'; v: string }
  | { t: 'code'; v: string }
  | { t: 'em' | 'strong' | 'del'; c: Inline[] }
  | { t: 'link'; href: string; c: Inline[] }
  | { t: 'img'; src: string; alt: string }
  | { t: 'br' };

export type Align = 'left' | 'center' | 'right' | null;

export interface ListItem {
  /** A task list item's box: checked, unchecked, or none. */
  check: boolean | null;
  c: Block[];
}

export type Block =
  | { t: 'p'; c: Inline[] }
  | { t: 'h'; level: 1 | 2 | 3 | 4 | 5 | 6; c: Inline[] }
  | { t: 'code'; lang: string; v: string }
  | { t: 'quote'; c: Block[] }
  | { t: 'list'; ordered: boolean; start: number; tight: boolean; items: ListItem[] }
  | { t: 'table'; align: Align[]; head: Inline[][]; rows: Inline[][][] }
  | { t: 'hr' };

export function parseMarkdown(source: string): Block[] {
  return parseBlocks(source.replace(/\r\n?/g, '\n').split('\n'), 0);
}

// ─── Blocks ─────────────────────────────────────────────────────────────────────────────────────

/** Deeper nesting than this is shown as text, so hostile input cannot exhaust the stack. */
const MAX_DEPTH = 24;

const FENCE_OPEN = /^( {0,3})(`{3,}|~{3,})(.*)$/;
const FENCE_CLOSE = /^ {0,3}(`{3,}|~{3,})[ \t]*$/;
const ATX = /^ {0,3}(#{1,6})(?:[ \t]+(.*))?$/;
const HR = /^ {0,3}(?:(?:-[ \t]*){3,}|(?:\*[ \t]*){3,}|(?:_[ \t]*){3,})$/;
const QUOTE = /^ {0,3}> ?(.*)$/;
const SETEXT = /^ {0,3}(=+|-+)[ \t]*$/;
const BULLET = /^( {0,3})([-*+])(?:([ \t]+)(.*))?$/;
const ORDERED = /^( {0,3})(\d{1,9})([.)])(?:([ \t]+)(.*))?$/;
const TASK = /^\[([ xX])\][ \t]+/;
const DELIM_CELL = /^:?-+:?$/;

const isBlank = (line: string): boolean => /^[ \t]*$/.test(line);

/** Columns of leading whitespace; a tab reaches the next multiple of 4. */
function indentOf(line: string): number {
  let col = 0;
  for (const ch of line) {
    if (ch === ' ') col += 1;
    else if (ch === '\t') col += 4 - (col % 4);
    else break;
  }
  return col;
}

/** Drops `n` columns of leading whitespace, or all of it if there is less. */
function dedent(line: string, n: number): string {
  let col = 0;
  let i = 0;
  while (i < line.length && col < n) {
    const ch = line[i];
    if (ch === ' ') col += 1;
    else if (ch === '\t') {
      const width = 4 - (col % 4);
      if (col + width > n) return ' '.repeat(col + width - n) + line.slice(i + 1);
      col += width;
    } else break;
    i += 1;
  }
  return line.slice(i);
}

function fenceOpen(line: string): { indent: number; fence: string; info: string } | undefined {
  const m = FENCE_OPEN.exec(line);
  if (m === null) return undefined;
  const fence = m[2] ?? '';
  const info = (m[3] ?? '').trim();
  // A backtick fence's info string may not contain backticks (it would be a code span).
  if (fence.startsWith('`') && info.includes('`')) return undefined;
  return { indent: (m[1] ?? '').length, fence, info };
}

interface ItemStart {
  ordered: boolean;
  /** The bullet character, or the ordered delimiter (`.` or `)`). */
  marker: string;
  start: number;
  indent: number;
  /** Where the item's content starts; continuation lines must reach it. */
  contentIndent: number;
  first: string;
}

function listItemStart(line: string): ItemStart | undefined {
  const bullet = BULLET.exec(line);
  const ordered = bullet === null ? ORDERED.exec(line) : null;
  const m = bullet ?? ordered;
  if (m === null) return undefined;
  const indent = (m[1] ?? '').length;
  const markerText = bullet !== null ? (m[2] ?? '') : `${m[2] ?? ''}${m[3] ?? ''}`;
  const space = bullet !== null ? m[3] : m[4];
  const rest = (bullet !== null ? m[4] : m[5]) ?? '';
  const markerEnd = indent + markerText.length;
  let contentIndent = markerEnd + 1;
  let first = rest;
  if (space !== undefined && rest !== '') {
    const width = indentOf(space);
    // Five or more spaces: the content is indented code; keep all but one of them.
    if (width <= 4) contentIndent = markerEnd + width;
    else first = ' '.repeat(width - 1) + rest;
  }
  return {
    ordered: ordered !== null,
    marker: bullet !== null ? (m[2] ?? '-') : (m[3] ?? '.'),
    start: ordered !== null ? Number(m[2]) : 1,
    indent,
    contentIndent,
    first,
  };
}

/** Whether a line begins a block that ends a paragraph. */
function interruptsParagraph(line: string): boolean {
  if (fenceOpen(line) !== undefined || ATX.test(line) || HR.test(line) || QUOTE.test(line)) return true;
  const item = listItemStart(line);
  return item !== undefined && item.first !== '' && (!item.ordered || item.start === 1);
}

function splitRow(line: string): string[] {
  let row = line.trim();
  if (row.startsWith('|')) row = row.slice(1);
  if (row.endsWith('|') && !row.endsWith('\\|')) row = row.slice(0, -1);
  const cells: string[] = [];
  let cell = '';
  for (let i = 0; i < row.length; i++) {
    const ch = row[i];
    if (ch === '\\' && row[i + 1] === '|') {
      cell += '|';
      i += 1;
    } else if (ch === '|') {
      cells.push(cell.trim());
      cell = '';
    } else {
      cell += ch;
    }
  }
  cells.push(cell.trim());
  return cells;
}

function isTableStart(lines: readonly string[], i: number): boolean {
  const head = lines[i];
  const delim = lines[i + 1];
  if (head === undefined || delim === undefined || !head.includes('|') || !delim.includes('|')) return false;
  const cells = splitRow(delim);
  return cells.every((c) => DELIM_CELL.test(c)) && splitRow(head).length === cells.length;
}

function parseBlocks(lines: readonly string[], depth: number): Block[] {
  if (depth > MAX_DEPTH) return [{ t: 'p', c: [{ t: 'text', v: lines.join('\n') }] }];
  const blocks: Block[] = [];
  let i = 0;
  while (i < lines.length) {
    const line = lines[i] ?? '';
    if (isBlank(line)) {
      i += 1;
      continue;
    }

    const fence = fenceOpen(line);
    if (fence !== undefined) {
      const body: string[] = [];
      i += 1;
      while (i < lines.length) {
        const l = lines[i] ?? '';
        const close = FENCE_CLOSE.exec(l)?.[1];
        i += 1;
        if (close !== undefined && close[0] === fence.fence[0] && close.length >= fence.fence.length) break;
        body.push(dedent(l, fence.indent));
      }
      blocks.push({ t: 'code', lang: fence.info.split(/\s+/)[0] ?? '', v: body.join('\n') });
      continue;
    }

    const atx = ATX.exec(line);
    if (atx !== null) {
      const text = (atx[2] ?? '').replace(/(?:^|[ \t]+)#+[ \t]*$/, '').trim();
      const level = (atx[1] ?? '#').length as 1 | 2 | 3 | 4 | 5 | 6;
      blocks.push({ t: 'h', level, c: parseInline(text) });
      i += 1;
      continue;
    }

    if (HR.test(line)) {
      blocks.push({ t: 'hr' });
      i += 1;
      continue;
    }

    if (QUOTE.test(line)) {
      const inner: string[] = [];
      while (i < lines.length) {
        const l = lines[i] ?? '';
        const quoted = QUOTE.exec(l);
        if (quoted !== null) {
          inner.push(quoted[1] ?? '');
        } else if (!isBlank(l) && !isBlank(inner.at(-1) ?? '') && !interruptsParagraph(l)) {
          inner.push(l); // a lazy continuation of the quoted paragraph
        } else {
          break;
        }
        i += 1;
      }
      blocks.push({ t: 'quote', c: parseBlocks(inner, depth + 1) });
      continue;
    }

    const item = listItemStart(line);
    if (item !== undefined) {
      const [list, next] = parseList(lines, i, item, depth);
      blocks.push(list);
      i = next;
      continue;
    }

    if (isTableStart(lines, i)) {
      const [table, next] = parseTable(lines, i);
      blocks.push(table);
      i = next;
      continue;
    }

    // A paragraph, or a setext heading if an underline ends it.
    const text: string[] = [line.replace(/^[ \t]+/, '')];
    let level: 1 | 2 | undefined;
    i += 1;
    while (i < lines.length) {
      const l = lines[i] ?? '';
      if (isBlank(l)) break;
      const underline = SETEXT.exec(l)?.[1];
      if (underline !== undefined) {
        level = underline.startsWith('=') ? 1 : 2;
        i += 1;
        break;
      }
      if (interruptsParagraph(l) || isTableStart(lines, i)) break;
      text.push(l.replace(/^[ \t]+/, ''));
      i += 1;
    }
    const joined = text.join('\n');
    blocks.push(
      level === undefined
        ? { t: 'p', c: parseInline(joined.replace(/[ \t]+$/, '')) }
        : { t: 'h', level, c: parseInline(joined.trim()) },
    );
  }
  return blocks;
}

function sameList(a: ItemStart, b: ItemStart): boolean {
  return a.ordered === b.ordered && a.marker === b.marker;
}

function parseList(lines: readonly string[], at: number, first: ItemStart, depth: number): [Block, number] {
  const items: ListItem[] = [];
  let tight = true;
  let current = first;
  let i = at;
  for (;;) {
    let firstLine = current.first;
    let check: boolean | null = null;
    const task = TASK.exec(firstLine);
    if (task !== null) {
      check = task[1] !== ' ';
      firstLine = firstLine.slice(task[0].length);
    }
    const body: string[] = [firstLine];
    let blanks = 0;
    i += 1;
    while (i < lines.length) {
      const l = lines[i] ?? '';
      if (isBlank(l)) {
        blanks += 1;
      } else if (indentOf(l) >= current.contentIndent) {
        if (blanks > 0) tight = false;
        for (; blanks > 0; blanks--) body.push('');
        body.push(dedent(l, current.contentIndent));
      } else if (blanks === 0 && !interruptsParagraph(l) && listItemStart(l) === undefined) {
        body.push(l.replace(/^[ \t]+/, '')); // a lazy continuation
      } else {
        break;
      }
      i += 1;
    }
    items.push({ check, c: parseBlocks(body, depth + 1) });
    const next = i < lines.length ? listItemStart(lines[i] ?? '') : undefined;
    if (next === undefined || !sameList(first, next)) break;
    if (blanks > 0) tight = false;
    current = next;
  }
  return [{ t: 'list', ordered: first.ordered, start: first.start, tight, items }, i];
}

function parseTable(lines: readonly string[], at: number): [Block, number] {
  const head = splitRow(lines[at] ?? '');
  const align: Align[] = splitRow(lines[at + 1] ?? '').map((cell) => {
    const left = cell.startsWith(':');
    const right = cell.endsWith(':');
    return left && right ? 'center' : right ? 'right' : left ? 'left' : null;
  });
  const width = head.length;
  const fit = (cells: string[]): Inline[][] =>
    Array.from({ length: width }, (_, k) => parseInline(cells[k] ?? ''));
  const rows: Inline[][][] = [];
  let i = at + 2;
  while (i < lines.length) {
    const l = lines[i] ?? '';
    if (isBlank(l) || !l.includes('|') || interruptsParagraph(l)) break;
    rows.push(fit(splitRow(l)));
    i += 1;
  }
  return [{ t: 'table', align, head: fit(head), rows }, i];
}

// ─── Inlines ────────────────────────────────────────────────────────────────────────────────────

interface Delim {
  t: 'delim';
  ch: '*' | '_' | '~';
  n: number;
  /** The run's length before any of it was used, for the "multiple of 3" rule. */
  orig: number;
  open: boolean;
  close: boolean;
}

type Node = Inline | Delim;

const PUNCT = /[!-/:-@[-`{-~\p{P}\p{S}]/u;
const WS = /\s/u;
const ENTITIES: Record<string, string> = {
  amp: '&',
  lt: '<',
  gt: '>',
  quot: '"',
  apos: "'",
  nbsp: ' ',
  copy: '©',
  reg: '®',
  hellip: '…',
  mdash: '—',
  ndash: '–',
};
const ENTITY = /&(#\d{1,7}|#[xX][0-9a-fA-F]{1,6}|[a-zA-Z]{2,8});/y;
const BARE_URL = /https?:\/\/[^\s<>]+/iy;
const AUTOLINK = /<([a-zA-Z][a-zA-Z0-9+.-]{1,31}:[^\s<>]*)>/y;
const AUTOMAIL = /<([^\s<>@\\]+@[a-zA-Z0-9](?:[a-zA-Z0-9-]*[a-zA-Z0-9])?(?:\.[a-zA-Z0-9](?:[a-zA-Z0-9-]*[a-zA-Z0-9])?)+)>/y;
/** How far a link's label may reach, so unmatched brackets stay linear. */
const MAX_LABEL = 1000;

/** What parsing one inline run keeps beside the text. */
interface Scan {
  /**
   * The scanning ahead the run may still do beyond reading it once: looking for a link's end and
   * for the opener of each emphasis closer. Real text needs a small multiple of its length;
   * hostile text (`a* b* c* …`, `[a]([a]([a](…`) would need the square of it. Once it is spent,
   * the rest of the run stays literal text: no more links or emphasis.
   */
  left: number;
  ticks: BacktickRuns;
}

const scanOf = (src: string): Scan => ({ left: 20_000 + 8 * src.length, ticks: backtickRuns(src) });

function runLength(src: string, i: number, ch: string): number {
  let n = 0;
  while (src[i + n] === ch) n += 1;
  return n;
}

/** Where each backtick run of a text starts, by the run's length, in order. */
type BacktickRuns = Map<number, number[]>;

function backtickRuns(src: string): BacktickRuns {
  const runs: BacktickRuns = new Map();
  let i = src.indexOf('`');
  while (i !== -1) {
    const n = runLength(src, i, '`');
    const starts = runs.get(n);
    if (starts === undefined) runs.set(n, [i]);
    else starts.push(i);
    i = src.indexOf('`', i + n);
  }
  return runs;
}

/**
 * The index of the first backtick run of exactly `n` at or after `from`, or -1. `from` is always
 * just past a run, so the runs after it are the ones `backtickRuns` found. A lookup, not a scan:
 * text with many unmatched runs stays linear.
 */
function backtickClose(runs: BacktickRuns, from: number, n: number): number {
  const starts = runs.get(n);
  if (starts === undefined) return -1;
  let lo = 0;
  let hi = starts.length;
  while (lo < hi) {
    const mid = (lo + hi) >>> 1;
    if ((starts[mid] ?? Infinity) < from) lo = mid + 1;
    else hi = mid;
  }
  return starts[lo] ?? -1;
}

function decodeEntity(name: string): string | undefined {
  if (name.startsWith('#')) {
    const code = name[1] === 'x' || name[1] === 'X' ? parseInt(name.slice(2), 16) : parseInt(name.slice(1), 10);
    if (!Number.isFinite(code) || code > 0x10ffff) return undefined;
    return code === 0 || (code >= 0xd800 && code <= 0xdfff) ? '�' : String.fromCodePoint(code);
  }
  return ENTITIES[name];
}

function matchAt(re: RegExp, src: string, i: number): RegExpExecArray | null {
  re.lastIndex = i;
  return re.exec(src);
}

/** Drops trailing punctuation a bare URL is unlikely to end with, and unbalanced `)`. */
function trimUrl(url: string): string {
  // Trimming never removes a `(`, so the counts are kept as it goes rather than recounted.
  const opens = url.match(/\(/g)?.length ?? 0;
  let closes = url.match(/\)/g)?.length ?? 0;
  let end = url.length;
  for (;;) {
    const last = url[end - 1];
    if (last !== undefined && '?!.,:;*_~\'"'.includes(last)) {
      end -= 1;
    } else if (last === ')' && opens < closes) {
      end -= 1;
      closes -= 1;
    } else break;
  }
  return url.slice(0, end);
}

interface LinkParts {
  label: string;
  dest: string;
  end: number;
}

/**
 * `[label](dest "title")` starting at the `[` at `i`. What it reads past the label is charged to
 * the scan; with nothing left, nothing more is a link.
 */
function linkAt(src: string, i: number, scan: Scan): LinkParts | undefined {
  if (scan.left <= 0) return undefined;
  let depth = 0;
  let k = i;
  const limit = Math.min(src.length, i + MAX_LABEL);
  for (; k < limit; k++) {
    const ch = src[k];
    if (ch === '\\') {
      k += 1;
    } else if (ch === '`') {
      const run = runLength(src, k, '`');
      const close = backtickClose(scan.ticks, k + run, run);
      k = close === -1 ? k + run - 1 : close + run - 1;
    } else if (ch === '[') {
      depth += 1;
    } else if (ch === ']') {
      depth -= 1;
      if (depth === 0) break;
    }
  }
  if (k >= limit || src[k] !== ']' || src[k + 1] !== '(') return undefined;
  const label = src.slice(i + 1, k);
  const from = k + 2;
  let far = from;
  try {
    const tail = linkTail(src, from, (to) => {
      far = Math.max(far, to);
    });
    return tail === undefined ? undefined : { label, ...tail };
  } finally {
    scan.left -= far - from;
  }
}

/** The `(dest "title")` part of a link, from just after its `(`; `reached` hears how far it read. */
function linkTail(src: string, from: number, reached: (to: number) => void): { dest: string; end: number } | undefined {
  let p = from;
  const skipSpace = () => {
    while (p < src.length && /[ \t\n]/.test(src[p] ?? '')) p += 1;
    reached(p);
  };
  skipSpace();
  let dest: string;
  if (src[p] === '<') {
    const close = src.indexOf('>', p);
    reached(close === -1 ? src.length : close);
    if (close === -1 || src.slice(p, close).includes('\n')) return undefined;
    dest = src.slice(p + 1, close);
    p = close + 1;
  } else {
    const start = p;
    let parens = 0;
    while (p < src.length) {
      const ch = src[p] ?? '';
      if (ch === '\\' && p + 1 < src.length) {
        p += 2;
        continue;
      }
      if (ch === '(') parens += 1;
      else if (ch === ')') {
        if (parens === 0) break;
        parens -= 1;
      } else if (/\s/.test(ch)) break;
      p += 1;
    }
    reached(p);
    dest = src.slice(start, p).replace(/\\([!-/:-@[-`{-~])/g, '$1');
  }
  skipSpace();
  const quote = src[p];
  if (quote === '"' || quote === "'" || quote === '(') {
    const close = src.indexOf(quote === '(' ? ')' : quote, p + 1);
    reached(close === -1 ? src.length : close);
    if (close === -1) return undefined;
    p = close + 1;
    skipSpace();
  }
  if (src[p] !== ')') return undefined;
  return { dest, end: p + 1 };
}

export function plainText(inlines: readonly Inline[]): string {
  return inlines
    .map((node) => {
      switch (node.t) {
        case 'text':
        case 'code':
          return node.v;
        case 'br':
          return '\n';
        case 'img':
          return node.alt;
        default:
          return plainText(node.c);
      }
    })
    .join('');
}

export function parseInline(src: string, depth = 0): Inline[] {
  if (depth > MAX_DEPTH) return src === '' ? [] : [{ t: 'text', v: src }];
  const scan = scanOf(src);
  const nodes: Node[] = [];
  let text = '';
  const flush = () => {
    if (text !== '') nodes.push({ t: 'text', v: text });
    text = '';
  };
  let i = 0;
  while (i < src.length) {
    const ch = src[i] ?? '';

    if (ch === '\\') {
      const next = src[i + 1];
      if (next === '\n') {
        flush();
        nodes.push({ t: 'br' });
        i += 2;
      } else if (next !== undefined && /[!-/:-@[-`{-~]/.test(next)) {
        text += next;
        i += 2;
      } else {
        text += ch;
        i += 1;
      }
      continue;
    }

    if (ch === '`') {
      const run = runLength(src, i, '`');
      const close = backtickClose(scan.ticks, i + run, run);
      if (close === -1) {
        text += '`'.repeat(run);
        i += run;
        continue;
      }
      let code = src.slice(i + run, close).replace(/\n/g, ' ');
      if (code.length > 2 && code.startsWith(' ') && code.endsWith(' ') && code.trim() !== '') {
        code = code.slice(1, -1);
      }
      flush();
      nodes.push({ t: 'code', v: code });
      i = close + run;
      continue;
    }

    if (ch === '\n') {
      text = text.replace(/[ \t]+$/, '');
      flush();
      nodes.push({ t: 'br' });
      i += 1;
      while (src[i] === ' ' || src[i] === '\t') i += 1;
      continue;
    }

    if (ch === '!' && src[i + 1] === '[') {
      const link = linkAt(src, i + 1, scan);
      if (link !== undefined) {
        flush();
        nodes.push({ t: 'img', src: link.dest, alt: plainText(parseInline(link.label, depth + 1)) });
        i = link.end;
        continue;
      }
    }

    if (ch === '[') {
      const link = linkAt(src, i, scan);
      if (link !== undefined) {
        flush();
        nodes.push(parent({ t: 'link', href: link.dest, c: parseInline(link.label, depth + 1) }));
        i = link.end;
        continue;
      }
    }

    if (ch === '<') {
      const auto = matchAt(AUTOLINK, src, i);
      const mail = auto === null ? matchAt(AUTOMAIL, src, i) : null;
      const found = auto ?? mail;
      if (found !== null) {
        const target = found[1] ?? '';
        flush();
        nodes.push({ t: 'link', href: mail !== null ? `mailto:${target}` : target, c: [{ t: 'text', v: target }] });
        i += found[0].length;
        continue;
      }
    }

    if ((ch === 'h' || ch === 'H') && !/[\p{L}\p{N}]/u.test(src[i - 1] ?? ' ')) {
      const url = matchAt(BARE_URL, src, i);
      if (url !== null) {
        const trimmed = trimUrl(url[0]);
        if (/^https?:\/\/[^/?#\s]/i.test(trimmed)) {
          flush();
          nodes.push({ t: 'link', href: trimmed, c: [{ t: 'text', v: trimmed }] });
          i += trimmed.length;
          continue;
        }
      }
    }

    if (ch === '*' || ch === '_' || (ch === '~' && src[i + 1] === '~')) {
      const n = runLength(src, i, ch);
      if (ch === '~' && n !== 2) {
        text += ch.repeat(n);
        i += n;
        continue;
      }
      const before = src[i - 1] ?? ' ';
      const after = src[i + n] ?? ' ';
      const left = !WS.test(after) && (!PUNCT.test(after) || WS.test(before) || PUNCT.test(before));
      const right = !WS.test(before) && (!PUNCT.test(before) || WS.test(after) || PUNCT.test(after));
      const open = ch === '_' ? left && (!right || PUNCT.test(before)) : left;
      const close = ch === '_' ? right && (!left || PUNCT.test(after)) : right;
      flush();
      nodes.push({ t: 'delim', ch, n, orig: n, open, close });
      i += n;
      continue;
    }

    if (ch === '&') {
      const entity = matchAt(ENTITY, src, i);
      const decoded = entity === null ? undefined : decodeEntity(entity[1] ?? '');
      if (entity !== null && decoded !== undefined) {
        text += decoded;
        i += entity[0].length;
        continue;
      }
    }

    text += ch;
    i += 1;
  }
  flush();
  return finish(emphasis(nodes, scan));
}

/** How deep each inline with children nests, so emphasis cannot nest past `MAX_DEPTH`. */
const nesting = new WeakMap<Inline, number>();

function depthOf(nodes: readonly Inline[]): number {
  let max = 0;
  for (const node of nodes) max = Math.max(max, nesting.get(node) ?? 0);
  return max;
}

function parent<T extends Inline & { c: Inline[] }>(node: T): T {
  nesting.set(node, 1 + depthOf(node.c));
  return node;
}

/** The nearest opener on `stack` that `closer` can close, or -1. Each node looked at costs 1. */
function findOpener(stack: readonly Node[], closer: Delim, scan: Scan): number {
  for (let o = stack.length - 1; o >= 0 && scan.left > 0; o--) {
    scan.left -= 1;
    const node = stack[o];
    if (node?.t !== 'delim' || node.ch !== closer.ch || !node.open || node.n === 0) continue;
    // CommonMark's rule of 3, so `*foo**bar*` parses as it should.
    const either = node.close || closer.open;
    if (
      closer.ch !== '~' &&
      either &&
      (node.orig + closer.orig) % 3 === 0 &&
      !(node.orig % 3 === 0 && closer.orig % 3 === 0)
    ) {
      continue;
    }
    return o;
  }
  return -1;
}

/**
 * Pairs delimiter runs into emphasis, strong and strikethrough, innermost first. What precedes the
 * closer in hand is a stack, so a match replaces the stack's top instead of splicing the middle of
 * an array. Looking back for openers is charged to the scan: once it runs out, the remaining
 * delimiters stay text. So does a pair that would nest deeper than `MAX_DEPTH`.
 */
function emphasis(nodes: readonly Node[], scan: Scan): Node[] {
  const stack: Node[] = [];
  for (const node of nodes) {
    if (node.t !== 'delim' || !node.close) {
      stack.push(node);
      continue;
    }
    const closer = node;
    // A closer with some of its run left may close another opener further out.
    while (closer.n > 0 && scan.left > 0) {
      const o = findOpener(stack, closer, scan);
      const opener = stack[o];
      if (opener?.t !== 'delim' || (closer.ch === '~' && opener.n !== closer.n)) break;
      const use = closer.ch === '~' ? closer.n : opener.n >= 2 && closer.n >= 2 ? 2 : 1;
      const type = closer.ch === '~' ? 'del' : use === 2 ? 'strong' : 'em';
      const inner = finish(stack.splice(o + 1));
      scan.left -= inner.length;
      opener.n -= use;
      closer.n -= use;
      if (opener.n === 0) stack.pop();
      if (depthOf(inner) < MAX_DEPTH) {
        stack.push(parent({ t: type, c: inner }));
      } else {
        stack.push({ t: 'text', v: closer.ch.repeat(use) });
        for (const child of inner) stack.push(child);
        stack.push({ t: 'text', v: closer.ch.repeat(use) });
      }
    }
    if (closer.n > 0) stack.push(closer);
  }
  return stack;
}

/** Unused delimiters become text; adjacent text merges. */
function finish(nodes: readonly Node[]): Inline[] {
  const out: Inline[] = [];
  for (const node of nodes) {
    const inline: Inline | undefined =
      node.t === 'delim' ? (node.n > 0 ? { t: 'text', v: node.ch.repeat(node.n) } : undefined) : node;
    if (inline === undefined) continue;
    const last = out.at(-1);
    if (inline.t === 'text' && last?.t === 'text') out[out.length - 1] = { t: 'text', v: last.v + inline.v };
    else out.push(inline);
  }
  return out;
}
