// The file viewer's syntax colouring: which files it colours, and what it makes of them.

import { describe, expect, it } from 'vitest';
import { HIGHLIGHT_LIMIT, highlightLines, languageFor, tokenize, toLines, type Token } from '../highlight.ts';

/** The coloured pieces of `text` as `kind:text`, plain text left out. */
function marks(path: string, text: string): string[] {
  const language = languageFor(path);
  if (language === undefined) throw new Error(`no language for ${path}`);
  return tokenize(text, language)
    .filter((t) => t.kind !== null)
    .map((t) => `${t.kind}:${t.text}`);
}

const SAMPLES: Record<string, string> = {
  'a.ts': 'const answer: number = 42; // the answer\nfunction greet(name: string) { return `hi ${name}`; }\n/* done */',
  'b.rs': '#[derive(Debug)]\nfn main<\'a>(x: &\'a str) { let c = \'x\'; println!("{}", r#"raw "quoted""#); }',
  'c.py': '@cache\ndef f(x):\n    """Doc\n    string"""\n    return f"{x}" if x else None  # note',
  'd.json': '{"key": "value", "n": -1.5e3, "ok": true, "nothing": null}',
  'e.yaml': '# comment\n---\nname: demo\nlist:\n  - item: 1\n    flag: yes\nanchor: &a value',
  'f.toml': '[package]\nname = "pitcrew" # trailing\nversion = 1\n[[bin]]\npath = \'src/main.rs\'',
  'g.sh': '#!/bin/sh\nif [ -n "$HOME" ]; then echo ${USER} $#; fi # done',
  'h.md': '# Title\n\nSome `code` and **bold** and [a link](https://example.com).\n\n```ts\nconst x = 1;\n```\n> quoted\n- item',
  'i.tex': '\\section{Method} % comment\nWe use $x^2$ and \\% literal.\n$$\\int_0^1 f$$',
  'j.html': '<!-- note --><div class="a" data-x=\'b\'>Hi &amp; <b>bye</b></div><script>alert(1)</script>',
  'k.css': '@media (min-width: 10px) { .a { color: #fff; margin: -1.5em !important; } }',
  'l.sql': "SELECT name FROM t WHERE id = 1 -- comment\n/* block */ insert into t values ('it''s');",
  'm.diff': 'diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-old\n+new\n context',
  'n.c': '#include <stdio.h>\n  #define N 3\nint main(void) { return NULL; }',
  Dockerfile: 'FROM node:22 AS build\n  # comment\nRUN echo ${HOME} "$PATH"',
  Makefile: 'all: build # default\n\ttouch $@ $(OUT)\ninclude other.mk',
  'o.go': 'package main\nfunc main() { s := `raw`; _ = nil }',
  'p.java': '@Override public String toString() { return """\ntext"""; }',
  'q.r': 'f <- function(x) { if (x) TRUE else NA } # c',
  'r.jl': '#= block =#\n@time function f(x) x end # c',
  's.rb': 'def f; @x = :sym; nil; end # c',
  't.lua': '--[[ block ]] local x = [[long]] -- c',
  'u.ini': '; comment\n[section]\nkey=value',
};

describe('syntax colouring', () => {
  it('picks a language from the name, and none for the rest', () => {
    expect(languageFor('src/main.TS')?.id).toBe('javascript');
    expect(languageFor('deep/path/Cargo.lock')?.id).toBe('toml');
    expect(languageFor('Dockerfile.dev')?.id).toBe('dockerfile');
    expect(languageFor('paper/method.tex')?.id).toBe('tex');
    expect(languageFor('notes.txt')).toBeUndefined();
    expect(languageFor('.hidden')).toBeUndefined();
    expect(languageFor('README')).toBeUndefined();
    expect(languageFor('image.svg')?.id).toBe('markup');
  });

  it('keeps every character: the tokens join back into the file exactly', () => {
    for (const [path, text] of Object.entries(SAMPLES)) {
      const language = languageFor(path);
      expect(language, path).toBeDefined();
      const tokens = tokenize(text, language as NonNullable<typeof language>);
      expect(tokens.map((t) => t.text).join(''), path).toBe(text);
      expect(tokens.every((t) => t.text.length > 0), path).toBe(true);
    }
    // Anything at all, even unterminated strings and comments.
    const awkward = '"unterminated\n/* open\n`tick ${ "x\'\\\n\u{1F600}\r\n\t<a b="';
    for (const path of Object.keys(SAMPLES)) {
      const language = languageFor(path);
      if (language === undefined) continue;
      expect(tokenize(awkward, language).map((t) => t.text).join(''), path).toBe(awkward);
    }
  });

  it('colours the common constructs', () => {
    expect(marks('a.ts', SAMPLES['a.ts'] as string)).toEqual([
      'keyword:const',
      'type:number',
      'number:42',
      'comment:// the answer',
      'keyword:function',
      'function:greet',
      'type:string',
      'keyword:return',
      'string:`hi ${name}`',
      'comment:/* done */',
    ]);
    const rust = marks('b.rs', SAMPLES['b.rs'] as string);
    expect(rust).toEqual(
      expect.arrayContaining(['meta:#[derive(Debug)]', "meta:'a", "string:'x'", 'function:println!', 'string:r#"raw "quoted""#']),
    );
    const python = marks('c.py', SAMPLES['c.py'] as string);
    expect(python).toEqual(
      expect.arrayContaining(['meta:@cache', 'keyword:def', 'string:"""Doc\n    string"""', 'number:None', 'comment:# note']),
    );
    expect(marks('d.json', SAMPLES['d.json'] as string)).toEqual([
      'attr:"key"',
      'string:"value"',
      'attr:"n"',
      'number:-1.5e3',
      'attr:"ok"',
      'number:true',
      'attr:"nothing"',
      'number:null',
    ]);
    expect(marks('e.yaml', SAMPLES['e.yaml'] as string)).toEqual(
      expect.arrayContaining(['comment:# comment', 'meta:---', 'attr:name', 'attr:item', 'number:yes', 'meta:&a']),
    );
    expect(marks('f.toml', SAMPLES['f.toml'] as string)).toEqual(
      expect.arrayContaining(['type:[package]', 'attr:name', 'string:"pitcrew"', 'comment:# trailing', 'type:[[bin]]']),
    );
    expect(marks('g.sh', SAMPLES['g.sh'] as string)).toEqual(
      expect.arrayContaining(['comment:#!/bin/sh', 'keyword:if', 'string:"$HOME"', 'type:${USER}', 'type:$#', 'comment:# done']),
    );
    expect(marks('h.md', SAMPLES['h.md'] as string)).toEqual(
      expect.arrayContaining(['heading:# Title', 'string:`code`', 'keyword:**bold**', 'attr:[a link](https://example.com)', 'string:```ts\nconst x = 1;\n```', 'comment:> quoted', 'meta:-']),
    );
    expect(marks('i.tex', SAMPLES['i.tex'] as string)).toEqual(
      expect.arrayContaining(['keyword:\\section', 'comment:% comment', 'string:$x^2$', 'keyword:\\%', 'string:$$\\int_0^1 f$$']),
    );
    expect(marks('m.diff', SAMPLES['m.diff'] as string)).toEqual([
      'meta:diff --git a/x b/x',
      'meta:--- a/x',
      'meta:+++ b/x',
      'keyword:@@ -1 +1 @@',
      'deleted:-old',
      'inserted:+new',
    ]);
    expect(marks('n.c', SAMPLES['n.c'] as string)).toEqual(
      expect.arrayContaining(['meta:#include <stdio.h>', 'meta:#define N 3', 'keyword:int', 'number:NULL']),
    );
    expect(marks('Dockerfile', SAMPLES.Dockerfile as string)).toEqual(
      expect.arrayContaining(['keyword:FROM', 'keyword:AS', 'comment:# comment', 'keyword:RUN', 'type:${HOME}']),
    );
    expect(marks('Makefile', SAMPLES.Makefile as string)).toEqual(
      expect.arrayContaining(['function:all', 'comment:# default', 'type:$@', 'type:$(OUT)', 'keyword:include']),
    );
  });

  it('keeps markup as text: tags and attributes are tokens, never elements', () => {
    expect(marks('j.html', SAMPLES['j.html'] as string)).toEqual([
      'comment:<!-- note -->',
      'tag:<div',
      'attr:class',
      'string:"a"',
      'attr:data-x',
      "string:'b'",
      'tag:>',
      'meta:&amp;',
      'tag:<b',
      'tag:>',
      'tag:</b',
      'tag:>',
      'tag:</div',
      'tag:>',
      'tag:<script',
      'tag:>',
      'tag:</script',
      'tag:>',
    ]);
  });

  it('splits tokens into lines, and leaves large or unknown files plain', () => {
    const lines = toLines([
      { kind: 'comment', text: '/* a\nb */' },
      { kind: null, text: '\n' },
      { kind: 'keyword', text: 'let' },
    ] satisfies Token[]);
    expect(lines).toEqual([[{ kind: 'comment', text: '/* a' }], [{ kind: 'comment', text: 'b */' }], [{ kind: 'keyword', text: 'let' }]]);

    const big = 'let x = 1;\n'.repeat(HIGHLIGHT_LIMIT / 10);
    const plain = highlightLines(big, 'big.ts');
    expect(plain.language).toBeUndefined();
    expect(plain.lines[0]).toEqual([{ kind: null, text: 'let x = 1;' }]);
    expect(plain.lines.at(-1)).toEqual([]);
    expect(highlightLines('a\n\nb', 'notes.txt').lines).toEqual([[{ kind: null, text: 'a' }], [], [{ kind: null, text: 'b' }]]);
    expect(highlightLines('let x', 'x.ts').language?.id).toBe('javascript');
  });

  it('stays linear on hostile input, in every language', () => {
    // Long enough that a quadratic rule takes many seconds, short enough to keep the suite light.
    const half = 40_000;
    const hostile = [
      `a${' '.repeat(half)}`,
      'a '.repeat(half / 2),
      'r#"\n'.repeat(half / 4),
      '/*'.repeat(half / 2),
      `FROM x ${'a '.repeat(half / 2)}`,
      '<a b="'.repeat(half / 6),
      `x${'.'.repeat(half)}`,
      '"\\'.repeat(half / 3),
      `- ${'k'.repeat(half)}`,
      '**['.repeat(half / 3),
      '`'.repeat(half),
      '$'.repeat(half),
      '&'.repeat(half),
      '@'.repeat(half),
      '\\'.repeat(half),
      `${'\t'.repeat(half / 2)}#`,
      '= '.repeat(half / 2),
    ];
    const seen = new Set<string>();
    for (const path of Object.keys(SAMPLES)) {
      const language = languageFor(path);
      if (language === undefined || seen.has(language.id)) continue;
      seen.add(language.id);
      for (const text of hostile) {
        const started = performance.now();
        expect(tokenize(text, language).map((t) => t.text).join('')).toBe(text);
        expect(performance.now() - started, `${language.id}: ${JSON.stringify(text.slice(0, 12))}`).toBeLessThan(1_000);
      }
    }
  }, 60_000);

  it('colours a large file in reasonable time', () => {
    const text = SAMPLES['a.ts']?.repeat(Math.floor(HIGHLIGHT_LIMIT / (SAMPLES['a.ts']?.length ?? 1))) ?? '';
    const started = performance.now();
    const { lines } = highlightLines(text, 'big.ts');
    expect(lines.length).toBeGreaterThan(1_000);
    expect(performance.now() - started).toBeLessThan(2_000);
  });
});
