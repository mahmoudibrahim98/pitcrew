// Checks the initial JavaScript budget after `build`: the entry script and every module it
// preloads, gzipped, must stay under 250 kB. Lazy chunks (routes, the palette) do not count.
// Usage: node check-size.mjs [dist]

import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { gzipSync } from 'node:zlib';

const BUDGET = 250 * 1000;
const dist = process.argv[2] ?? 'dist';
const html = readFileSync(join(dist, 'index.html'), 'utf8');

const initial = new Set();
for (const match of html.matchAll(/<(?:script[^>]*\ssrc|link[^>]*rel="modulepreload"[^>]*\shref)="([^"]+\.js)"/g)) {
  initial.add(match[1]);
}
if (initial.size === 0) {
  console.error(`No entry script found in ${join(dist, 'index.html')}`);
  process.exit(1);
}

let total = 0;
for (const src of initial) {
  const size = gzipSync(readFileSync(join(dist, src.replace(/^\//, '')))).length;
  total += size;
  console.log(`${(size / 1000).toFixed(1).padStart(8)} kB  ${src}`);
}
console.log(`${(total / 1000).toFixed(1).padStart(8)} kB  initial JS, gzipped (budget ${BUDGET / 1000} kB)`);
if (total > BUDGET) {
  console.error('Over budget.');
  process.exit(1);
}
