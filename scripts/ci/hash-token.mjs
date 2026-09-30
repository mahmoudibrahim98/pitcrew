// Prints the SHA-256 of each word (lowercased), one per line, so a private word can be added to
// .github/scrub/hashes.txt without being written down anywhere:
//   node scripts/ci/hash-token.mjs <word> [<word> ...] >> .github/scrub/hashes.txt
//   echo <word> | node scripts/ci/hash-token.mjs >> .github/scrub/hashes.txt
// Warnings go to stderr and name words by position only.
import { readFileSync } from 'node:fs';
import { ConfigError, EXIT_OK, runIfMain } from './lib/cli.mjs';
import { sha256 } from './scrub-gate.mjs';

const NAME = 'hash-token';

export const USAGE = `Usage: node scripts/ci/hash-token.mjs <word> [<word> ...]
       <words on stdin, separated by spaces or newlines> | node scripts/ci/hash-token.mjs
Prints one lowercase SHA-256 per word, ready to append to .github/scrub/hashes.txt.`;

export function normalizeWord(word) {
  return String(word).trim().toLowerCase();
}

export function hashWord(word) {
  return sha256(normalizeWord(word));
}

// Why the scrub gate might not match this word the way the maintainer expects.
export function wordWarnings(word) {
  const w = normalizeWord(word);
  if (!/^[a-z0-9][a-z0-9._@\/-]*$/.test(w)) {
    return ['contains characters outside a-z 0-9 . _ @ / - (or starts with a separator), so the gate never sees it whole; hash its parts instead'];
  }
  if (/[._@\/-]/.test(w)) {
    return ['contains . _ @ / or -, so it only matches where it stands alone as a whole token; consider also hashing its parts'];
  }
  return [];
}

export async function main(argv) {
  if (argv.includes('--help') || argv.includes('-h')) {
    console.log(USAGE);
    return EXIT_OK;
  }
  let words = argv;
  if (words.length === 0) {
    if (process.stdin.isTTY) throw new ConfigError(`no words given\n\n${USAGE}`);
    words = readFileSync(0, 'utf8').replace(/^﻿/, '').split(/\s+/);
  }
  words = words.map(normalizeWord).filter(Boolean);
  if (words.length === 0) throw new ConfigError(`no words given\n\n${USAGE}`);

  words.forEach((word, i) => {
    for (const warning of wordWarnings(word)) console.error(`${NAME}: warning: word ${i + 1} ${warning}`);
    console.log(hashWord(word));
  });
  return EXIT_OK;
}

await runIfMain(import.meta.url, NAME, main);
