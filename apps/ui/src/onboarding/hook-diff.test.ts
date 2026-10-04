import { expect, it } from 'vitest';
import { hookDiff } from './hook-diff.ts';
it('shows one unified hunk with context for an edit and a new file', () => {
  expect(hookDiff('config', 'same\r\nold\r\nend', 'same\r\nnew\r\nend')).toBe('--- config\n+++ config\n@@ -1,3 +1,3 @@\n same\n-old\n+new\n end');
  expect(hookDiff('plugin', null, 'new')).toBe('--- /dev/null\n+++ plugin\n@@ -0,0 +1,1 @@\n+new');
});

it('does not count a trailing newline as an extra file line', () => {
  expect(hookDiff('plugin', null, 'new\n')).toBe('--- /dev/null\n+++ plugin\n@@ -0,0 +1,1 @@\n+new');
});
