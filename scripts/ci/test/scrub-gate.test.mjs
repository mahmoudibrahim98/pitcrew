import assert from 'node:assert/strict';
import { writeFileSync } from 'node:fs';
import { join } from 'node:path';
import test from 'node:test';
import { ConfigError } from '../lib/cli.mjs';
import {
  candidates,
  createScanner,
  parseHashes,
  parsePatterns,
  readTextFile,
  scanCommits,
  scanFiles,
  scanText,
  sha256,
} from '../scrub-gate.mjs';
import { makeGitRepo, makeTempDir, runScript, writeFiles } from './_support.mjs';

// A made-up word standing in for private data.
const WORD = 'zebracorn';
const HASH = sha256(WORD);
const PREFIX = HASH.slice(0, 12);
const neverPrinted = (output) => assert.ok(!output.toLowerCase().includes(WORD), 'output must not contain the word');

test('sha256 of utf-8 text', () => {
  assert.equal(sha256('abc'), 'ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad');
});

test('candidates: whole token, token without trailing separators, and its parts', () => {
  assert.deepEqual([...candidates('foo.bar/baz-')].sort(), ['bar', 'baz', 'foo', 'foo.bar/baz', 'foo.bar/baz-'].sort());
  assert.deepEqual([...candidates('plain')], ['plain']);
});

test('parseHashes ignores comments and blank lines, accepts CRLF and upper case', () => {
  const text = `# comment\r\n\r\n${HASH}\r\n  ${sha256('x').toUpperCase()}  # trailing comment\n`;
  assert.deepEqual([...parseHashes(text)], [HASH, sha256('x')]);
  assert.throws(
    () => parseHashes(`# ok\n${HASH}\nnot-a-hash\n`, 'hashes.txt'),
    (err) => err instanceof ConfigError && /hashes\.txt:3 is not a SHA-256 digest/.test(err.message),
  );
});

test('parsePatterns: one case-insensitive regex per non-blank line; errors never show the pattern', () => {
  const patterns = parsePatterns('quokka-\\d+\r\n\r\n  wombat[0-9]{3}  \n');
  assert.equal(patterns.length, 2);
  assert.equal(patterns[0].test('QUOKKA-7'), true);
  assert.equal(patterns[1].test('Wombat123'), true);
  assert.deepEqual(parsePatterns(undefined), []);
  assert.throws(
    () => parsePatterns('fine\n(quokka'),
    (err) => err instanceof ConfigError && /SCRUB_PATTERNS #2/.test(err.message) && !err.message.includes('quokka'),
  );
  assert.throws(() => parsePatterns('quokka\nx?'), (err) => err instanceof ConfigError && /#2 matches an empty line/.test(err.message));
});

test('scanText finds the word in any case, inside paths and emails, and only as a whole part', () => {
  const scanner = createScanner(new Set([HASH]));
  const text = [
    'nothing here', // 1
    'We met Zebracorn.', // 2
    'see vendor/zebracorn-kit/x', // 3
    'mail ZEBRACORN@example.com', // 4
    'zebracorns and zebra corn and xzebracorn', // 5
    'zebracorn zebracorn', // 6: one hit per line
  ].join('\r\n');
  assert.deepEqual(scanText(text, scanner), [
    { line: 2, hash: HASH },
    { line: 3, hash: HASH },
    { line: 4, hash: HASH },
    { line: 6, hash: HASH },
  ]);
});

test('multi-part words match as a whole token', () => {
  const scanner = createScanner(new Set([sha256('acme.corp')]));
  assert.equal(scanText('visit acme.corp.', scanner).length, 1);
  assert.equal(scanText('visit acme', scanner).length, 0);
});

test('patterns report the 1-based pattern number', () => {
  const scanner = createScanner(new Set(), parsePatterns('quokka-\\d+\nwombat[0-9]{3}'));
  assert.deepEqual(scanText('a QUOKKA-42 b\nnone\nWOMBAT123', scanner), [
    { line: 1, pattern: 1 },
    { line: 3, pattern: 2 },
  ]);
});

test('redact hides listed words and pattern matches', () => {
  const scanner = createScanner(new Set([HASH]), parsePatterns('quokka-\\d+'));
  assert.equal(scanner.redact('docs/Zebracorn-notes/quokka-12.md'), 'docs/***-notes/***.md');
  assert.equal(scanner.redact('docs/plain.md'), 'docs/plain.md');
});

test('readTextFile skips binary and large files', (t) => {
  const dir = makeTempDir(t);
  writeFileSync(join(dir, 'bin'), Buffer.from([0x7a, 0x00, 0x7a]));
  writeFileSync(join(dir, 'big'), Buffer.alloc(5 * 1024 * 1024 + 1, 0x61));
  writeFileSync(join(dir, 'ok'), 'text');
  assert.deepEqual(readTextFile(join(dir, 'bin')), { skip: 'binary' });
  assert.deepEqual(readTextFile(join(dir, 'big')), { skip: 'larger than 5 MiB' });
  assert.deepEqual(readTextFile(join(dir, 'missing')), { skip: 'missing' });
  assert.deepEqual(readTextFile(join(dir, 'ok')), { text: 'text' });
});

test('scanFiles checks repo-relative file names too, and prints them redacted', (t) => {
  const root = makeTempDir(t);
  writeFiles(root, { 'docs/zebracorn/readme.md': 'clean\n' });
  const scanner = createScanner(new Set([HASH]));
  const result = scanFiles(['docs/zebracorn/readme.md'], { root, scanner });
  assert.equal(result.scanned, 1);
  assert.deepEqual(result.hits, [{ file: 'docs/***/readme.md', clean: false, where: 'path', hash: HASH }]);
});

test('CLI: finds the word, reports file:line and hash prefix, never the word', (t) => {
  const dir = makeTempDir(t, 'pitcrew-scrub-');
  const hashes = join(dir, 'hashes.txt');
  writeFiles(dir, {
    'hashes.txt': `# test list\n\n${HASH}\n`,
    'notes.md': 'first line\nsecond line\nwe met Zebracorn.\n',
    'nested.txt': 'path: vendor/zebracorn-kit/x\n',
    'clean.md': 'zebracorns everywhere\nzebra corn\n',
    'blob.bin': Buffer.concat([Buffer.from([0]), Buffer.from(WORD)]),
    'zebracorn-notes.md': 'about ZEBRACORN\n',
  });
  const files = ['notes.md', 'nested.txt', 'clean.md', 'blob.bin', 'zebracorn-notes.md', 'hashes.txt'].map((f) => join(dir, f));

  const r = runScript('scrub-gate.mjs', ['--hashes', hashes, '--files', files.join(',')]);
  assert.equal(r.code, 1, r.output);
  assert.match(r.stdout, new RegExp(`notes\\.md:3: hashed word ${PREFIX}`));
  assert.match(r.stdout, new RegExp(`nested\\.txt:1: hashed word ${PREFIX}`));
  assert.match(r.stdout, /\*\*\*-notes\.md:1: hashed word/);
  assert.doesNotMatch(r.stdout, /clean\.md/);
  assert.match(r.stdout, /scanned 4 files \(skipped: 1 binary, 1 hash list\)/);
  assert.match(r.stdout, /found 3 matches/);
  neverPrinted(r.output);

  const annotated = runScript('scrub-gate.mjs', ['--hashes', hashes, '--files', files.join(',')], {
    env: { GITHUB_ACTIONS: 'true' },
  });
  assert.match(annotated.stdout, /^::error file=.*notes\.md,line=3::Private data \(hashed word/m);
  neverPrinted(annotated.output);
});

test('CLI: clean files exit 0', (t) => {
  const dir = makeTempDir(t);
  writeFiles(dir, { 'hashes.txt': `${HASH}\n`, 'a.md': 'nothing to see\n' });
  const r = runScript('scrub-gate.mjs', ['--hashes', join(dir, 'hashes.txt'), '--files', join(dir, 'a.md')]);
  assert.equal(r.code, 0, r.output);
  assert.match(r.stdout, /ok: no private data found/);
});

test('CLI: SCRUB_PATTERNS hits report the pattern number only', (t) => {
  const dir = makeTempDir(t);
  writeFiles(dir, { 'hashes.txt': '# empty\n', 'cfg.txt': 'host = QUOKKA-42.internal\nnothing\nid: wombat123\n' });
  const r = runScript('scrub-gate.mjs', ['--hashes', join(dir, 'hashes.txt'), '--files', join(dir, 'cfg.txt')], {
    env: { SCRUB_PATTERNS: 'quokka-\\d+\r\n\r\nwombat[0-9]{3}\n' },
  });
  assert.equal(r.code, 1, r.output);
  assert.match(r.stdout, /cfg\.txt:1: SCRUB_PATTERNS #1/);
  assert.match(r.stdout, /cfg\.txt:3: SCRUB_PATTERNS #2/);
  assert.doesNotMatch(r.output.toLowerCase(), /quokka|wombat/);
});

test('CLI: an invalid pattern or hash list is a config error (exit 2)', (t) => {
  const dir = makeTempDir(t);
  writeFiles(dir, { 'hashes.txt': '# empty\n', 'bad.txt': 'nope\n', 'a.md': 'x\n' });
  const bad = runScript('scrub-gate.mjs', ['--hashes', join(dir, 'hashes.txt'), '--files', join(dir, 'a.md')], {
    env: { SCRUB_PATTERNS: 'ok\n(quokka' },
  });
  assert.equal(bad.code, 2, bad.output);
  assert.match(bad.stderr, /SCRUB_PATTERNS #2/);
  assert.doesNotMatch(bad.output.toLowerCase(), /quokka/);

  assert.equal(runScript('scrub-gate.mjs', ['--hashes', join(dir, 'bad.txt'), '--files', join(dir, 'a.md')]).code, 2);
  assert.equal(runScript('scrub-gate.mjs', ['--hashes', join(dir, 'missing.txt'), '--files', join(dir, 'a.md')]).code, 2);
  const badBase = runScript('scrub-gate.mjs', ['--hashes', join(dir, 'hashes.txt'), '--files', join(dir, 'a.md'), '--base=--output=x']);
  assert.equal(badBase.code, 2, badBase.output);
  assert.match(badBase.stderr, /--base must be a git revision/);
});

test('commit names, emails and messages in base..HEAD are scanned', (t) => {
  const dir = makeTempDir(t);
  const { repo, run } = makeGitRepo(dir);
  run('commit', '-q', '--allow-empty', '-m', 'initial');
  run('tag', 'base');
  run('commit', '-q', '--allow-empty', '-m', 'clean change', '-m', 'body mentions Zebracorn here');
  run('commit', '-q', '--allow-empty', '--author', 'Zebracorn Fan <fan@example.com>', '-m', 'third');
  const [third, second] = run('rev-list', 'base..HEAD').trim().split('\n');

  const scanner = createScanner(new Set([HASH]));
  const result = scanCommits('base', scanner, { cwd: repo });
  assert.equal(result.count, 2);
  assert.deepEqual(result.hits, [
    { sha: third, field: 'author name', hash: HASH },
    { sha: second, field: 'message line 3', hash: HASH },
  ]);
});
