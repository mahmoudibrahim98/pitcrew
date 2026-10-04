// Syntax colouring for the file viewer: a small tokenizer of our own, no dependency. Each language
// is a list of sticky regular expressions tried in order at each position; the first that matches
// makes a token. The result is plain data (kinds and text), rendered as React text, so a file can
// never become markup. It colours, it does not parse: a construct the rules miss stays plain text.

export type TokenKind =
  | 'comment'
  | 'string'
  | 'keyword'
  | 'number'
  | 'type'
  | 'function'
  | 'tag'
  | 'attr'
  | 'meta'
  | 'inserted'
  | 'deleted'
  | 'heading';

/** `kind` null is plain text. */
export interface Token {
  kind: TokenKind | null;
  text: string;
}

type Rule =
  | readonly [TokenKind | null, RegExp]
  /** A match tokenized again with another grammar (a markup tag's name, attributes and values). */
  | readonly ['inner', RegExp, () => readonly Rule[]];

export interface Language {
  id: string;
  label: string;
  rules: readonly Rule[];
}

/** Above this many characters a file shows as plain text: colouring it would hold up the page. */
export const HIGHLIGHT_LIMIT = 200_000;

// Every rule must stay linear on any input: a file is untrusted, and a rule that backtracks over
// a long line could hold up the page (tests/highlight.test.ts feeds each language hostile input).
// So: alternatives start differently, an unterminated construct runs to the end of the file
// (`|$`), and a lookbehind only looks back over spaces.

// ─── Pieces ─────────────────────────────────────────────────────────────────────────────────────

const words = (list: string, flags = '') => new RegExp(`\\b(?:${list.trim().split(/\s+/).join('|')})\\b`, `y${flags}`);

const SPACE = [null, /\s+/y] as const;
const IDENT = [null, /[A-Za-z_$][\w$]*/y] as const;
const FUNCTION = ['function', /[A-Za-z_$][\w$]*(?=\s*\()/y] as const;
const TYPE = ['type', /[A-Z][\w$]*/y] as const;
const NUMBER = [
  'number',
  /(?:0[xX][\da-fA-F_]+|0[bB][01_]+|0[oO][0-7_]+|(?:\d[\d_]*\.?[\d_]*|\.\d[\d_]*)(?:[eE][+-]?\d+)?)[a-zA-Z\d]*/y,
] as const;
const DOUBLE = ['string', /"(?:[^"\\\n]|\\.)*"?/y] as const;
const SINGLE = ['string', /'(?:[^'\\\n]|\\.)*'?/y] as const;
const BACKTICK = ['string', /`(?:[^`\\]|\\[\s\S])*`?/y] as const;
const SLASH_LINE = ['comment', /\/\/.*/y] as const;
const SLASH_BLOCK = ['comment', /\/\*[\s\S]*?(?:\*\/|$)/y] as const;
/** `#` at the start of a line or after a space (so `a#b` and `$#` stay plain). */
const HASH_LINE = ['comment', /(?<=^|\s)#.*/my] as const;

const clike = (keywords: string, literals: string, extra: readonly Rule[] = []): readonly Rule[] => [
  SPACE,
  SLASH_LINE,
  SLASH_BLOCK,
  ...extra,
  DOUBLE,
  SINGLE,
  NUMBER,
  ['keyword', words(keywords)],
  ['number', words(literals)],
  FUNCTION,
  TYPE,
  IDENT,
];

// ─── Languages ──────────────────────────────────────────────────────────────────────────────────

const JS_KEYWORDS = `abstract as async await break case catch class const continue debugger declare default delete do
  else enum export extends finally for from function get if implements import in infer instanceof interface is keyof let
  namespace new of override private protected public readonly return satisfies set static super switch this throw try
  type typeof unique var void while with yield`;

const javascript: Language = {
  id: 'javascript',
  label: 'JavaScript / TypeScript',
  rules: clike(JS_KEYWORDS, 'true false null undefined NaN Infinity', [
    BACKTICK,
    ['meta', /@[A-Za-z_$][\w$]*/y],
    ['type', words('any bigint boolean never number object string symbol unknown')],
  ]),
};

const json: Language = {
  id: 'json',
  label: 'JSON',
  rules: [
    SPACE,
    SLASH_LINE,
    SLASH_BLOCK,
    ['attr', /"(?:[^"\\\n]|\\.)*"(?=\s*:)/y],
    DOUBLE,
    ['number', /-?(?:\d+\.?\d*|\.\d+)(?:[eE][+-]?\d+)?/y],
    ['number', words('true false null')],
    IDENT,
  ],
};

const rust: Language = {
  id: 'rust',
  label: 'Rust',
  rules: clike(
    `as async await break const continue crate dyn else enum extern fn for if impl in let loop match mod move mut pub
     ref return self Self static struct super trait type unsafe use where while`,
    'true false',
    [
      ['string', /r(#*)"[\s\S]*?(?:"\1|$)/y],
      ['meta', /#!?\[[^\]\n]*\]?/y],
      ['string', /'(?:[^'\\\n]|\\.[^'\n]{0,8})'/y],
      ['meta', /'[A-Za-z_]\w*/y],
      ['function', /[A-Za-z_]\w*!/y],
    ],
  ),
};

const go: Language = {
  id: 'go',
  label: 'Go',
  rules: clike(
    `break case chan const continue default defer else fallthrough for func go goto if import interface map package
     range return select struct switch type var`,
    'true false nil iota',
    [BACKTICK],
  ),
};

const c: Language = {
  id: 'c',
  label: 'C / C++',
  rules: clike(
    `auto break case char const continue default do double else enum extern float for goto if inline int long
     register restrict return short signed sizeof static struct switch typedef union unsigned void volatile while bool
     class namespace template typename public private protected virtual override final new delete this throw try catch
     using operator friend constexpr noexcept explicit mutable static_cast dynamic_cast reinterpret_cast const_cast`,
    'true false NULL nullptr',
    [['meta', /(?<=^[ \t]*)#[ \t]*\w+.*/my]],
  ),
};

const java: Language = {
  id: 'java',
  label: 'Java, Kotlin, C#, Swift',
  rules: clike(
    `abstract async await break case catch class const continue default defer do else enum extends extension final
     finally for fun func guard if implements import in interface internal is let namespace new object operator
     override package private protected protocol public return sealed static struct super switch this throw throws try
     typealias using val var void when where while yield`,
    'true false null nil',
    [['meta', /@[A-Za-z_]\w*/y], ['string', /"""[\s\S]*?(?:"""|$)/y]],
  ),
};

const pythonStrings: readonly Rule[] = [
  ['string', /[rRbBuUfF]{0,2}"""[\s\S]*?(?:"""|$)/y],
  ['string', /[rRbBuUfF]{0,2}'''[\s\S]*?(?:'''|$)/y],
  ['string', /[rRbBuUfF]{1,2}(?="|')/y],
];

const python: Language = {
  id: 'python',
  label: 'Python',
  rules: [
    SPACE,
    HASH_LINE,
    ...pythonStrings,
    DOUBLE,
    SINGLE,
    ['meta', /@[\w.]+/y],
    NUMBER,
    [
      'keyword',
      words(`and as assert async await break class continue def del elif else except finally for from global if import
        in is lambda match case nonlocal not or pass raise return try while with yield`),
    ],
    ['number', words('True False None')],
    FUNCTION,
    TYPE,
    IDENT,
  ],
};

const r: Language = {
  id: 'r',
  label: 'R',
  rules: [
    SPACE,
    HASH_LINE,
    DOUBLE,
    SINGLE,
    NUMBER,
    ['keyword', words('function if else for while repeat break next return in library require')],
    ['number', words('TRUE FALSE NULL NA NaN Inf')],
    FUNCTION,
    [null, /[A-Za-z_.][\w.]*/y],
  ],
};

const julia: Language = {
  id: 'julia',
  label: 'Julia',
  rules: [
    SPACE,
    ['comment', /#=[\s\S]*?(?:=#|$)/y],
    HASH_LINE,
    ['string', /"""[\s\S]*?(?:"""|$)/y],
    DOUBLE,
    ['meta', /@\w+/y],
    NUMBER,
    [
      'keyword',
      words(`function end if elseif else for while begin let local global const struct mutable abstract type module
        using import export return break continue try catch finally macro quote do in where`),
    ],
    ['number', words('true false nothing missing')],
    FUNCTION,
    TYPE,
    IDENT,
  ],
};

const ruby: Language = {
  id: 'ruby',
  label: 'Ruby',
  rules: [
    SPACE,
    HASH_LINE,
    DOUBLE,
    SINGLE,
    ['type', /@@?\w+/y],
    ['number', /:\w+/y],
    NUMBER,
    [
      'keyword',
      words(`def end if elsif else unless while until for in do class module return yield begin rescue ensure case
        when then self and or not require lambda proc`),
    ],
    ['number', words('true false nil')],
    FUNCTION,
    TYPE,
    IDENT,
  ],
};

const lua: Language = {
  id: 'lua',
  label: 'Lua',
  rules: [
    SPACE,
    ['comment', /--\[(=*)\[[\s\S]*?(?:\]\1\]|$)/y],
    ['comment', /--.*/y],
    ['string', /\[(=*)\[[\s\S]*?(?:\]\1\]|$)/y],
    DOUBLE,
    SINGLE,
    NUMBER,
    ['keyword', words('and break do else elseif end for function goto if in local not or repeat return then until while')],
    ['number', words('true false nil')],
    FUNCTION,
    IDENT,
  ],
};

const shell: Language = {
  id: 'shell',
  label: 'Shell',
  rules: [
    SPACE,
    HASH_LINE,
    DOUBLE,
    ['string', /'[^']*'?/y],
    ['type', /\$\{[^}\n]*\}?|\$[\w@#?$!*-]/y],
    [
      'keyword',
      words(`if then else elif fi for in do done while until case esac function return local export readonly declare
        unset shift exit break continue select time`),
    ],
    NUMBER,
    [null, /[\w.-]+/y],
  ],
};

const toml: Language = {
  id: 'toml',
  label: 'TOML / INI',
  rules: [
    SPACE,
    ['comment', /(?<=^|\s)[#;].*/my],
    ['type', /(?<=^[ \t]*)\[\[?[^\]\n]*\]\]?/my],
    // A key, dotted or quoted: no part of it can also be the space before `=` (no backtracking).
    ['attr', /(?<=^[ \t]*)(?:[\w-]+|"[^"\n]*"|'[^'\n]*')(?:[ \t]*\.[ \t]*(?:[\w-]+|"[^"\n]*"|'[^'\n]*'))*(?=[ \t]*[=:])/my],
    ['string', /"""[\s\S]*?(?:"""|$)/y],
    ['string', /'''[\s\S]*?(?:'''|$)/y],
    DOUBLE,
    SINGLE,
    ['number', words('true false')],
    NUMBER,
    IDENT,
  ],
};

const yaml: Language = {
  id: 'yaml',
  label: 'YAML',
  rules: [
    SPACE,
    HASH_LINE,
    ['meta', /^(?:---|\.\.\.)(?=\s|$)/my],
    ['attr', /(?<=^[ \t]*(?:-[ \t]+)?)[^\s#:'"-][^:#\n]*?(?=:(?:[ \t]|$))/my],
    DOUBLE,
    SINGLE,
    ['meta', /[&*][\w-]+/y],
    ['number', words('true false null yes no on off')],
    ['number', /~(?=\s|$)/y],
    NUMBER,
    [null, /[\w.-]+/y],
  ],
};

const markdown: Language = {
  id: 'markdown',
  label: 'Markdown',
  rules: [
    // The fence's run of marks is taken whole (`(?![`~])`), so a failed match never retries it shorter.
    ['string', /^(`{3,}|~{3,})(?![`~])[^\n]*\n[\s\S]*?(?:^\1[ \t]*$|(?![\s\S]))/my],
    ['heading', /^#{1,6}[ \t].*/my],
    ['comment', /^[ \t]*>.*/my],
    ['meta', /^[ \t]*(?:[-*+]|\d+[.)])(?=[ \t])/my],
    ['string', /`[^`\n]+`/y],
    ['keyword', /\*\*[^*\n]+\*\*|__[^_\n]+__/y],
    // Bounded, so a line of unclosed brackets is not scanned again from each one.
    ['attr', /!?\[[^\]\n]{0,500}\]\([^)\n]{0,2000}\)/y],
    [null, /[^\n`*_[!#>~\-+\d]+/y],
  ],
};

const tex: Language = {
  id: 'tex',
  label: 'TeX',
  rules: [
    SPACE,
    ['keyword', /\\(?:[A-Za-z@]+|.)/y],
    ['comment', /%.*/y],
    ['string', /\$\$[\s\S]*?(?:\$\$|$)/y],
    ['string', /\$(?:[^$\\\n]|\\.)*\$?/y],
    ['meta', /[{}[\]&]/y],
    [null, /[^\\%${}[\]&\s]+/y],
  ],
};

const css: Language = {
  id: 'css',
  label: 'CSS',
  rules: [
    SPACE,
    SLASH_BLOCK,
    SLASH_LINE,
    DOUBLE,
    SINGLE,
    ['keyword', /@[\w-]+|!important\b/y],
    ['number', /#[\da-fA-F]{3,8}(?![\w-])/y],
    ['attr', /--?[A-Za-z][\w-]*(?=\s*:)|[A-Za-z][\w-]*(?=\s*:[^:])/y],
    ['number', /-?(?:\d+\.?\d*|\.\d+)(?:%|[A-Za-z]+)?/y],
    FUNCTION,
    [null, /[\w-]+/y],
  ],
};

const tagInside: () => readonly Rule[] = () => [
  SPACE,
  ['tag', /<\/?[\w:.-]*|\/?>/y],
  ['attr', /[^\s=/>"']+/y],
  ['string', /"[^"]*"?|'[^']*'?/y],
];

const markup: Language = {
  id: 'markup',
  label: 'HTML / XML',
  rules: [
    ['comment', /<!--[\s\S]*?(?:-->|$)/y],
    ['string', /<!\[CDATA\[[\s\S]*?(?:\]\]>|$)/y],
    ['meta', /<[!?][^>]*>?/y],
    ['inner', /<\/?[A-Za-z][\w:.-]*(?:[^>"']|"[^"]*"|'[^']*')*>?/y, tagInside],
    ['meta', /&(?:#\d+|#x[\da-fA-F]+|\w+);/y],
    [null, /[^<&]+/y],
  ],
};

const sql: Language = {
  id: 'sql',
  label: 'SQL',
  rules: [
    SPACE,
    ['comment', /--.*/y],
    SLASH_BLOCK,
    ['string', /'(?:[^']|'')*'?/y],
    DOUBLE,
    NUMBER,
    [
      'keyword',
      words(
        `select from where and or not insert into values update set delete create table index view drop alter add
         primary key foreign references join left right inner outer full on group by order having limit offset as
         distinct union all case when then else end is null in exists between like begin commit rollback with returning`,
        'i',
      ),
    ],
    FUNCTION,
    IDENT,
  ],
};

const diff: Language = {
  id: 'diff',
  label: 'Diff',
  rules: [
    ['meta', /^(?:\+\+\+|---|diff |index ).*/my],
    ['keyword', /^@@.*/my],
    ['inserted', /^\+.*/my],
    ['deleted', /^-.*/my],
    [null, /.+|\n/y],
  ],
};

const dockerfile: Language = {
  id: 'dockerfile',
  label: 'Dockerfile',
  rules: [
    SPACE,
    ['comment', /(?<=^[ \t]*)#.*/my],
    [
      'keyword',
      /(?<=^[ \t]*)(?:FROM|RUN|CMD|LABEL|EXPOSE|ENV|ADD|COPY|ENTRYPOINT|VOLUME|USER|WORKDIR|ARG|ONBUILD|STOPSIGNAL|HEALTHCHECK|SHELL)\b/imy,
    ],
    ['keyword', /\bAS\b/y],
    DOUBLE,
    SINGLE,
    ['type', /\$\{[^}\n]*\}?|\$\w+/y],
    [null, /[\w.-]+/y],
  ],
};

const make: Language = {
  id: 'make',
  label: 'Makefile',
  rules: [
    SPACE,
    HASH_LINE,
    ['keyword', /(?<=^[ \t]*)(?:include|-include|ifeq|ifneq|ifdef|ifndef|else|endif|define|endef|export|override)\b/my],
    ['function', /^[^\s:#=]+(?=:(?!=))/my],
    ['type', /\$[({][^)}\n]*[)}]?|\$[@<^?*%]/y],
    DOUBLE,
    SINGLE,
    [null, /[\w.-]+/y],
  ],
};

const BY_EXTENSION: Record<string, Language> = {};
const add = (language: Language, extensions: string) => {
  for (const ext of extensions.split(' ')) BY_EXTENSION[ext] = language;
};
add(javascript, 'js jsx mjs cjs ts tsx mts cts');
add(json, 'json jsonc json5 jsonl ipynb');
add(rust, 'rs');
add(go, 'go');
add(c, 'c h cc cpp cxx hpp hh hxx cu cuh ino');
add(java, 'java kt kts cs swift scala dart groovy gradle');
add(python, 'py pyi pyw');
add(r, 'r');
add(julia, 'jl');
add(ruby, 'rb rake gemspec');
add(lua, 'lua');
add(shell, 'sh bash zsh fish ksh');
add(toml, 'toml ini cfg conf properties editorconfig');
add(yaml, 'yaml yml');
add(markdown, 'md markdown mdx');
add(tex, 'tex sty cls ltx bib bst dtx');
add(css, 'css scss sass less');
add(markup, 'html htm xhtml xml svg xsl xslt vue svelte plist');
add(sql, 'sql');
add(diff, 'diff patch');

const BY_NAME: Record<string, Language> = {
  dockerfile,
  containerfile: dockerfile,
  makefile: make,
  gnumakefile: make,
  '.bashrc': shell,
  '.bash_profile': shell,
  '.zshrc': shell,
  '.profile': shell,
  '.gitignore': shell,
  '.env': shell,
  'cargo.lock': toml,
};

/** The language a file's name says, if the viewer colours it. */
export function languageFor(path: string): Language | undefined {
  const name = (path.split('/').pop() ?? '').toLowerCase();
  const byName = BY_NAME[name];
  if (byName !== undefined) return byName;
  if (name.startsWith('dockerfile.') || name.endsWith('.dockerfile')) return dockerfile;
  if (name.endsWith('.mk')) return make;
  const dot = name.lastIndexOf('.');
  return dot <= 0 ? undefined : BY_EXTENSION[name.slice(dot + 1)];
}

// ─── The tokenizer ──────────────────────────────────────────────────────────────────────────────

/** What nothing matched is skipped as plain text: a whole word at once, else one character. */
const SKIP = /[\w$]+|[\s\S]/y;

function run(text: string, rules: readonly Rule[], out: Token[]): void {
  let plain = '';
  const push = (kind: TokenKind | null, value: string) => {
    if (kind === null) {
      plain += value;
      return;
    }
    if (plain !== '') {
      out.push({ kind: null, text: plain });
      plain = '';
    }
    out.push({ kind, text: value });
  };
  let at = 0;
  next: while (at < text.length) {
    for (const rule of rules) {
      const pattern = rule[1];
      pattern.lastIndex = at;
      const match = pattern.exec(text);
      if (match === null || match[0].length === 0) continue;
      if (rule[0] === 'inner') {
        if (plain !== '') {
          out.push({ kind: null, text: plain });
          plain = '';
        }
        run(match[0], rule[2](), out);
      } else {
        push(rule[0], match[0]);
      }
      at += match[0].length;
      continue next;
    }
    // A whole word, so the rules are not tried again at every letter of it (which, after a rule
    // that scanned the word and failed, would be quadratic on a long one).
    SKIP.lastIndex = at;
    const skipped = (SKIP.exec(text) as RegExpExecArray)[0];
    plain += skipped;
    at += skipped.length;
  }
  if (plain !== '') out.push({ kind: null, text: plain });
}

/** `text` as tokens in `language`; their texts join back into `text` exactly. */
export function tokenize(text: string, language: Language): Token[] {
  const out: Token[] = [];
  run(text, language.rules, out);
  return out;
}

/** Tokens split into lines (on `\n`, which no token keeps). */
export function toLines(tokens: readonly Token[]): Token[][] {
  const lines: Token[][] = [[]];
  for (const token of tokens) {
    const parts = token.text.split('\n');
    parts.forEach((part, i) => {
      if (i > 0) lines.push([]);
      if (part !== '') (lines[lines.length - 1] as Token[]).push({ kind: token.kind, text: part });
    });
  }
  return lines;
}

/**
 * The file's lines, coloured when its name says a language and it is small enough; otherwise each
 * line is one plain token.
 */
export function highlightLines(text: string, path: string): { language: Language | undefined; lines: Token[][] } {
  const language = text.length <= HIGHLIGHT_LIMIT ? languageFor(path) : undefined;
  if (language === undefined) {
    return { language, lines: text.split('\n').map((line) => (line === '' ? [] : [{ kind: null, text: line }])) };
  }
  return { language, lines: toLines(tokenize(text, language)) };
}
