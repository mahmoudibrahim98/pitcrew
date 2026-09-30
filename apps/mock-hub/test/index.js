// Node 21+ treats `node --test <folder>` as a single entry point and resolves the folder to this
// file, so `node --test apps/mock-hub/test/` (the command in the root package.json) runs every
// *.test.ts file next to it. `node --test "apps/mock-hub/test/**/*.test.ts"` also works and runs
// each file in its own process.

import { readdirSync } from 'node:fs';

const files = readdirSync(import.meta.dirname).filter((name) => name.endsWith('.test.ts'));
for (const name of files.sort()) {
  await import(`./${name}`);
}
