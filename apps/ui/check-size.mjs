// Checks the initial JavaScript after `build`:
// - the entry script and every module it preloads, gzipped, must stay under 250 kB (lazy chunks,
//   such as routes and the palette, do not count);
// - the desktop gateway, and `@tauri-apps/api` with it, must not be in it: the desktop app loads
//   them on demand, so a browser never downloads them. They must be in a lazy chunk instead.
// Usage: node check-size.mjs [dist]

import { readdirSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { gzipSync } from 'node:zlib';

const BUDGET = 250 * 1000;
/**
 * Strings only the desktop data layer (`src/data/desktop.tsx`, `src/data/gateway.ts`) and
 * `@tauri-apps/api` contain.
 */
const DESKTOP_ONLY = ['cannot follow the gateway', 'gateway_socket_open', '__TAURI_TO_IPC_KEY__', 'plugin:event|listen'];

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
let failed = false;
const initialFiles = new Set();
for (const src of initial) {
  const file = src.replace(/^\//, '');
  initialFiles.add(file);
  const code = readFileSync(join(dist, file));
  const size = gzipSync(code).length;
  total += size;
  console.log(`${(size / 1000).toFixed(1).padStart(8)} kB  ${src}`);
  const text = code.toString('utf8');
  const found = DESKTOP_ONLY.filter((marker) => text.includes(marker));
  if (found.length > 0) {
    console.error(`Desktop-only code in the initial JS (${src}): ${found.join(', ')}`);
    failed = true;
  }
}
console.log(`${(total / 1000).toFixed(1).padStart(8)} kB  initial JS, gzipped (budget ${BUDGET / 1000} kB)`);
if (total > BUDGET) {
  console.error('Over budget.');
  failed = true;
}

// The gateway must still be built, in chunks the desktop app loads on demand.
const lazy = readdirSync(join(dist, 'assets'))
  .filter((name) => name.endsWith('.js') && !initialFiles.has(`assets/${name}`))
  .map((name) => ({ name, text: readFileSync(join(dist, 'assets', name), 'utf8') }))
  .filter(({ text }) => DESKTOP_ONLY.some((marker) => text.includes(marker)));
const missing = DESKTOP_ONLY.filter((marker) => !lazy.some(({ text }) => text.includes(marker)));
if (missing.length > 0) {
  console.error(`No lazy chunk holds the desktop gateway's ${missing.join(', ')}.`);
  failed = true;
} else {
  console.log(`Desktop gateway and @tauri-apps/api: lazy only (${lazy.map(({ name }) => `/assets/${name}`).join(', ')})`);
}
if (failed) process.exit(1);
