import { describe, expect, it } from 'vitest';
import { rank, scoreFields, scoreWord } from '../src/shell/fuzzy.ts';

describe('scoreWord', () => {
  it('ranks exact over prefix over word start over substring over scattered letters', () => {
    const exact = scoreWord('pap-4', 'pap-4');
    const prefix = scoreWord('seed', 'seed runs');
    const wordStart = scoreWord('runs', 'seed runs');
    const substring = scoreWord('eed', 'seed runs');
    const scattered = scoreWord('sdrn', 'seed runs');
    expect(exact).toBe(100);
    expect(prefix).toBeGreaterThan(wordStart ?? Infinity);
    expect(wordStart).toBeGreaterThan(substring ?? Infinity);
    expect(substring).toBeGreaterThan(scattered ?? Infinity);
    expect(scattered).toBeGreaterThan(0);
  });

  it('is null when a letter is missing or out of order', () => {
    expect(scoreWord('xyz', 'seed runs')).toBeNull();
    expect(scoreWord('nr', 'run')).toBeNull();
  });

  it('prefers a later word start to an earlier hit inside a word', () => {
    expect(scoreWord('run', 'prune runs')).toBeGreaterThan(scoreWord('run', 'prunes') ?? Infinity);
  });
});

describe('scoreFields', () => {
  it('needs every word of the query to match some field', () => {
    expect(scoreFields('seed paper', ['Seed runs', 'Paper · Diffusion study'])).not.toBeNull();
    expect(scoreFields('seed tooling', ['Seed runs', 'Paper · Diffusion study'])).toBeNull();
  });

  it('matches an empty query with score 0', () => {
    expect(scoreFields('  ', ['anything'])).toBe(0);
  });
});

describe('rank', () => {
  const tasks = [
    { key: 'PAP-40', title: 'Plot the loss curves' },
    { key: 'TL-1', title: 'Parse PAP-4 references' },
    { key: 'PAP-4', title: 'Run seeds 1–5 on the cluster' },
    { key: 'PAP-5', title: 'Decide what to do about seed 3' },
  ];
  const fields = (t: (typeof tasks)[number]) => [t.key, t.title];

  it('puts the task whose key is typed first', () => {
    expect(rank('PAP-4', tasks, fields).map((t) => t.key)).toEqual(['PAP-4', 'PAP-40', 'TL-1']);
  });

  it('searches titles too, case-insensitively', () => {
    expect(rank('seeds', tasks, fields).map((t) => t.key)).toEqual(['PAP-4']);
    expect(rank('SEED', tasks, fields).map((t) => t.key)).toEqual(['PAP-4', 'PAP-5']);
  });

  it('keeps the original order for equal scores and drops non-matches', () => {
    expect(rank('', tasks, fields)).toEqual(tasks);
    expect(rank('zzz', tasks, fields)).toEqual([]);
  });
});
