import { describe, expect, it } from 'vitest';
import { changedRanges, parseDiff } from '../render/diff-parse.ts';

const METHOD_DIFF = [
  '--- a/method.tex',
  '+++ b/method.tex',
  '@@ -1,1 +1,2 @@',
  '-% TODO method',
  '+\\subsection{Model}',
  '+We use a four-level U-Net.',
  '',
].join('\n');

describe('parseDiff', () => {
  it('numbers lines and counts changes', () => {
    const diff = parseDiff(METHOD_DIFF);
    expect(diff.added).toBe(2);
    expect(diff.removed).toBe(1);
    expect(diff.lines.map((l) => [l.type, l.old, l.new])).toEqual([
      ['file', undefined, undefined],
      ['file', undefined, undefined],
      ['hunk', undefined, undefined],
      ['del', 1, undefined],
      ['add', undefined, 1],
      ['add', undefined, 2],
    ]);
  });

  it('reads a removed line that looks like a header as a removal', () => {
    const diff = parseDiff('@@ -1,2 +1,1 @@\n--- not a header\n kept\n');
    expect(diff.lines.map((l) => l.type)).toEqual(['hunk', 'del', 'ctx']);
    expect(diff.lines[1]?.text).toBe('-- not a header');
  });

  it('marks the words that changed in an edited line', () => {
    const diff = parseDiff('@@ -1 +1 @@\n-const speed = 10;\n+const speed = 20;\n');
    const del = diff.lines[1];
    const add = diff.lines[2];
    expect(del?.marks).toEqual([[14, 16]]);
    expect(add?.marks).toEqual([[14, 16]]);
    expect(add?.text.slice(14, 16)).toBe('20');
  });

  it('marks nothing when two lines share nothing', () => {
    expect(changedRanges('alpha', 'beta')).toBeUndefined();
  });
});
