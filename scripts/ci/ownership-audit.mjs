// Checks that every tracked file is owned by exactly one stream (or is shared),
// and warns about globs in docs/build/ownership.json that match nothing.
// Usage: node scripts/ci/ownership-audit.mjs [--untracked] [--files <list>] [--ownership <file>]
import { EXIT_OK, EXIT_VIOLATION, annotation, parseCli, runIfMain, splitList } from './lib/cli.mjs';
import { listFiles } from './lib/git.mjs';
import { matches, normalizePath } from './lib/glob.mjs';
import { OWNERSHIP_FILE, loadOwnership } from './lib/ownership.mjs';

const NAME = 'ownership-audit';

export const USAGE = `Usage: node scripts/ci/ownership-audit.mjs [options]
  --untracked         also check files not yet added to git (ignored files are skipped)
  --files <list>      check this list instead of asking git (comma or newline separated)
  --ownership <file>  ownership file to use (default: ${OWNERSHIP_FILE})`;

export function auditOwnership(files, ownership) {
  const paths = [...new Set(files.map(normalizePath).filter(Boolean))].sort();
  const unowned = [];
  const multiOwned = [];
  let shared = 0;
  for (const file of paths) {
    if (ownership.isShared(file)) {
      shared++;
      continue;
    }
    const owners = ownership.ownersOf(file);
    if (owners.length === 0) unowned.push(file);
    else if (owners.length > 1) multiOwned.push({ file, owners });
  }

  const unusedGlobs = [];
  const unused = (glob) => !paths.some((file) => matches(glob, file));
  for (const stream of ownership.streams.values()) {
    for (const glob of stream.paths) if (unused(glob)) unusedGlobs.push({ owner: `stream ${stream.id}`, glob });
  }
  for (const glob of ownership.shared) if (unused(glob)) unusedGlobs.push({ owner: 'shared', glob });

  return {
    ok: unowned.length === 0 && multiOwned.length === 0,
    files: paths.length,
    shared,
    unowned,
    multiOwned,
    unusedGlobs,
  };
}

const plural = (n, word, many = `${word}s`) => `${n} ${n === 1 ? word : many}`;

export function formatAudit(report, ownership, env = {}) {
  const lines = [];
  const annotate = (level, message, where) => {
    const line = annotation(level, message, where, env);
    if (line) lines.push(line);
  };

  lines.push(
    `${NAME}: ${plural(report.files, 'file')} checked against ${plural(ownership.streams.size, 'stream')} ` +
      `(${report.shared} shared).`,
  );
  if (report.unowned.length) {
    lines.push('');
    lines.push(`error: ${plural(report.unowned.length, 'file has', 'files have')} no owner. Add a glob for each to exactly one stream in ${OWNERSHIP_FILE} (a stream 0 change):`);
    for (const file of report.unowned) {
      annotate('error', `${file} is not owned by any stream in ${OWNERSHIP_FILE}.`, { file });
      lines.push(`  ${file}`);
    }
  }
  if (report.multiOwned.length) {
    lines.push('');
    lines.push(`error: ${plural(report.multiOwned.length, 'file is', 'files are')} owned by more than one stream. Narrow the globs so exactly one stream matches:`);
    for (const { file, owners } of report.multiOwned) {
      annotate('error', `${file} is owned by streams ${owners.join(', ')}; it must have exactly one owner.`, { file });
      lines.push(`  ${file}  (streams ${owners.join(', ')})`);
    }
  }
  if (report.unusedGlobs.length) {
    lines.push('');
    lines.push(`warning: ${plural(report.unusedGlobs.length, 'glob matches', 'globs match')} no file yet (fine for streams that have not started):`);
    for (const { owner, glob } of report.unusedGlobs) lines.push(`  ${owner}: ${glob}`);
  }
  lines.push('');
  lines.push(
    report.ok
      ? `${NAME}: ok: every file has exactly one owner or is shared.`
      : `${NAME}: failed: ${report.unowned.length} without an owner, ${report.multiOwned.length} with several owners.`,
  );
  return lines;
}

export async function main(argv, env) {
  const opts = parseCli(
    argv,
    {
      untracked: { type: 'boolean', default: false },
      files: { type: 'string' },
      ownership: { type: 'string' },
      help: { type: 'boolean', short: 'h' },
    },
    USAGE,
  );
  if (opts.help) {
    console.log(USAGE);
    return EXIT_OK;
  }
  const ownership = loadOwnership(opts.ownership);
  const files = opts.files !== undefined ? splitList(opts.files) : listFiles({ untracked: opts.untracked });
  const report = auditOwnership(files, ownership);
  for (const line of formatAudit(report, ownership, env)) console.log(line);
  return report.ok ? EXIT_OK : EXIT_VIOLATION;
}

await runIfMain(import.meta.url, NAME, main);
