import assert from 'node:assert/strict';
import { writeFileSync } from 'node:fs';
import { join } from 'node:path';
import test from 'node:test';
import { loadOwnership } from '../lib/ownership.mjs';
import { checkBranch, formatResult, parseBranch } from '../path-guard.mjs';
import { makeTempDir, runScript } from './_support.mjs';

const ownership = loadOwnership();

test('parseBranch recognises each branch kind', () => {
  const cases = [
    ['s/C/store-fix', { kind: 'stream', stream: 'C', topic: 'store-fix' }],
    ['s/0/skeleton/part-2', { kind: 'stream', stream: '0', topic: 'skeleton/part-2' }],
    ['refs/heads/s/H/login', { kind: 'stream', stream: 'H', topic: 'login' }],
    ['integrator/wave-1', { kind: 'integrator', topic: 'wave-1' }],
    ['dependabot/cargo/serde-1.0.200', { kind: 'dependabot' }],
    ['dependabot/github_actions/actions/checkout-5', { kind: 'dependabot' }],
    ['main', { kind: 'invalid' }],
    ['feature/login', { kind: 'invalid' }],
    ['s/C', { kind: 'invalid' }],
    ['s//topic', { kind: 'invalid' }],
    ['integrator/', { kind: 'invalid' }],
    ['dependabot', { kind: 'invalid' }],
    ['', { kind: 'invalid' }],
  ];
  for (const [branch, expected] of cases) {
    const parsed = parseBranch(branch);
    for (const [key, value] of Object.entries(expected)) assert.equal(parsed[key], value, `${branch}: ${key}`);
  }
});

test('a stream branch may touch its own paths and shared files', () => {
  const r = checkBranch(
    's/C/store-fix',
    ['crates/store/src/lib.rs', 'crates/store/migrations/0105_add_index.sql', 'crates/store/Cargo.toml', 'Cargo.lock'],
    ownership,
  );
  assert.equal(r.ok, true);
  assert.deepEqual(r.violations, []);
});

test('a stream branch touching another stream reports the owner', () => {
  const r = checkBranch(
    's/C/store-fix',
    ['crates/store/src/lib.rs', 'crates/api/src/x.rs', 'crates/store/migrations/0205_x.sql', 'nowhere/file.txt'],
    ownership,
  );
  assert.equal(r.ok, false);
  assert.deepEqual(r.violations, [
    { file: 'crates/api/src/x.rs', owners: ['H'] },
    { file: 'crates/store/migrations/0205_x.sql', owners: ['E'] },
    { file: 'nowhere/file.txt', owners: [] },
  ]);
  const text = formatResult(r, ownership).join('\n');
  assert.match(text, /crates\/api\/src\/x\.rs is owned by stream H \(API and auth\); branch s\/C\/store-fix may only touch stream C paths or shared files/);
  assert.match(text, /nowhere\/file\.txt is not owned by any stream/);
  assert.match(text, /crates\/store\/src\/\*\*/);
});

test('stream 0 owns only the 00* migrations', () => {
  assert.equal(checkBranch('s/0/init', ['crates/store/migrations/0001_init.sql'], ownership).ok, true);
  assert.equal(checkBranch('s/0/init', ['crates/store/migrations/0101_x.sql'], ownership).ok, false);
});

test('an unknown stream fails', () => {
  const r = checkBranch('s/Z/x', ['crates/store/src/lib.rs'], ownership);
  assert.equal(r.ok, false);
  assert.equal(r.error, 'unknown-stream');
  assert.match(formatResult(r, ownership).join('\n'), /names stream "Z".*\n.*Known streams: 0, A, B/);
});

test('an integrator branch may touch anything', () => {
  const r = checkBranch('integrator/wave-1', ['crates/api/src/x.rs', 'apps/ui/src/console/x.tsx', 'nowhere/x'], ownership);
  assert.equal(r.ok, true);
  assert.match(formatResult(r, ownership).join('\n'), /notice: integrator\/wave-1 is an integrator branch/);
});

test('a dependabot branch may touch only dependency manifests', () => {
  const manifests = ['crates/api/Cargo.toml', 'Cargo.toml', 'Cargo.lock', 'apps/ui/package.json', 'pnpm-lock.yaml', '.github/workflows/ci.yml'];
  assert.equal(checkBranch('dependabot/cargo/serde-1.0.200', manifests, ownership).ok, true);
  const r = checkBranch('dependabot/cargo/serde-1.0.200', [...manifests, 'crates/api/src/lib.rs'], ownership);
  assert.equal(r.ok, false);
  assert.deepEqual(r.violations, [{ file: 'crates/api/src/lib.rs', owners: ['H'] }]);
  assert.match(formatResult(r, ownership).join('\n'), /Dependabot branches may only change files matching/);
});

test('any other branch name fails with the naming rule', () => {
  const r = checkBranch('feature/login', ['README.md'], ownership);
  assert.equal(r.ok, false);
  assert.equal(r.error, 'bad-branch-name');
  const text = formatResult(r, ownership).join('\n');
  assert.match(text, /does not follow the naming rule/);
  assert.match(text, /s\/<stream>\/<topic>/);
  assert.match(text, /integrator\/<topic>/);
});

test('changed paths are normalised and de-duplicated', () => {
  const r = checkBranch('s/C/x', ['crates\\store\\src\\lib.rs', './crates/store/src/lib.rs', 'crates\\api\\src\\x.rs'], ownership);
  assert.deepEqual(r.files, ['crates/api/src/x.rs', 'crates/store/src/lib.rs']);
  assert.deepEqual(r.violations, [{ file: 'crates/api/src/x.rs', owners: ['H'] }]);
});

test('CLI: violations exit 1 and name the owning stream', () => {
  const r = runScript('path-guard.mjs', ['--branch', 's/C/x', '--files', 'crates/api/src/x.rs,crates/store/src/lib.rs']);
  assert.equal(r.code, 1, r.output);
  assert.match(r.stdout, /crates\/api\/src\/x\.rs is owned by stream H/);
});

test('CLI: a clean change exits 0 with a one-line summary', () => {
  const r = runScript('path-guard.mjs', ['--files', 'crates/store/src/lib.rs\nCargo.lock'], {
    env: { GITHUB_HEAD_REF: 's/C/x' },
  });
  assert.equal(r.code, 0, r.output);
  assert.equal(r.stdout.trim().split('\n').length, 1);
  assert.match(r.stdout, /ok: 2 changed files on s\/C\/x/);
});

test('CLI: --branch wins over GITHUB_HEAD_REF', () => {
  const r = runScript('path-guard.mjs', ['--branch', 'integrator/x', '--files', 'crates/api/src/x.rs'], {
    env: { GITHUB_HEAD_REF: 's/C/x' },
  });
  assert.equal(r.code, 0, r.output);
});

test('CLI: bad branch names and unknown streams exit 1', () => {
  assert.equal(runScript('path-guard.mjs', ['--branch', 'main', '--files', 'README.md']).code, 1);
  const r = runScript('path-guard.mjs', ['--branch', 's/Z/x', '--files', 'README.md']);
  assert.equal(r.code, 1);
  assert.match(r.stdout, /Known streams/);
});

test('CLI: usage and config errors exit 2', (t) => {
  assert.equal(runScript('path-guard.mjs', ['--nope']).code, 2);
  assert.equal(runScript('path-guard.mjs', ['--branch', 's/C/x', '--base', '-x']).code, 2);
  const dir = makeTempDir(t);
  const bad = join(dir, 'ownership.json');
  writeFileSync(bad, '{"streams": {}}');
  const r = runScript('path-guard.mjs', ['--branch', 's/C/x', '--files', 'a', '--ownership', bad]);
  assert.equal(r.code, 2);
  assert.match(r.stderr, /at least one stream/);
});

test('CLI: --ownership uses another config', (t) => {
  const dir = makeTempDir(t);
  const file = join(dir, 'ownership.json');
  writeFileSync(file, JSON.stringify({ streams: { X: { paths: ['x/**'] } } }));
  assert.equal(runScript('path-guard.mjs', ['--branch', 's/X/t', '--files', 'x/a.rs', '--ownership', file]).code, 0);
  assert.equal(runScript('path-guard.mjs', ['--branch', 's/X/t', '--files', 'y/a.rs', '--ownership', file]).code, 1);
});

test('CLI: GitHub annotations on Actions', () => {
  const r = runScript('path-guard.mjs', ['--branch', 's/C/x', '--files', 'crates/api/src/x.rs'], {
    env: { GITHUB_ACTIONS: 'true' },
  });
  assert.equal(r.code, 1);
  assert.match(r.stdout, /^::error file=crates\/api\/src\/x\.rs::crates\/api\/src\/x\.rs is owned by stream H/m);
});
