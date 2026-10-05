import assert from 'node:assert/strict';
import { test } from 'node:test';
import { ignoreMatcher } from '../src/file-ignore.ts';

test('location-local ignore globs, nested precedence, directory rules and escaped names', () => {
  const files: Record<string, string> = { '.gitignore': '*.log\n!keep.log\nbuild/\n/root.txt\n**/cache/**\n\\#notes\n', 'src/.gitignore': '!local.log\n*.tmp\n' };
  const ignored = ignoreMatcher('src', path => files[path]);
  for (const path of ['debug.log', 'src/debug.log', 'build/file.txt', 'src/a.tmp', 'src/cache/data', '#notes', 'root.txt']) assert.equal(ignored(path, false), true, path);
  for (const path of ['keep.log', 'src/keep.log', 'src/local.log', 'src/root.txt', 'method.tex']) assert.equal(ignored(path, false), false, path);
  assert.equal(ignored('build', true), true);
  assert.equal(ignored('build', false), false);
});
