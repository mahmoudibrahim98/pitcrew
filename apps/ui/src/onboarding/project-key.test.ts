// The project-key rule (`project-key.ts`): a valid `ProjectKey` from any name, unique among the
// keys already taken.

import { describe, expect, it } from 'vitest';
import { baseKey, keyWords, PROJECT_KEY, projectKeyFor } from './project-key.ts';

describe('the key a name suggests', () => {
  it.each([
    ['Diffusion study', 'DS'],
    ['diffusion-paper', 'DP'],
    ['lab-tools', 'LT'],
    ['paper', 'PAP'],
    ['pitcrew', 'PIT'],
    ['My cool project 2', 'MCP2'],
    ['a b c d e f', 'ABCD'],
    ['Café notes', 'CN'],
    ['Ångström', 'ANG'],
    ['2024 notes', 'NOT'],
    ['3d-models', 'DM'],
    ['  /scratch/sam/diffusion-runs  ', 'SSDR'],
    ['ab', 'AB'],
    ['x', 'XX'],
    ['日本語のメモ', 'PRJ'],
    ['', 'PRJ'],
    ['42', 'PRJ'],
  ])('%j → %s', (name, key) => {
    expect(baseKey(name)).toBe(key);
    expect(key).toMatch(PROJECT_KEY);
  });

  it('splits on anything but ASCII letters and digits, and drops digits before the first letter', () => {
    expect(keyWords('  lab_tools: v2.1 ')).toEqual(['LAB', 'TOOLS', 'V2', '1']);
    expect(keyWords('007 bond')).toEqual(['BOND']);
    expect(keyWords('9lives club')).toEqual(['LIVES', 'CLUB']);
    expect(keyWords('—')).toEqual([]);
  });
});

describe('a key unique in the workspace', () => {
  it('is the suggested key while it is free', () => {
    expect(projectKeyFor('paper', new Set(['TL', 'DS']))).toBe('PAP');
  });

  it('takes the smallest number from 2 that frees it', () => {
    expect(projectKeyFor('paper', new Set(['PAP']))).toBe('PAP2');
    expect(projectKeyFor('paper', new Set(['PAP', 'PAP2', 'PAP4']))).toBe('PAP3');
    const nine = new Set(['PAP', ...[2, 3, 4, 5, 6, 7, 8, 9].map((n) => `PAP${n}`)]);
    expect(projectKeyFor('paper', nine)).toBe('PAP10');
  });

  it('gives each project of one batch its own key', () => {
    const taken = new Set(['DP']);
    const keys = ['diffusion-paper', 'Diffusion paper', 'diffusion_paper', 'paper'].map((name) => {
      const key = projectKeyFor(name, taken);
      taken.add(key);
      return key;
    });
    expect(keys).toEqual(['DP2', 'DP3', 'DP4', 'PAP']);
  });

  it('is always a valid key, whatever the name', () => {
    const names = ['', ' ', '!!!', 'Z', 'zz top', 'ÉCOLE', 'ω', 'a1 b2 c3 d4 e5', 'Q'.repeat(40), '0'.repeat(12)];
    const taken = new Set<string>();
    for (const name of names) {
      for (let i = 0; i < 3; i += 1) {
        const key = projectKeyFor(name, taken);
        expect(key, name).toMatch(PROJECT_KEY);
        expect(taken.has(key), name).toBe(false);
        taken.add(key);
      }
    }
  });
});
