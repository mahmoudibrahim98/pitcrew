// Shared plumbing for the CI guard scripts: errors, argument parsing, entry detection,
// GitHub annotations. Exit codes: 0 ok, 1 violations, 2 usage or config error.
import { realpathSync } from 'node:fs';
import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { parseArgs } from 'node:util';

export const EXIT_OK = 0;
export const EXIT_VIOLATION = 1;
export const EXIT_CONFIG = 2;

// A problem with how the script was called or configured (not a violation).
export class ConfigError extends Error {
  constructor(message) {
    super(message);
    this.name = 'ConfigError';
  }
}

// True when the module with this import.meta.url is the file node was asked to run.
export function isMain(metaUrl) {
  const entry = process.argv[1];
  if (!entry) return false;
  let real;
  try {
    real = realpathSync.native(entry);
  } catch {
    real = resolve(entry);
  }
  return pathToFileURL(real).href === metaUrl;
}

export function parseCli(argv, options, usage) {
  try {
    return parseArgs({ args: argv, options, allowPositionals: false, strict: true }).values;
  } catch (err) {
    if (String(err.code).startsWith('ERR_PARSE_ARGS')) throw new ConfigError(`${err.message}\n\n${usage}`);
    throw err;
  }
}

// "a,b" or "a\nb" (or both) -> ['a', 'b']
export function splitList(value) {
  return String(value ?? '')
    .split(/[,\r\n]+/)
    .map((s) => s.trim())
    .filter(Boolean);
}

// Git revisions come from the command line; never let one be read as a git option.
export function checkRef(ref, flag = '--base') {
  if (!ref || ref.startsWith('-') || /\s/.test(ref)) {
    throw new ConfigError(`${flag} must be a git revision such as origin/main (got ${JSON.stringify(ref)})`);
  }
  return ref;
}

// GitHub Actions workflow command, e.g. ::error file=a.rs,line=3::message. Empty outside Actions.
export function annotation(level, message, { file, line } = {}, env = process.env) {
  if (env.GITHUB_ACTIONS !== 'true') return '';
  const props = [];
  if (file) props.push(`file=${escapeProperty(file)}`);
  if (file && line) props.push(`line=${line}`);
  return `::${level}${props.length ? ' ' + props.join(',') : ''}::${escapeData(message)}`;
}

function escapeData(s) {
  return String(s).replace(/%/g, '%25').replace(/\r/g, '%0D').replace(/\n/g, '%0A');
}

function escapeProperty(s) {
  return escapeData(s).replace(/:/g, '%3A').replace(/,/g, '%2C');
}

// Runs main(argv, env) when this module is the entry point and turns the result into an exit code.
export async function runIfMain(metaUrl, name, main) {
  if (!isMain(metaUrl)) return;
  try {
    process.exitCode = await main(process.argv.slice(2), process.env);
  } catch (err) {
    if (err instanceof ConfigError) {
      console.error(`${name}: ${err.message}`);
    } else {
      console.error(`${name}: unexpected error`);
      console.error(err);
    }
    process.exitCode = EXIT_CONFIG;
  }
}
