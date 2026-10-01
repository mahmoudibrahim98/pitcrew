import { describe, expect, it } from 'vitest';
import { parseInline, parseMarkdown, plainText, type Block, type Inline } from '../render/markdown-parse.ts';
import { safeHref } from '../render/links.tsx';

const text = (v: string): Inline => ({ t: 'text', v });

describe('parseMarkdown blocks', () => {
  it('parses headings, paragraphs, rules and fenced code', () => {
    const blocks = parseMarkdown('# Title\n\nSome *text*.\n\n---\n\n```ts\nconst x = 1;\n```');
    expect(blocks).toEqual<Block[]>([
      { t: 'h', level: 1, c: [text('Title')] },
      { t: 'p', c: [text('Some '), { t: 'em', c: [text('text')] }, text('.')] },
      { t: 'hr' },
      { t: 'code', lang: 'ts', v: 'const x = 1;' },
    ]);
  });

  it('keeps an unterminated fence to the end, and tilde fences', () => {
    expect(parseMarkdown('~~~\na\n```\nb')).toEqual([{ t: 'code', lang: '', v: 'a\n```\nb' }]);
  });

  it('parses setext headings', () => {
    expect(parseMarkdown('Title\n===\nSub\n---')).toEqual([
      { t: 'h', level: 1, c: [text('Title')] },
      { t: 'h', level: 2, c: [text('Sub')] },
    ]);
  });

  it('parses nested and task lists, tight and loose', () => {
    const [list] = parseMarkdown('- [x] done\n- [ ] todo\n  - nested\n- plain');
    expect(list).toMatchObject({
      t: 'list',
      ordered: false,
      tight: true,
      items: [
        { check: true, c: [{ t: 'p', c: [text('done')] }] },
        {
          check: false,
          c: [
            { t: 'p', c: [text('todo')] },
            { t: 'list', items: [{ c: [{ t: 'p', c: [text('nested')] }] }] },
          ],
        },
        { check: null },
      ],
    });
    const [loose] = parseMarkdown('1. one\n\n2. two');
    expect(loose).toMatchObject({ t: 'list', ordered: true, start: 1, tight: false });
    expect(parseMarkdown('3) three')[0]).toMatchObject({ t: 'list', ordered: true, start: 3 });
  });

  it('parses quotes and tables', () => {
    expect(parseMarkdown('> quoted\n> **bold**')).toEqual([
      { t: 'quote', c: [{ t: 'p', c: [text('quoted'), { t: 'br' }, { t: 'strong', c: [text('bold')] }] }] },
    ]);
    const [table] = parseMarkdown('| a | b |\n|:--|--:|\n| 1 | `2` |\n| 3 |');
    expect(table).toEqual({
      t: 'table',
      align: ['left', 'right'],
      head: [[text('a')], [text('b')]],
      rows: [
        [[text('1')], [{ t: 'code', v: '2' }]],
        [[text('3')], []],
      ],
    });
  });

  it('keeps raw HTML as text', () => {
    const source = '<img src=x onerror=alert(1)>\n<script>alert(1)</script>';
    const blocks = parseMarkdown(source);
    expect(blocks).toHaveLength(1);
    expect(plainText((blocks[0] as { c: Inline[] }).c)).toBe(source);
  });

  it('survives deep nesting', () => {
    const deep = '>'.repeat(5_000) + ' x';
    expect(() => parseMarkdown(deep)).not.toThrow();
    const brackets = '['.repeat(20_000);
    expect(plainText(parseInline(brackets))).toBe(brackets);
  });
});

describe('parseInline', () => {
  it('pairs emphasis, strong and strikethrough', () => {
    expect(parseInline('**a** _b_ ~~c~~ ***d***')).toEqual<Inline[]>([
      { t: 'strong', c: [text('a')] },
      text(' '),
      { t: 'em', c: [text('b')] },
      text(' '),
      { t: 'del', c: [text('c')] },
      text(' '),
      { t: 'em', c: [{ t: 'strong', c: [text('d')] }] },
    ]);
  });

  it('leaves intraword underscores, single tildes and unmatched stars alone', () => {
    expect(parseInline('snake_case_name ~/path 2 * 3')).toEqual([text('snake_case_name ~/path 2 * 3')]);
  });

  it('parses code spans before anything else', () => {
    expect(parseInline('`a *b* <c>` and `` d ` e ``')).toEqual([
      { t: 'code', v: 'a *b* <c>' },
      text(' and '),
      { t: 'code', v: 'd ` e' },
    ]);
  });

  it('parses links, images, autolinks and bare URLs', () => {
    expect(parseInline('[docs](https://example.com/a_(b) "t") ![logo](https://x.test/l.png)')).toEqual([
      { t: 'link', href: 'https://example.com/a_(b)', c: [text('docs')] },
      text(' '),
      { t: 'img', src: 'https://x.test/l.png', alt: 'logo' },
    ]);
    expect(parseInline('see <https://a.test/x> or https://b.test/y.')).toEqual([
      text('see '),
      { t: 'link', href: 'https://a.test/x', c: [text('https://a.test/x')] },
      text(' or '),
      { t: 'link', href: 'https://b.test/y', c: [text('https://b.test/y')] },
      text('.'),
    ]);
    expect(parseInline('<sam@example.com>')).toEqual([
      { t: 'link', href: 'mailto:sam@example.com', c: [text('sam@example.com')] },
    ]);
  });

  it('decodes entities and escapes into text', () => {
    expect(parseInline('\\*not em\\* &amp; &lt;b&gt; &#65;')).toEqual([text('*not em* & <b> A')]);
  });

  it('turns newlines into line breaks', () => {
    expect(parseInline('a  \nb')).toEqual([text('a'), { t: 'br' }, text('b')]);
  });
});

describe('parseInline on hostile input', () => {
  // Each of these took seconds to minutes while matching was quadratic (at 50,000 units the
  // closers about 15 s, the link openings about 3 minutes). Linear, each takes tens of
  // milliseconds; the bound leaves room for a busy test machine.
  const BOUND_MS = 1_500;
  const N = 50_000;

  function timed(source: string): { out: Inline[]; ms: number } {
    const started = performance.now();
    const out = parseInline(source);
    return { out, ms: performance.now() - started };
  }

  function depth(nodes: readonly Inline[]): number {
    let max = 0;
    for (const node of nodes) {
      if ('c' in node) max = Math.max(max, 1 + depth(node.c));
    }
    return max;
  }

  it('stays linear on tens of thousands of closers with no opener, which stay text', () => {
    const source = 'a* b_ c** '.repeat(N);
    const { out, ms } = timed(source);
    expect(ms).toBeLessThan(BOUND_MS);
    expect(out).toEqual([text(source)]);
  });

  it('stays linear on tens of thousands of pairs', () => {
    const { out, ms } = timed('*a* '.repeat(N));
    expect(ms).toBeLessThan(BOUND_MS);
    expect(out.filter((node) => node.t === 'em')).toHaveLength(N);
  });

  it('stays linear on link openings that never close, and on trailing parentheses', () => {
    const tails = '[a]('.repeat(N);
    const first = timed(tails);
    expect(first.ms).toBeLessThan(BOUND_MS);
    expect(first.out).toEqual([text(tails)]);

    const url = `see https://a.test/x${')'.repeat(N)}`;
    const second = timed(url);
    expect(second.ms).toBeLessThan(BOUND_MS);
    expect(second.out[1]).toEqual({ t: 'link', href: 'https://a.test/x', c: [text('https://a.test/x')] });
    expect(plainText(second.out)).toBe(url);
  });

  it('stays linear on many backtick runs that never close, and still pairs code spans', () => {
    const source = Array.from({ length: 1_000 }, (_, i) => '`'.repeat(1_000 - i)).join('x');
    const { out, ms } = timed(source);
    expect(ms).toBeLessThan(BOUND_MS);
    expect(out).toEqual([text(source)]);
    expect(parseInline('`a` ``b`` ```c``` `d`')).toEqual([
      { t: 'code', v: 'a' },
      text(' '),
      { t: 'code', v: 'b' },
      text(' '),
      { t: 'code', v: 'c' },
      text(' '),
      { t: 'code', v: 'd' },
    ]);
  });

  it('keeps emphasis nested deeper than 24 levels as text, so the tree stays shallow', () => {
    const source = `${'*a '.repeat(5_000)}${'a* '.repeat(5_000)}`.trimEnd();
    const { out, ms } = timed(source);
    expect(ms).toBeLessThan(BOUND_MS);
    expect(depth(out)).toBeLessThanOrEqual(24);
    expect(() => structuredClone(out)).not.toThrow();
    const nested = parseInline(`${'*a '.repeat(24)}x${' a*'.repeat(24)}`);
    expect(depth(nested)).toBe(24);
  });
});

describe('safeHref', () => {
  it('allows http, https and mailto only', () => {
    expect(safeHref('https://example.com/x')).toBe('https://example.com/x');
    expect(safeHref('mailto:sam@example.com')).toBe('mailto:sam@example.com');
    for (const bad of [
      'javascript:alert(1)',
      ' JavaScript:alert(1)',
      'java\tscript:alert(1)',
      'java\nscript:alert(1)',
      'data:text/html,<script>alert(1)</script>',
      'vbscript:msgbox(1)',
      'file:///etc/passwd',
      '/relative/path',
      'notes.md',
    ]) {
      expect(safeHref(bad), bad).toBeUndefined();
    }
  });
});
