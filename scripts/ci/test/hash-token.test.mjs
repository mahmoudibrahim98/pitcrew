import assert from 'node:assert/strict';
import test from 'node:test';
import { hashWord, wordWarnings } from '../hash-token.mjs';
import { sha256 } from '../scrub-gate.mjs';
import { runScript } from './_support.mjs';

test('hashWord lowercases and trims', () => {
  assert.equal(hashWord('  ZebraCorn '), sha256('zebracorn'));
});

test('wordWarnings flags words the gate cannot see whole', () => {
  assert.deepEqual(wordWarnings('zebracorn'), []);
  assert.match(wordWarnings('acme.corp')[0], /whole token/);
  assert.match(wordWarnings("o'brien")[0], /never sees it whole/);
});

test('CLI: hashes arguments, one per line', () => {
  const r = runScript('hash-token.mjs', ['Zebracorn', 'quokka']);
  assert.equal(r.code, 0, r.output);
  assert.equal(r.stdout, `${sha256('zebracorn')}\n${sha256('quokka')}\n`);
});

test('CLI: reads words from stdin', () => {
  const r = runScript('hash-token.mjs', [], { input: '﻿Zebracorn\r\n  quokka \n' });
  assert.equal(r.code, 0, r.output);
  assert.equal(r.stdout, `${sha256('zebracorn')}\n${sha256('quokka')}\n`);
});

test('CLI: warnings name the word by position only', () => {
  const r = runScript('hash-token.mjs', ['zebracorn', 'acme.corp']);
  assert.equal(r.code, 0);
  assert.match(r.stderr, /word 2 contains/);
  assert.doesNotMatch(r.stderr, /acme/);
});

test('CLI: no words is a usage error', () => {
  assert.equal(runScript('hash-token.mjs', [], { input: '  \n' }).code, 2);
});
