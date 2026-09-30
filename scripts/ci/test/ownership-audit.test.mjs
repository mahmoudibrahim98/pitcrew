import assert from 'node:assert/strict';
import { writeFileSync } from 'node:fs';
import { join } from 'node:path';
import test from 'node:test';
import { createOwnership, loadOwnership } from '../lib/ownership.mjs';
import { auditOwnership, formatAudit } from '../ownership-audit.mjs';
import { makeTempDir, runScript } from './_support.mjs';

const config = {
  shared: ['lock'],
  streams: {
    X: { name: 'Ex', paths: ['a/**'] },
    Y: { name: 'Why', paths: ['a/b/**', 'c/*'] },
    Z: { paths: ['never/**'] },
  },
};

test('finds unowned and double-owned files and unused globs', () => {
  const ownership = createOwnership(config);
  const report = auditOwnership(['a/x', 'a/b/y', 'c/d', 'c/d/e', 'e/f', 'lock', 'a\\x'], ownership);
  assert.equal(report.ok, false);
  assert.equal(report.files, 6);
  assert.equal(report.shared, 1);
  assert.deepEqual(report.unowned, ['c/d/e', 'e/f']);
  assert.deepEqual(report.multiOwned, [{ file: 'a/b/y', owners: ['X', 'Y'] }]);
  assert.deepEqual(report.unusedGlobs, [{ owner: 'stream Z', glob: 'never/**' }]);

  const text = formatAudit(report, ownership).join('\n');
  assert.match(text, /error: 2 files have no owner/);
  assert.match(text, /^ {2}e\/f$/m);
  assert.match(text, /error: 1 file is owned by more than one stream/);
  assert.match(text, /a\/b\/y {2}\(streams X, Y\)/);
  assert.match(text, /warning: 1 glob matches no file yet/);
  assert.match(text, /stream Z: never\/\*\*/);
});

test('a clean tree passes; unused globs only warn', () => {
  const ownership = createOwnership(config);
  const report = auditOwnership(['a/x', 'c/d', 'lock'], ownership);
  assert.equal(report.ok, true);
  assert.deepEqual(report.unusedGlobs.map((u) => u.glob), ['a/b/**', 'never/**']);
  assert.match(formatAudit(report, ownership).at(-1), /ok: every file has exactly one owner/);
});

test('shared files are never reported, even when a stream glob matches them', () => {
  const ownership = createOwnership({ shared: ['a/lock'], streams: { X: { paths: ['a/**'] }, Y: { paths: ['a/*'] } } });
  const report = auditOwnership(['a/lock'], ownership);
  assert.equal(report.ok, true);
});

test('the real ownership.json gives the skeleton files exactly one owner', () => {
  const report = auditOwnership(
    [
      'Cargo.toml',
      'Cargo.lock',
      'crates/store/migrations/0001_init.sql',
      'crates/store/src/lib.rs',
      'crates/store/README.md',
      'apps/desktop/src-tauri/src/main.rs',
      'docs/build/ownership.json',
      '.github/scrub/hashes.txt',
      'scripts/ci/ownership-audit.mjs',
    ],
    loadOwnership(),
  );
  assert.deepEqual(report.unowned, []);
  assert.deepEqual(report.multiOwned, []);
});

test('CLI: --files takes a newline or comma list and exits 1 on errors', (t) => {
  const dir = makeTempDir(t);
  const file = join(dir, 'ownership.json');
  writeFileSync(file, JSON.stringify(config));
  const r = runScript('ownership-audit.mjs', ['--ownership', file, '--files', 'a/x\r\na/b/y,e/f']);
  assert.equal(r.code, 1, r.output);
  assert.match(r.stdout, /3 files checked against 3 streams/);
  assert.match(r.stdout, /e\/f/);
  assert.match(r.stdout, /a\/b\/y {2}\(streams X, Y\)/);

  const ok = runScript('ownership-audit.mjs', ['--ownership', file, '--files', 'a/x,c/d,lock']);
  assert.equal(ok.code, 0, ok.output);
});

test('CLI: real ownership, unowned file, GitHub annotation', () => {
  const r = runScript('ownership-audit.mjs', ['--files', 'crates/store/src/lib.rs,stray/file.txt'], {
    env: { GITHUB_ACTIONS: 'true' },
  });
  assert.equal(r.code, 1, r.output);
  assert.match(r.stdout, /^::error file=stray\/file\.txt::stray\/file\.txt is not owned by any stream/m);
});

test('CLI: unknown option exits 2', () => {
  assert.equal(runScript('ownership-audit.mjs', ['--bogus']).code, 2);
});
