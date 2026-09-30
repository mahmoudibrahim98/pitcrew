// Pull request guard: a branch may only change the paths its stream owns.
//   s/<stream>/<topic>   paths of that stream, plus shared files
//   integrator/<topic>   anything
//   dependabot/...       dependency manifests only
// Usage: node scripts/ci/path-guard.mjs [--branch <name>] [--base <ref>] [--files a,b,c] [--ownership <file>]
import {
  ConfigError,
  EXIT_OK,
  EXIT_VIOLATION,
  annotation,
  checkRef,
  parseCli,
  runIfMain,
  splitList,
} from './lib/cli.mjs';
import { changedFiles, currentBranch, showFile } from './lib/git.mjs';
import { matches, matchesAny, normalizePath } from './lib/glob.mjs';
import { OWNERSHIP_FILE, loadOwnership, parseOwnership } from './lib/ownership.mjs';

const NAME = 'path-guard';

export const USAGE = `Usage: node scripts/ci/path-guard.mjs [options]
  --branch <name>     branch to check (default: $GITHUB_HEAD_REF, then the current git branch)
  --base <ref>        compare HEAD against this ref (default: origin/main)
  --files <a,b,c>     check this list instead of asking git (comma or newline separated)
  --ownership <file>  ownership file to use (default: ${OWNERSHIP_FILE} as it is on --base)`;

export const NAMING_RULE = `Branch names decide which files a pull request may change:
  s/<stream>/<topic>   only paths owned by that stream (docs/build/ownership.json), plus shared files
  integrator/<topic>   cross-stream integration work; any path
  dependabot/<...>     dependency manifests only (opened by Dependabot)`;

export function parseBranch(branch) {
  const name = String(branch ?? '').trim().replace(/^refs\/heads\//, '');
  let m = /^s\/([^/]+)\/(.+)$/.exec(name);
  if (m) return { kind: 'stream', name, stream: m[1], topic: m[2] };
  m = /^integrator\/(.+)$/.exec(name);
  if (m) return { kind: 'integrator', name, topic: m[1] };
  if (matches('dependabot/**', name)) return { kind: 'dependabot', name };
  return { kind: 'invalid', name };
}

// Decides whether branch may change files. Pure: returns what happened, prints nothing.
export function checkBranch(branch, files, ownership) {
  const parsed = parseBranch(branch);
  const paths = [...new Set(files.map(normalizePath).filter(Boolean))].sort();
  const base = { ...parsed, branch: parsed.name, files: paths, violations: [] };
  const offending = (allowed) =>
    paths.filter((file) => !allowed(file)).map((file) => ({ file, owners: ownership.ownersOf(file) }));

  if (parsed.kind === 'integrator') return { ...base, ok: true };
  if (parsed.kind === 'invalid') return { ...base, ok: false, error: 'bad-branch-name' };
  if (parsed.kind === 'dependabot') {
    const violations = offending((file) => ownership.isDependabotPath(file));
    return { ...base, ok: violations.length === 0, violations, allowed: ownership.dependabot };
  }
  const stream = ownership.streams.get(parsed.stream);
  if (!stream) return { ...base, ok: false, error: 'unknown-stream' };
  const violations = offending((file) => ownership.isShared(file) || matchesAny(stream.paths, file));
  return { ...base, ok: violations.length === 0, violations, allowed: stream.paths };
}

function ownerText(owners, ownership) {
  if (owners.length === 0) return 'is not owned by any stream';
  if (owners.length === 1) return `is owned by ${ownership.describe(owners[0])}`;
  return `is owned by several streams (${owners.join(', ')})`;
}

const plural = (n, word) => `${n} ${word}${n === 1 ? '' : 's'}`;

// Turns a checkBranch result into printable lines.
export function formatResult(result, ownership, env = {}) {
  const lines = [];
  const add = (line) => lines.push(line);
  const annotate = (level, message, where = {}) => {
    const line = annotation(level, message, where, env);
    if (line) lines.push(line);
  };
  const { branch, files } = result;

  if (result.error === 'bad-branch-name') {
    annotate('error', `Branch "${branch}" does not follow the naming rule.`);
    add(`${NAME}: branch "${branch}" does not follow the naming rule.`);
    add(NAMING_RULE);
    add('Push the work to a branch named after its stream (for example s/C/fix-migrations) and open the pull request from it.');
    return lines;
  }
  if (result.error === 'unknown-stream') {
    const known = [...ownership.streams.keys()].join(', ');
    annotate('error', `Branch "${branch}" names stream "${result.stream}", which does not exist.`);
    add(`${NAME}: branch "${branch}" names stream "${result.stream}", which is not in ${OWNERSHIP_FILE}.`);
    add(`Known streams: ${known}.`);
    add(NAMING_RULE);
    return lines;
  }
  if (result.kind === 'integrator') {
    annotate('notice', `Integrator branch: path ownership is not enforced (${plural(files.length, 'file')} changed).`);
    add(`${NAME}: notice: ${branch} is an integrator branch, so it may change any path (${plural(files.length, 'changed file')}). Review cross-stream edits by hand.`);
    return lines;
  }

  const scope = result.kind === 'dependabot' ? 'dependency manifests' : `${ownership.describe(result.stream)} or shared`;
  if (result.ok) {
    add(`${NAME}: ok: ${plural(files.length, 'changed file')} on ${branch}, all within ${scope}.`);
    return lines;
  }

  const allowedText = result.kind === 'dependabot' ? 'dependency manifests' : `stream ${result.stream} paths or shared files`;
  add(`${NAME}: ${branch} changes ${plural(result.violations.length, 'file')} outside ${scope}:`);
  for (const { file, owners } of result.violations) {
    const message = `${file} ${ownerText(owners, ownership)}; branch ${branch} may only touch ${allowedText}.`;
    annotate('error', message, { file });
    add(`  ${message}`);
  }
  add('');
  if (result.kind === 'dependabot') {
    add('Dependabot branches may only change files matching:');
    for (const glob of result.allowed) add(`  ${glob}`);
  } else {
    add(`Branch ${branch} may only change files matching ${ownership.describe(result.stream)}:`);
    for (const glob of result.allowed) add(`  ${glob}`);
    if (ownership.shared.length) add(`and the shared files: ${ownership.shared.join(', ')}`);
    add('');
    add('Move the other changes to a pull request from the owning stream (s/<owner>/<topic>),');
    add('or ask for an integrator/<topic> branch if the change has to land across streams at once.');
    add(`Files owned by no stream need a glob in ${OWNERSHIP_FILE} first (a stream 0 change).`);
  }
  return lines;
}

// On a real PR, read ownership.json from the base so a PR cannot widen its own permissions.
function ownershipAtBase(base) {
  const text = showFile(base, OWNERSHIP_FILE);
  if (text === null) {
    console.log(`${NAME}: notice: ${base} has no ${OWNERSHIP_FILE}; using the working tree copy.`);
    return loadOwnership();
  }
  return parseOwnership(text, `${base}:${OWNERSHIP_FILE}`);
}

export async function main(argv, env) {
  const opts = parseCli(
    argv,
    {
      branch: { type: 'string' },
      base: { type: 'string', default: 'origin/main' },
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

  const branch = opts.branch ?? (env.GITHUB_HEAD_REF || currentBranch());
  if (!branch) throw new ConfigError(`no branch to check: pass --branch <name> or set GITHUB_HEAD_REF\n\n${USAGE}`);

  let files;
  let ownership;
  if (opts.files !== undefined) {
    files = splitList(opts.files);
    ownership = loadOwnership(opts.ownership);
  } else {
    checkRef(opts.base);
    files = changedFiles(opts.base);
    ownership = opts.ownership ? loadOwnership(opts.ownership) : ownershipAtBase(opts.base);
  }

  const result = checkBranch(branch, files, ownership);
  for (const line of formatResult(result, ownership, env)) console.log(line);
  return result.ok ? EXIT_OK : EXIT_VIOLATION;
}

await runIfMain(import.meta.url, NAME, main);
