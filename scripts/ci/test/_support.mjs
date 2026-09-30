// Helpers shared by the tests. Not a test file itself.
import { execFileSync, spawnSync } from 'node:child_process';
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

export const CI_DIR = fileURLToPath(new URL('../', import.meta.url));

// Runs scripts/ci/<name> in a child process with a predictable environment.
export function runScript(name, args = [], { env = {}, input } = {}) {
  const base = { ...process.env };
  for (const key of ['GITHUB_ACTIONS', 'GITHUB_HEAD_REF', 'SCRUB_PATTERNS']) delete base[key];
  const r = spawnSync(process.execPath, [join(CI_DIR, name), ...args], {
    encoding: 'utf8',
    env: { ...base, ...env },
    input: input ?? '',
    windowsHide: true,
  });
  return { code: r.status, stdout: r.stdout, stderr: r.stderr, output: `${r.stdout}${r.stderr}` };
}

// A fresh temp directory, removed when the test ends.
export function makeTempDir(t, prefix = 'pitcrew-ci-') {
  const dir = mkdtempSync(join(tmpdir(), prefix));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  return dir;
}

export function writeFiles(root, files) {
  for (const [path, content] of Object.entries(files)) {
    const full = join(root, path);
    mkdirSync(dirname(full), { recursive: true });
    writeFileSync(full, content);
  }
}

// A git repo in <dir>/repo, isolated from the user's and system git config.
export function makeGitRepo(dir) {
  const repo = join(dir, 'repo');
  mkdirSync(repo, { recursive: true });
  const globalConfig = join(dir, 'gitconfig');
  writeFileSync(globalConfig, '');
  const env = {
    ...process.env,
    GIT_CONFIG_GLOBAL: globalConfig,
    GIT_CONFIG_NOSYSTEM: '1',
    GIT_AUTHOR_NAME: 'Test Author',
    GIT_AUTHOR_EMAIL: 'author@example.com',
    GIT_COMMITTER_NAME: 'Test Committer',
    GIT_COMMITTER_EMAIL: 'committer@example.com',
  };
  const run = (...args) =>
    execFileSync('git', args, { cwd: repo, env, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] });
  run('init', '-q', '-b', 'main');
  return { repo, run };
}
