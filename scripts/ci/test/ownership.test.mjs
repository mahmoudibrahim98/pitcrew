import assert from 'node:assert/strict';
import { join } from 'node:path';
import test from 'node:test';
import { ConfigError } from '../lib/cli.mjs';
import {
  createOwnership,
  findDuplicateKeys,
  loadOwnership,
  parseOwnership,
  validateOwnership,
} from '../lib/ownership.mjs';
import { makeTempDir } from './_support.mjs';

const real = loadOwnership();

test('the real ownership.json is valid', () => {
  assert.ok(real.streams.size > 0);
  for (const stream of real.streams.values()) assert.ok(stream.paths.length > 0, `stream ${stream.id} has paths`);
});

test('owners of paths in the real ownership.json', () => {
  const cases = [
    ['crates/store/migrations/0001_init.sql', ['0']],
    ['crates/store/migrations/0105_x.sql', ['C']],
    ['crates/store/migrations/0205_x.sql', ['E']],
    ['crates/store/migrations/0301_x.sql', ['F']],
    ['crates/store/migrations/0401_x.sql', ['G']],
    ['crates/store/migrations/0501_x.sql', ['O']],
    ['crates/store/src/lib.rs', ['C']],
    ['crates/store/Cargo.toml', ['C']],
    ['crates/store/README.md', ['C']],
    ['crates/api/src/x.rs', ['H']],
    ['apps/ui/package.json', ['L']],
    ['apps/ui/src/main.tsx', ['L']],
    ['apps/ui/src/console/x.tsx', ['M']],
    ['apps/ui/src/projects/board.tsx', ['N']],
    ['apps/ui/src/onboarding/import.tsx', ['O']],
    ['.github/workflows/ci.yml', ['0']],
    ['.github/workflows/release-nightly.yml', ['P']],
    ['.github/scrub/hashes.txt', ['0']],
    ['scripts/ci/path-guard.mjs', ['0']],
    ['Cargo.toml', ['0']],
    ['crates/store/src\\lib.rs', ['C']],
    ['crates/unknown/src/lib.rs', []],
  ];
  for (const [path, owners] of cases) assert.deepEqual(real.ownersOf(path), owners, path);
});

test('lock files are shared and owned by no stream', () => {
  for (const path of ['Cargo.lock', 'pnpm-lock.yaml']) {
    assert.equal(real.isShared(path), true, path);
    assert.deepEqual(real.ownersOf(path), [], path);
  }
  assert.equal(real.isShared('crates/store/Cargo.lock'), false);
  assert.equal(real.isShared('crates/store/src/lib.rs'), false);
});

test('describe() names the stream', () => {
  assert.equal(real.describe('H'), 'stream H (API and auth)');
  assert.equal(createOwnership({ streams: { X: { paths: ['x/**'] } } }).describe('X'), 'stream X');
});

test('validation rejects broken configs with a readable reason', () => {
  const bad = [
    [null, /top level/],
    [{}, /"streams" must be an object/],
    [{ streams: {} }, /at least one stream/],
    [{ streams: { A: {} } }, /streams\.A\.paths must be a non-empty array/],
    [{ streams: { A: { paths: [] } } }, /streams\.A\.paths must be a non-empty array/],
    [{ streams: { A: { paths: [''] } } }, /non-empty string/],
    [{ streams: { A: { paths: ['/abs/**'] } } }, /relative to the repo root/],
    [{ streams: { A: { paths: ['a\\b'] } } }, /write paths with '\/'/],
    [{ streams: { A: { paths: ['a/../b'] } } }, /'\.\.'/],
    [{ streams: { 'a/b': { paths: ['x/**'] } } }, /stream id "a\/b"/],
    [{ streams: { A: { paths: ['x/**'], name: 3 } } }, /name must be a string/],
    [{ streams: { A: { paths: ['x/**'] } }, shared: 'Cargo.lock' }, /shared must be an array/],
  ];
  for (const [raw, pattern] of bad) {
    assert.match(validateOwnership(raw).join('\n'), pattern, JSON.stringify(raw));
    assert.throws(() => createOwnership(raw), ConfigError);
  }
  assert.deepEqual(validateOwnership({ streams: { A: { paths: ['a/**'] } } }), []);
});

test('duplicate stream ids are rejected even though JSON.parse would hide them', () => {
  const text = '{"streams": {"A": {"paths": ["a/**"]}, "A": {"paths": ["b/**"]}}}';
  assert.throws(() => parseOwnership(text), (err) => err instanceof ConfigError && /repeats the key\(s\) "A"/.test(err.message));
});

test('findDuplicateKeys only compares keys within the same object', () => {
  assert.deepEqual(findDuplicateKeys('{"a": 1, "b": {"a": 2}, "c": [{"a": 3}, {"a": 4}]}'), []);
  assert.deepEqual(findDuplicateKeys('{"a": "x\\"y", "b": ["a", "a"]}'), []);
  assert.deepEqual(findDuplicateKeys('{"a": 1, "b": [{"c": 1, "c": 2}]}'), ['c']);
});

test('parse and load errors are config errors', (t) => {
  assert.throws(() => parseOwnership('{ not json'), (err) => err instanceof ConfigError && /not valid JSON/.test(err.message));
  const dir = makeTempDir(t);
  assert.throws(() => loadOwnership(join(dir, 'missing.json')), (err) => err instanceof ConfigError && /cannot read/.test(err.message));
});
