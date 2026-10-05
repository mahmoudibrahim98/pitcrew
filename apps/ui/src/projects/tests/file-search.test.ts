import { expect, it, vi } from 'vitest';
import { collectFiles, fileScore, SEARCH_LIMITS } from '../file-search.ts';
import type { FileListing } from '../../data/files.ts';

const entry = (name: string, kind: 'folder' | 'file' | 'link' = 'file', ignored = false) => ({ name, kind, size: 0, modified_at: null, ignored });
it('searches relative filenames, skips links, dot names and ignored directories, and preserves fuzzy ordering', async () => {
  const list = vi.fn(async (path: string): Promise<FileListing> => ({ entries: path === '' ? [entry('src', 'folder'), entry('outside', 'link'), entry('.git', 'folder'), entry('build', 'folder', true)] : [entry('method.tex'), entry('notes.txt')], truncated: false }));
  const found = await collectFiles({ list }, false, new AbortController().signal);
  expect(found).toEqual({ paths: ['src/method.tex', 'src/notes.txt'], truncated: false, unavailable: false });
  expect(list.mock.calls.map(call => call[0])).toEqual(['', 'src']);
  expect(fileScore('src/method.tex', 'smtex')).toBeTypeOf('number');
  expect(fileScore('src/method.tex', 'xyz')).toBeUndefined();
  expect(fileScore('method.tex', 'method')).toBeGreaterThan(fileScore('m-e-t-h-o-d.tex', 'method') ?? Number.NEGATIVE_INFINITY);
});
it('show hidden includes ignored entries without following links', async () => {
  const list = vi.fn(async () => ({ entries: [entry('.env'), entry('build.log', 'file', true), entry('outside', 'link')], truncated: false }));
  expect((await collectFiles({ list }, true, new AbortController().signal)).paths).toEqual(['.env', 'build.log']);
  expect(list).toHaveBeenCalledTimes(1);
});
it('bounds directories and entries, reports truncation and unavailable folders, and cancels', async () => {
  const directoryList = vi.fn(async () => ({ entries: [entry('child', 'folder')], truncated: false }));
  expect((await collectFiles({ list: directoryList }, false, new AbortController().signal)).truncated).toBe(true);
  expect(directoryList).toHaveBeenCalledTimes(SEARCH_LIMITS.directories);
  const entries = Array.from({ length: 5001 }, (_, i) => entry(`${i}.txt`));
  expect((await collectFiles({ list: async () => ({ entries, truncated: false }) }, false, new AbortController().signal)).paths).toHaveLength(5000);
  expect((await collectFiles({ list: async () => { throw Error('unavailable'); } }, false, new AbortController().signal)).unavailable).toBe(true);
  const aborted = AbortSignal.abort();
  await expect(collectFiles({ list: directoryList }, false, aborted)).rejects.toBeDefined();
});
