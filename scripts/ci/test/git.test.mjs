import assert from 'node:assert/strict';
import { writeFileSync } from 'node:fs';
import { join } from 'node:path';
import test from 'node:test';
import { ConfigError } from '../lib/cli.mjs';
import { changedFiles, commitsInRange, currentBranch, listFiles } from '../lib/git.mjs';
import { makeGitRepo, makeTempDir, writeFiles } from './_support.mjs';

function repoWithHistory(t) {
  const dir = makeTempDir(t);
  const { repo, run } = makeGitRepo(dir);
  writeFiles(repo, { 'a.txt': 'a\n', 'old/name.txt': 'same content for a rename\n', 'gone.txt': 'x\n', '.gitignore': 'ignored.log\n' });
  run('add', '-A');
  run('commit', '-q', '-m', 'base');
  run('switch', '-q', '-c', 's/C/topic');
  writeFiles(repo, { 'new/name.txt': 'same content for a rename\n' });
  run('rm', '-q', 'old/name.txt', 'gone.txt');
  writeFileSync(join(repo, 'a.txt'), 'changed\n');
  writeFiles(repo, { 'added dir/file with space.txt': 'new\n' });
  run('add', '-A');
  run('commit', '-q', '-m', 'work');
  return { repo, run };
}

test('changedFiles lists both sides of a rename, deletions and odd names', (t) => {
  const { repo } = repoWithHistory(t);
  assert.deepEqual(changedFiles('main', { cwd: repo }).sort(), [
    'a.txt',
    'added dir/file with space.txt',
    'gone.txt',
    'new/name.txt',
    'old/name.txt',
  ]);
});

test('changedFiles uses the merge base (three dots)', (t) => {
  const { repo, run } = repoWithHistory(t);
  run('switch', '-q', 'main');
  writeFiles(repo, { 'main-only.txt': 'x\n' });
  run('add', '-A');
  run('commit', '-q', '-m', 'main moved on');
  run('switch', '-q', 's/C/topic');
  assert.ok(!changedFiles('main', { cwd: repo }).includes('main-only.txt'));
});

test('listFiles: tracked only, or with untracked files but never ignored ones', (t) => {
  const { repo } = repoWithHistory(t);
  writeFiles(repo, { 'untracked.txt': 'u\n', 'ignored.log': 'i\n' });
  const tracked = listFiles({ cwd: repo });
  assert.ok(tracked.includes('new/name.txt'));
  assert.ok(!tracked.includes('untracked.txt'));
  const all = listFiles({ cwd: repo, untracked: true });
  assert.ok(all.includes('untracked.txt'));
  assert.ok(!all.includes('ignored.log'));
});

test('currentBranch and commitsInRange', (t) => {
  const { repo } = repoWithHistory(t);
  assert.equal(currentBranch({ cwd: repo }), 's/C/topic');
  const commits = commitsInRange('main', { cwd: repo });
  assert.equal(commits.length, 1);
  assert.match(commits[0].sha, /^[0-9a-f]{40}$/);
  assert.equal(commits[0].text, 'Test Author\nauthor@example.com\nTest Committer\ncommitter@example.com\nwork\n');
});

test('git failures and option-like refs are config errors', (t) => {
  const { repo } = repoWithHistory(t);
  assert.throws(() => changedFiles('no-such-ref', { cwd: repo }), (err) => err instanceof ConfigError && /git diff/.test(err.message));
  assert.throws(() => changedFiles('--output=x', { cwd: repo }), ConfigError);
});
