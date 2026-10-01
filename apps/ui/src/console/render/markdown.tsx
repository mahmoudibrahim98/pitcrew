// Renders parsed markdown as React elements. There is no HTML string anywhere on this path:
// text is always a text node, and a link is made only for an http, https or mailto target, and
// opens outside the app.

import type { ReactNode } from 'react';
import { cx } from '../../lib/cx.ts';
import { useParsed } from './client.ts';
import { ExternalLink, safeHref } from './links.tsx';
import type { Block, Inline } from './markdown-parse.ts';

function renderInlines(nodes: readonly Inline[], inLink: boolean): ReactNode[] {
  return nodes.map((node, i) => renderInline(node, i, inLink));
}

function renderInline(node: Inline, key: number, inLink: boolean): ReactNode {
  switch (node.t) {
    case 'text':
      return node.v;
    case 'br':
      return <br key={key} />;
    case 'code':
      return (
        <code key={key} className="rounded-sm bg-sunken px-1 py-px font-mono text-[0.9em]">
          {node.v}
        </code>
      );
    case 'em':
      return <em key={key}>{renderInlines(node.c, inLink)}</em>;
    case 'strong':
      return (
        <strong key={key} className="font-semibold">
          {renderInlines(node.c, inLink)}
        </strong>
      );
    case 'del':
      return <del key={key}>{renderInlines(node.c, inLink)}</del>;
    case 'link': {
      const href = inLink ? undefined : safeHref(node.href);
      const label = renderInlines(node.c, true);
      return href === undefined ? (
        <span key={key}>{label}</span>
      ) : (
        <ExternalLink key={key} href={href}>
          {label}
        </ExternalLink>
      );
    }
    case 'img': {
      // Remote images are never loaded; the image is offered as a link.
      const href = inLink ? undefined : safeHref(node.src);
      const label = `[image: ${node.alt === '' ? 'untitled' : node.alt}]`;
      return href === undefined ? (
        <span key={key}>{label}</span>
      ) : (
        <ExternalLink key={key} href={href}>
          {label}
        </ExternalLink>
      );
    }
  }
}

const HEADING_SIZE = ['text-lg', 'text-md', 'text-md', 'text-sm', 'text-sm', 'text-sm'] as const;
const ALIGN = { left: 'text-left', center: 'text-center', right: 'text-right' } as const;

function Heading({ level, children }: { level: number; children: ReactNode }) {
  // Levels start at h3 so a message never outranks the page's own headings.
  const Tag = (['h3', 'h4', 'h5', 'h6', 'h6', 'h6'] as const)[level - 1] ?? 'h6';
  return <Tag className={cx('mt-3 mb-1 font-semibold first:mt-0', HEADING_SIZE[level - 1])}>{children}</Tag>;
}

function renderBlocks(blocks: readonly Block[], tight = false): ReactNode[] {
  return blocks.map((block, i) => renderBlock(block, i, tight));
}

function renderBlock(block: Block, key: number, tight: boolean): ReactNode {
  switch (block.t) {
    case 'p':
      return tight ? (
        <span key={key} className="block">
          {renderInlines(block.c, false)}
        </span>
      ) : (
        <p key={key} className="my-2 first:mt-0 last:mb-0">
          {renderInlines(block.c, false)}
        </p>
      );
    case 'h':
      return (
        <Heading key={key} level={block.level}>
          {renderInlines(block.c, false)}
        </Heading>
      );
    case 'code': {
      const lang = block.lang.replace(/[^\w+#.-]/g, '').slice(0, 32);
      return (
        <div key={key} className="my-2 overflow-hidden rounded-md border border-line bg-sunken first:mt-0 last:mb-0">
          {lang !== '' && (
            <div className="border-b border-line px-2 py-0.5 font-mono text-xs text-muted">{lang}</div>
          )}
          <pre className="overflow-x-auto px-3 py-2 font-mono text-xs leading-5">
            <code>{block.v}</code>
          </pre>
        </div>
      );
    }
    case 'quote':
      return (
        <blockquote key={key} className="my-2 border-l-2 border-line-2 pl-3 text-ink-2">
          {renderBlocks(block.c)}
        </blockquote>
      );
    case 'list': {
      const items = block.items.map((item, i) => (
        <li key={i} className={cx(item.check !== null && 'list-none', !block.tight && 'my-1')}>
          {item.check !== null && (
            <input
              type="checkbox"
              checked={item.check}
              readOnly
              disabled
              aria-label={item.check ? 'Done' : 'Not done'}
              className="mr-1.5 -ml-5 align-middle"
            />
          )}
          {renderBlocks(item.c, block.tight)}
        </li>
      ));
      return block.ordered ? (
        <ol key={key} start={block.start} className="my-2 list-decimal pl-6">
          {items}
        </ol>
      ) : (
        <ul key={key} className="my-2 list-disc pl-6">
          {items}
        </ul>
      );
    }
    case 'table':
      return (
        <div key={key} className="my-2 overflow-x-auto">
          <table className="border-collapse text-sm">
            <thead>
              <tr>
                {block.head.map((cell, i) => (
                  <th
                    key={i}
                    className={cx('border border-line bg-sunken px-2 py-1 font-semibold', ALIGN[block.align[i] ?? 'left'])}
                  >
                    {renderInlines(cell, false)}
                  </th>
                ))}
              </tr>
            </thead>
            <tbody>
              {block.rows.map((row, r) => (
                <tr key={r}>
                  {row.map((cell, i) => (
                    <td key={i} className={cx('border border-line px-2 py-1', ALIGN[block.align[i] ?? 'left'])}>
                      {renderInlines(cell, false)}
                    </td>
                  ))}
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      );
    case 'hr':
      return <hr key={key} className="my-3 border-line" />;
  }
}

/** Parsed blocks as elements; exported for tests and for callers that parse themselves. */
export function MarkdownBlocks({ blocks }: { blocks: readonly Block[] }) {
  return <>{renderBlocks(blocks)}</>;
}

/** Markdown text. Until the worker has parsed it, the raw text shows as plain text. */
export function Markdown({ text, className }: { text: string; className?: string }) {
  const blocks = useParsed('markdown', text);
  return (
    <div className={cx('min-w-0 break-words', className)} data-markdown={blocks === undefined ? 'pending' : 'ready'}>
      {blocks === undefined ? <p className="whitespace-pre-wrap">{text}</p> : renderBlocks(blocks)}
    </div>
  );
}
