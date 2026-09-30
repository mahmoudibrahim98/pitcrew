// Loads docs/build/ownership.json and answers "which stream owns this path?".
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { ConfigError } from './cli.mjs';
import { REPO_ROOT } from './git.mjs';
import { matches, matchesAny, normalizePath } from './glob.mjs';

export const OWNERSHIP_FILE = 'docs/build/ownership.json';
export const DEFAULT_OWNERSHIP_PATH = join(REPO_ROOT, OWNERSHIP_FILE);

const STREAM_ID = /^[A-Za-z0-9][A-Za-z0-9_-]*$/;

const isObject = (v) => v !== null && typeof v === 'object' && !Array.isArray(v);

// JSON.parse keeps only the last of two equal keys, which would silently drop a stream.
// Returns every key that appears twice in the same object. Expects valid JSON.
export function findDuplicateKeys(text) {
  const dups = [];
  const stack = [];
  let i = 0;
  while (i < text.length) {
    const c = text[i];
    if (c === '"') {
      let j = i + 1;
      while (j < text.length && text[j] !== '"') j += text[j] === '\\' ? 2 : 1;
      const str = JSON.parse(text.slice(i, j + 1));
      i = j + 1;
      let k = i;
      while (/\s/.test(text[k] ?? '')) k++;
      const keys = stack.at(-1);
      if (text[k] === ':' && keys) {
        if (keys.has(str)) dups.push(str);
        keys.add(str);
      }
      continue;
    }
    if (c === '{') stack.push(new Set());
    else if (c === '[') stack.push(null);
    else if (c === '}' || c === ']') stack.pop();
    i++;
  }
  return dups;
}

function globProblem(glob) {
  if (typeof glob !== 'string' || glob.trim() === '') return 'must be a non-empty string';
  if (glob !== glob.trim()) return 'has leading or trailing spaces';
  if (glob.includes('\\')) return "uses '\\'; write paths with '/'";
  if (glob.startsWith('/') || glob.startsWith('./')) return "must be relative to the repo root (no leading '/' or './')";
  if (glob.split('/').includes('..')) return "must not contain '..'";
  return null;
}

function checkGlobList(list, where, problems) {
  if (!Array.isArray(list)) {
    problems.push(`${where} must be an array of globs`);
    return;
  }
  list.forEach((glob, i) => {
    const problem = globProblem(glob);
    if (problem) problems.push(`${where}[${i}] ${JSON.stringify(glob)} ${problem}`);
  });
}

// Returns a list of human-readable problems; empty when the config is usable.
export function validateOwnership(raw) {
  const problems = [];
  if (!isObject(raw)) return ['the top level must be a JSON object'];
  if (!isObject(raw.streams) || Object.keys(raw.streams).length === 0) {
    problems.push('"streams" must be an object with at least one stream');
  } else {
    for (const [id, stream] of Object.entries(raw.streams)) {
      const where = `streams.${id}`;
      if (!STREAM_ID.test(id)) problems.push(`stream id ${JSON.stringify(id)} must be letters, digits, '-' or '_' (it is used in branch names s/<id>/<topic>)`);
      if (!isObject(stream)) {
        problems.push(`${where} must be an object with a "paths" array`);
        continue;
      }
      if (stream.name !== undefined && typeof stream.name !== 'string') problems.push(`${where}.name must be a string`);
      if (!Array.isArray(stream.paths) || stream.paths.length === 0) problems.push(`${where}.paths must be a non-empty array of globs`);
      else checkGlobList(stream.paths, `${where}.paths`, problems);
    }
  }
  if (raw.shared !== undefined) checkGlobList(raw.shared, 'shared', problems);
  if (raw.dependabot !== undefined) checkGlobList(raw.dependabot, 'dependabot', problems);
  return problems;
}

export function createOwnership(raw, source = OWNERSHIP_FILE) {
  const problems = validateOwnership(raw);
  if (problems.length) throw new ConfigError(`${source} is invalid:\n  - ${problems.join('\n  - ')}`);
  const streams = new Map(
    Object.entries(raw.streams).map(([id, s]) => [id, { id, name: s.name ?? '', paths: [...s.paths] }]),
  );
  const shared = [...(raw.shared ?? [])];
  const dependabot = [...(raw.dependabot ?? [])];
  return {
    source,
    streams,
    shared,
    dependabot,
    // Ids of every stream with a glob matching the path, in file order.
    ownersOf(path) {
      const p = normalizePath(path);
      return [...streams.values()].filter((s) => s.paths.some((g) => matches(g, p))).map((s) => s.id);
    },
    isShared(path) {
      return matchesAny(shared, path);
    },
    isDependabotPath(path) {
      return matchesAny(dependabot, path);
    },
    // "stream H (API and auth)"
    describe(id) {
      const s = streams.get(id);
      return s?.name ? `stream ${id} (${s.name})` : `stream ${id}`;
    },
  };
}

export function parseOwnership(text, source = OWNERSHIP_FILE) {
  let raw;
  try {
    raw = JSON.parse(text);
  } catch (err) {
    throw new ConfigError(`${source} is not valid JSON: ${err.message}`);
  }
  const dups = findDuplicateKeys(text);
  if (dups.length) {
    throw new ConfigError(
      `${source} repeats the key(s) ${dups.map((k) => JSON.stringify(k)).join(', ')}; stream ids must be unique`,
    );
  }
  return createOwnership(raw, source);
}

export function loadOwnership(file = DEFAULT_OWNERSHIP_PATH) {
  let text;
  try {
    text = readFileSync(file, 'utf8');
  } catch (err) {
    throw new ConfigError(`cannot read ${file}: ${err.message}`);
  }
  return parseOwnership(text.replace(/^﻿/, ''), file === DEFAULT_OWNERSHIP_PATH ? OWNERSHIP_FILE : file);
}
