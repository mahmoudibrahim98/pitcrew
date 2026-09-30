// Entry point for `node --test scripts/ci/test/`. Since Node 21, --test treats its arguments as
// globs, so a bare directory is run as a module and resolves to this file. It loads every
// *.test.mjs here. When the runner discovers this file on its own (plain `node --test`), the
// .test.mjs files already run separately, so it does nothing. Valid as CommonJS and as ESM.
const entry = process.argv[1] || '';

(async () => {
  const fs = await import('node:fs');
  const { join } = await import('node:path');
  const { pathToFileURL } = await import('node:url');
  if (!fs.statSync(entry, { throwIfNoEntry: false })?.isDirectory()) return;
  const names = fs.readdirSync(entry).filter((name) => name.endsWith('.test.mjs')).sort();
  for (const name of names) await import(pathToFileURL(join(entry, name)).href);
})();
