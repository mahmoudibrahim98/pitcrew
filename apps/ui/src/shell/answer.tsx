// An Orchestrator answer, shown as text. The answer comes from an agent CLI that read what other
// people and agents wrote, so it is untrusted: nothing in it is ever markup. This renders a small,
// safe subset of Markdown as React elements (paragraphs, lists, code blocks, `code` and **strong**)
// and turns the references the hub checked into links to app routes. Every other link stays text:
// no URL from an answer is ever opened.

import type { MouseEvent, ReactNode } from 'react';
import type { AnswerReference, ReferenceTarget } from '../data/index.ts';
import { cx } from '../lib/cx.ts';
import { paths } from './paths.ts';

/** The app route a reference opens. */
export function referencePath(ws: string, target: ReferenceTarget): string {
  switch (target.kind) {
    case 'session':
      return paths.session(ws, target.id);
    case 'task':
      return paths.task(ws, target.key);
    case 'workstream':
      return paths.workstream(ws, target.project, target.id);
    case 'project':
      return paths.project(ws, target.id);
    case 'recap':
      return target.workstream === undefined
        ? paths.project(ws, target.project)
        : paths.workstream(ws, target.project, target.workstream);
  }
}

/** What a link to a reference says: a task by its key, anything else by its name. */
export function referenceText(reference: AnswerReference): string {
  return reference.target.kind === 'task' ? reference.text : reference.label;
}

export type Inline =
  | { t: 'text'; v: string }
  | { t: 'code'; v: string }
  | { t: 'strong'; c: Inline[] }
  | { t: 'ref'; ref: AnswerReference; label?: string };

export type Block =
  | { t: 'p'; lines: Inline[][] }
  | { t: 'list'; ordered: boolean; items: Inline[][] }
  | { t: 'code'; v: string }
  | { t: 'heading'; c: Inline[] };

/**
 * Words as the hub finds references in them: runs of these characters, trimmed of `-`, `:` and
 * `@` at either end (`PAP-1:` is `PAP-1`). A reference is a whole word, never part of one.
 */
const WORD = /[A-Za-z0-9_:@-]+/g;
const EDGES = /^([-:@]*)(.*?)([-:@]*)$/s;

/** Plain text with the references in it turned into links. */
function withRefs(text: string, refs: readonly AnswerReference[]): Inline[] {
  const out: Inline[] = [];
  let plain = '';
  let last = 0;
  for (const match of text.matchAll(WORD)) {
    const [, lead = '', core = '', trail = ''] = EDGES.exec(match[0]) ?? [];
    const ref = core === '' ? undefined : refs.find((r) => r.text === core);
    if (ref === undefined) continue;
    plain += text.slice(last, match.index) + lead;
    if (plain !== '') out.push({ t: 'text', v: plain });
    out.push({ t: 'ref', ref });
    plain = trail;
    last = match.index + match[0].length;
  }
  plain += text.slice(last);
  if (plain !== '') out.push({ t: 'text', v: plain });
  return out;
}

const LINK = /^\[([^\]\n]{1,200})\]\(([^)\s]{1,300})\)/;

/** One line's inlines: `code`, **strong**, `[label](reference)`, and references. */
export function inlines(text: string, refs: readonly AnswerReference[], depth = 0): Inline[] {
  const out: Inline[] = [];
  let rest = '';
  const flush = () => {
    if (rest !== '') out.push(...withRefs(rest, refs));
    rest = '';
  };
  let i = 0;
  while (i < text.length) {
    const ch = text[i];
    if (ch === '`') {
      const end = text.indexOf('`', i + 1);
      if (end > i + 1) {
        flush();
        const code = text.slice(i + 1, end);
        // A reference in a code span is still a link: agents put ids in backticks.
        const ref = refs.find((r) => r.text === code.trim());
        out.push(ref === undefined ? { t: 'code', v: code } : { t: 'ref', ref });
        i = end + 1;
        continue;
      }
    }
    if (ch === '*' && text[i + 1] === '*' && depth < 2) {
      const end = text.indexOf('**', i + 2);
      if (end > i + 2) {
        flush();
        out.push({ t: 'strong', c: inlines(text.slice(i + 2, end), refs, depth + 1) });
        i = end + 2;
        continue;
      }
    }
    if (ch === '[') {
      const link = LINK.exec(text.slice(i));
      if (link !== null) {
        const [whole, label = '', target = ''] = link;
        const ref = refs.find((r) => r.text === target);
        flush();
        if (ref !== undefined) out.push({ t: 'ref', ref, label });
        // Any other link stays text: its label, then where it pointed.
        else out.push(...withRefs(`${label} (${target})`, refs));
        i += whole.length;
        continue;
      }
    }
    rest += ch;
    i += 1;
  }
  flush();
  return out;
}

const BULLET = /^\s{0,6}[-*+]\s+(.*)$/;
const ORDERED = /^\s{0,6}\d{1,3}[.)]\s+(.*)$/;
const HEADING = /^\s{0,3}#{1,6}\s+(.*)$/;
const FENCE = /^\s{0,3}(```|~~~)/;

/** The answer's blocks. */
export function blocks(text: string, refs: readonly AnswerReference[]): Block[] {
  const out: Block[] = [];
  const lines = text.replace(/\r\n?/g, '\n').split('\n');
  let paragraph: Inline[][] = [];
  let list: { ordered: boolean; items: Inline[][] } | undefined;
  const endParagraph = () => {
    if (paragraph.length > 0) out.push({ t: 'p', lines: paragraph });
    paragraph = [];
  };
  const endList = () => {
    if (list !== undefined) out.push({ t: 'list', ...list });
    list = undefined;
  };
  for (let i = 0; i < lines.length; i += 1) {
    const line = lines[i] ?? '';
    const fence = FENCE.exec(line);
    if (fence !== null) {
      endParagraph();
      endList();
      const body: string[] = [];
      i += 1;
      while (i < lines.length && !(lines[i] ?? '').trimStart().startsWith(fence[1] ?? '```')) {
        body.push(lines[i] ?? '');
        i += 1;
      }
      out.push({ t: 'code', v: body.join('\n') });
      continue;
    }
    if (line.trim() === '') {
      endParagraph();
      endList();
      continue;
    }
    const heading = HEADING.exec(line);
    if (heading !== null) {
      endParagraph();
      endList();
      out.push({ t: 'heading', c: inlines(heading[1] ?? '', refs) });
      continue;
    }
    const bullet = BULLET.exec(line);
    const ordered = bullet === null ? ORDERED.exec(line) : null;
    const item = bullet ?? ordered;
    if (item !== null) {
      endParagraph();
      const isOrdered = ordered !== null;
      if (list !== undefined && list.ordered !== isOrdered) endList();
      list ??= { ordered: isOrdered, items: [] };
      list.items.push(inlines(item[1] ?? '', refs));
      continue;
    }
    if (list !== undefined && /^\s{2,}\S/.test(line)) {
      // A continuation of the list item above.
      const last = list.items.at(-1);
      if (last !== undefined) {
        last.push({ t: 'text', v: ' ' }, ...inlines(line.trim(), refs));
        continue;
      }
    }
    endList();
    paragraph.push(inlines(line, refs));
  }
  endParagraph();
  endList();
  return out;
}

const REF_LINK = 'rounded-sm font-medium text-accent-text underline underline-offset-2 hover:text-ink';

function renderInlines(nodes: readonly Inline[], ws: string, open: (path: string) => void): ReactNode[] {
  return nodes.map((node, i) => {
    switch (node.t) {
      case 'text':
        return node.v;
      case 'code':
        return (
          <code key={i} className="rounded-sm bg-sunken px-1 py-px font-mono text-[0.9em]">
            {node.v}
          </code>
        );
      case 'strong':
        return (
          <strong key={i} className="font-semibold">
            {renderInlines(node.c, ws, open)}
          </strong>
        );
      case 'ref': {
        const path = referencePath(ws, node.ref.target);
        return (
          <a
            key={i}
            href={path}
            title={`${node.ref.label} (${node.ref.text})`}
            className={REF_LINK}
            onClick={(e: MouseEvent) => {
              if (e.metaKey || e.ctrlKey || e.shiftKey || e.button !== 0) return;
              e.preventDefault();
              open(path);
            }}
          >
            {node.label ?? referenceText(node.ref)}
          </a>
        );
      }
    }
  });
}

/** An answer's text, with its references as links (see the module's comment). */
export function Answer({
  text,
  references,
  ws,
  open,
  className,
}: {
  text: string;
  references: readonly AnswerReference[];
  ws: string;
  /** Opens an app route (the router's `navigate`). */
  open: (path: string) => void;
  className?: string;
}) {
  return (
    <div className={cx('flex flex-col gap-2 text-sm leading-relaxed break-words', className)}>
      {blocks(text, references).map((block, i) => {
        switch (block.t) {
          case 'p':
            return (
              <p key={i}>
                {block.lines.map((line, j) => (
                  <span key={j} className="block">
                    {renderInlines(line, ws, open)}
                  </span>
                ))}
              </p>
            );
          case 'heading':
            return (
              <p key={i} className="font-semibold">
                {renderInlines(block.c, ws, open)}
              </p>
            );
          case 'code':
            return (
              <pre key={i} className="overflow-x-auto rounded-sm bg-sunken px-2 py-1.5 font-mono text-xs">
                {block.v}
              </pre>
            );
          case 'list': {
            const List = block.ordered ? 'ol' : 'ul';
            return (
              <List key={i} className={cx('flex flex-col gap-1 pl-5', block.ordered ? 'list-decimal' : 'list-disc')}>
                {block.items.map((item, j) => (
                  <li key={j}>{renderInlines(item, ws, open)}</li>
                ))}
              </List>
            );
          }
        }
      })}
    </div>
  );
}
