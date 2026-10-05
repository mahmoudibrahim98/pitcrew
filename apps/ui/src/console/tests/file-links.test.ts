import { expect, it } from 'vitest';
import { changedLine, resolveFilePath } from '../file-links.tsx';
import type { Workstream } from '../../data/index.ts';

const stream = { id: 'stream', locations: [{ machine: 'local', path: '/home/sam/paper' }, { machine: 'remote', path: '/scratch/paper' }, { machine: 'local', path: '/home/sam/paper/sections' }] } as Workstream;
it('resolves relative and absolute transcript paths at the deepest location on the session machine', () => {
  expect(resolveFilePath('sections/method.tex', '/home/sam/paper', 'local', stream)).toEqual({ kind: 'file', workstream: 'stream', location: 2, path: 'method.tex' });
  expect(resolveFilePath('/home/sam/paper/main.tex', '/elsewhere', 'local', stream)?.path).toBe('main.tex');
  for (const path of ['../secret', '/home/sam/paper-other/secret', '/scratch/paper/secret', 'sections/../main.tex', 'a\0b']) expect(resolveFilePath(path, '/home/sam/paper', 'local', stream)).toBeUndefined();
  expect(changedLine('@@ -12,3 +14,4 @@\n+new')).toBe(14);
  expect(changedLine(undefined)).toBeUndefined();
  expect(changedLine('@@ -10,3 +10,3 @@\n context\n context\n-old\n+new')).toBe(12);
});
it('handles Windows separators and casing, and rejects traversal', () => {
  const windows = { ...stream, locations: [{ machine: 'local', path: 'C:\\synthetic\\paper' }] };
  expect(resolveFilePath('C:\\SYNTHETIC\\paper\\sections\\method.tex', '', 'local', windows)?.path).toBe('sections/method.tex');
  expect(resolveFilePath('..\\secret', 'C:\\synthetic\\paper', 'local', windows)).toBeUndefined();
});
