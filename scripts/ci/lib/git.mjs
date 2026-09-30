// Thin wrappers over the git CLI. Arguments are passed as an array, never through a shell.
import { execFileSync } from 'node:child_process';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { ConfigError, checkRef } from './cli.mjs';

// The repository being checked. CI runs the base branch's copy of these scripts against the pull
// request's checkout (so a PR cannot weaken its own guards), and points PITCREW_REPO_ROOT at it.
export const REPO_ROOT = process.env.PITCREW_REPO_ROOT
  ? resolve(process.env.PITCREW_REPO_ROOT)
  : fileURLToPath(new URL('../../../', import.meta.url));

export function git(args, { cwd = REPO_ROOT } = {}) {
  try {
    return execFileSync('git', args, {
      cwd,
      encoding: 'utf8',
      maxBuffer: 512 * 1024 * 1024,
      stdio: ['ignore', 'pipe', 'pipe'],
      windowsHide: true,
    });
  } catch (err) {
    const detail = String(err.stderr || err.message).trim();
    throw new ConfigError(`git ${args.join(' ')} failed: ${detail}`);
  }
}

const splitNul = (out) => out.split('\0').filter(Boolean);

// Adds a hint when the base ref is probably just not fetched.
function withBaseHint(base, fn) {
  try {
    return fn();
  } catch (err) {
    if (err instanceof ConfigError && /bad revision|unknown revision|ambiguous argument|no merge base/i.test(err.message)) {
      throw new ConfigError(
        `${err.message}\nIs ${base} fetched? In CI check out with fetch-depth: 0; locally run git fetch origin.`,
      );
    }
    throw err;
  }
}

// Tracked files; with untracked: true also files not yet added (minus ignored ones).
export function listFiles({ untracked = false, cwd } = {}) {
  const args = ['ls-files', '-z'];
  if (untracked) args.push('--cached', '--others', '--exclude-standard');
  return [...new Set(splitNul(git(args, { cwd })))];
}

// Files changed on HEAD since it forked from base. A rename shows up as its old and new path.
export function changedFiles(base, { cwd } = {}) {
  checkRef(base);
  const args = ['diff', '--no-color', '--name-only', '-z', '--no-renames', `${base}...HEAD`, '--'];
  return withBaseHint(base, () => splitNul(git(args, { cwd })));
}

// Branch name, or '' when HEAD is detached.
export function currentBranch({ cwd } = {}) {
  return git(['branch', '--show-current'], { cwd }).trim();
}

// Commits in base..HEAD as { sha, text } where text is
// author name, author email, committer name, committer email, then the message, one per line.
export function commitsInRange(base, { cwd } = {}) {
  checkRef(base);
  const format = '--format=%H%n%an%n%ae%n%cn%n%ce%n%B';
  const args = ['-c', 'log.showSignature=false', 'log', '--no-color', '-z', format, `${base}..HEAD`, '--'];
  const out = withBaseHint(base, () => git(args, { cwd }));
  return splitNul(out)
    .map((entry) => entry.replace(/^\n+/, ''))
    .filter(Boolean)
    .map((entry) => {
      const nl = entry.indexOf('\n');
      return nl < 0 ? { sha: entry, text: '' } : { sha: entry.slice(0, nl), text: entry.slice(nl + 1) };
    });
}

// Contents of path at ref, or null when the ref has no such file.
export function showFile(ref, path, { cwd } = {}) {
  checkRef(ref);
  try {
    return git(['show', `${ref}:${path}`], { cwd });
  } catch {
    return null;
  }
}
