// Blocks private data from landing in this public repository.
// Words to block are stored only as SHA-256 hashes (.github/scrub/hashes.txt), and extra regexes
// can come from the SCRUB_PATTERNS secret. Output names file, line and hash prefix or pattern
// number, never the matching text, because CI logs are public.
// Usage: node scripts/ci/scrub-gate.mjs [--base <ref>] [--untracked] [--files <list>] [--hashes <file>]
import { createHash } from 'node:crypto';
import { lstatSync, readFileSync, readlinkSync } from 'node:fs';
import { isAbsolute, relative, resolve, sep } from 'node:path';
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
import { REPO_ROOT, commitsInRange, listFiles } from './lib/git.mjs';
import { normalizePath } from './lib/glob.mjs';

const NAME = 'scrub-gate';
export const HASHES_FILE = '.github/scrub/hashes.txt';
export const TOKEN_RE = /[a-z0-9][a-z0-9._@\/-]*/g;
export const SEPARATOR_RE = /[._@\/-]+/;
export const MAX_FILE_BYTES = 5 * 1024 * 1024;
const SNIFF_BYTES = 8 * 1024;
const PREFIX_LENGTH = 12;
const COMMIT_FIELDS = ['author name', 'author email', 'committer name', 'committer email'];

export const USAGE = `Usage: node scripts/ci/scrub-gate.mjs [options]
  --base <ref>      also scan messages, names and emails of the commits in <ref>..HEAD
  --untracked       also scan files not yet added to git (ignored files are skipped)
  --files <list>    scan these files instead of the tracked ones (comma or newline separated,
                    relative to the repo root)
  --hashes <file>   hash list to use (default: ${HASHES_FILE})
Environment:
  SCRUB_PATTERNS    optional newline-separated JavaScript regexes, matched case-insensitively
                    against every line (surrounding spaces on each line are trimmed)`;

export function sha256(text) {
  return createHash('sha256').update(text, 'utf8').digest('hex');
}

export function parseHashes(text, source = HASHES_FILE) {
  const hashes = new Set();
  text
    .replace(/^﻿/, '')
    .split('\n')
    .forEach((line, i) => {
      const value = line.replace(/#.*/, '').trim().toLowerCase();
      if (!value) return;
      if (!/^[0-9a-f]{64}$/.test(value)) {
        throw new ConfigError(`${source}:${i + 1} is not a SHA-256 digest (expected 64 hex characters)`);
      }
      hashes.add(value);
    });
  return hashes;
}

export function loadHashes(file, source = file) {
  let text;
  try {
    text = readFileSync(file, 'utf8');
  } catch (err) {
    throw new ConfigError(`cannot read the hash list ${source}: ${err.code ?? err.message}`);
  }
  return parseHashes(text, source);
}

// Patterns are secret, so errors name the pattern by its number only.
export function parsePatterns(text) {
  const sources = String(text ?? '')
    .split('\n')
    .map((line) => line.trim())
    .filter(Boolean);
  return sources.map((source, i) => {
    let re;
    try {
      re = new RegExp(source, 'i');
    } catch {
      throw new ConfigError(`SCRUB_PATTERNS #${i + 1} is not a valid JavaScript regular expression (not shown: it is secret)`);
    }
    if (re.test('')) throw new ConfigError(`SCRUB_PATTERNS #${i + 1} matches an empty line, so it would flag every line`);
    return re;
  });
}

// What gets hashed for one token: the token, the token without trailing separators,
// and each part between separators.
export function candidates(token) {
  const out = new Set([token]);
  const trimmed = token.replace(/[._@\/-]+$/, '');
  if (trimmed) out.add(trimmed);
  for (const part of token.split(SEPARATOR_RE)) if (part) out.add(part);
  return out;
}

export function createScanner(hashes, patterns = []) {
  const memo = new Map();
  const listed = (s) => {
    let hit = memo.get(s);
    if (hit === undefined) {
      const digest = sha256(s);
      hit = hashes.has(digest) ? digest : null;
      memo.set(s, hit);
    }
    return hit;
  };
  const globalPatterns = patterns.map((re) => new RegExp(re.source, 'gi'));

  // Listed hashes (full hex, unique) and 1-based pattern numbers found in one line of text.
  function scanLine(line) {
    const found = new Set();
    for (const [token] of line.toLowerCase().matchAll(TOKEN_RE)) {
      for (const c of candidates(token)) {
        const digest = listed(c);
        if (digest) found.add(digest);
      }
    }
    const matched = [];
    patterns.forEach((re, i) => {
      if (re.test(line)) matched.push(i + 1);
    });
    return { hashes: [...found], patterns: matched };
  }

  // Replaces listed words and pattern matches with *** so a path can be printed safely.
  function redact(text) {
    const lower = text.toLowerCase();
    if (lower.length !== text.length) {
      const r = scanLine(text);
      return r.hashes.length || r.patterns.length ? '[redacted]' : text;
    }
    const ranges = [];
    for (const m of lower.matchAll(TOKEN_RE)) {
      const token = m[0];
      const start = m.index;
      if (listed(token)) ranges.push([start, start + token.length]);
      const trimmed = token.replace(/[._@\/-]+$/, '');
      if (trimmed && listed(trimmed)) ranges.push([start, start + trimmed.length]);
      for (const part of token.matchAll(/[^._@\/-]+/g)) {
        if (listed(part[0])) ranges.push([start + part.index, start + part.index + part[0].length]);
      }
    }
    for (const re of globalPatterns) {
      for (const m of text.matchAll(re)) if (m[0]) ranges.push([m.index, m.index + m[0].length]);
    }
    if (ranges.length === 0) return text;
    ranges.sort((a, b) => a[0] - b[0]);
    let out = '';
    let pos = 0;
    for (const [start, end] of ranges) {
      if (start >= pos) {
        out += `${text.slice(pos, start)}***`;
        pos = end;
      } else if (end > pos) {
        pos = end;
      }
    }
    return out + text.slice(pos);
  }

  return { scanLine, redact, hashCount: hashes.size, patternCount: patterns.length };
}

// Hits in a block of text: [{ line, hash }] and [{ line, pattern }], lines 1-based.
export function scanText(text, scanner) {
  const hits = [];
  text.split('\n').forEach((raw, i) => {
    const line = raw.endsWith('\r') ? raw.slice(0, -1) : raw;
    const { hashes, patterns } = scanner.scanLine(line);
    for (const hash of hashes) hits.push({ line: i + 1, hash });
    for (const pattern of patterns) hits.push({ line: i + 1, pattern });
  });
  return hits;
}

// { text } for a readable text file, otherwise { skip: reason }.
export function readTextFile(file) {
  let stat;
  try {
    stat = lstatSync(file);
  } catch {
    return { skip: 'missing' };
  }
  if (stat.isSymbolicLink()) return { text: readlinkSync(file) };
  if (!stat.isFile()) return { skip: 'not a file' };
  if (stat.size > MAX_FILE_BYTES) return { skip: 'larger than 5 MiB' };
  const buf = readFileSync(file);
  if (buf.subarray(0, SNIFF_BYTES).includes(0)) return { skip: 'binary' };
  return { text: buf.toString('utf8') };
}

const pathKey = (p) => (process.platform === 'win32' ? resolve(p).toLowerCase() : resolve(p));

function insideRoot(root, abs) {
  const rel = relative(root, abs);
  return rel !== '' && rel !== '..' && !rel.startsWith(`..${sep}`) && !isAbsolute(rel) ? normalizePath(rel) : null;
}

// Scans file contents, and the repo-relative path of each file, for listed words and patterns.
export function scanFiles(files, { root = REPO_ROOT, scanner, skip = [] }) {
  const skipKeys = new Set(skip.map(pathKey));
  const hits = [];
  const skipped = [];
  let scanned = 0;
  for (const file of files) {
    const abs = resolve(root, file);
    const shown = normalizePath(file);
    const display = scanner.redact(shown);
    if (skipKeys.has(pathKey(abs))) {
      skipped.push({ file: display, reason: 'hash list' });
      continue;
    }
    const rel = insideRoot(root, abs);
    if (rel) {
      const { hashes, patterns } = scanner.scanLine(rel);
      for (const hash of hashes) hits.push({ file: display, clean: false, where: 'path', hash });
      for (const pattern of patterns) hits.push({ file: display, clean: false, where: 'path', pattern });
    }
    const read = readTextFile(abs);
    if (read.skip) {
      skipped.push({ file: display, reason: read.skip });
      continue;
    }
    scanned++;
    for (const hit of scanText(read.text, scanner)) {
      hits.push({ file: display, clean: display === shown, where: 'content', ...hit });
    }
  }
  return { hits, skipped, scanned };
}

// Scans names, emails and messages of the commits in base..HEAD.
export function scanCommits(base, scanner, { cwd } = {}) {
  const hits = [];
  const commits = commitsInRange(base, { cwd });
  for (const { sha, text } of commits) {
    text
      .replace(/\n+$/, '')
      .split('\n')
      .forEach((raw, i) => {
        const line = raw.endsWith('\r') ? raw.slice(0, -1) : raw;
        const field = i < COMMIT_FIELDS.length ? COMMIT_FIELDS[i] : `message line ${i - COMMIT_FIELDS.length + 1}`;
        const { hashes, patterns } = scanner.scanLine(line);
        for (const hash of hashes) hits.push({ sha, field, hash });
        for (const pattern of patterns) hits.push({ sha, field, pattern });
      });
  }
  return { hits, count: commits.length };
}

const what = (hit) =>
  hit.hash ? `hashed word ${hit.hash.slice(0, PREFIX_LENGTH)}` : `SCRUB_PATTERNS #${hit.pattern}`;

const plural = (n, word, many = `${word}s`) => `${n} ${n === 1 ? word : many}`;

export function formatScrub({ files, commits, scanner }, env = {}) {
  const lines = [];
  const annotate = (message, where) => {
    const line = annotation('error', message, where, env);
    if (line) lines.push(line);
  };

  const skippedBy = new Map();
  for (const { reason } of files.skipped) skippedBy.set(reason, (skippedBy.get(reason) ?? 0) + 1);
  const skippedText = skippedBy.size ? ` (skipped: ${[...skippedBy].map(([r, n]) => `${n} ${r}`).join(', ')})` : '';
  const commitText = commits ? ` and ${plural(commits.count, 'commit')}` : '';
  lines.push(
    `${NAME}: scanned ${plural(files.scanned, 'file')}${skippedText}${commitText} for ` +
      `${plural(scanner.hashCount, 'hashed word')} and ${plural(scanner.patternCount, 'pattern')}.`,
  );

  const all = [...files.hits, ...(commits?.hits ?? [])];
  if (all.length === 0) {
    lines.push(`${NAME}: ok: no private data found.`);
    return lines;
  }

  lines.push(`${NAME}: found ${plural(all.length, 'match', 'matches')} of private data. The matching text is never printed.`);
  for (const hit of files.hits) {
    const where = hit.where === 'path' ? `file name ${hit.file}` : `${hit.file}:${hit.line}`;
    annotate(`Private data (${what(hit)}).`, hit.clean ? { file: hit.file, line: hit.line } : {});
    lines.push(`  ${where}: ${what(hit)}`);
  }
  for (const hit of commits?.hits ?? []) {
    annotate(`Private data in commit ${hit.sha.slice(0, PREFIX_LENGTH)} ${hit.field} (${what(hit)}).`);
    lines.push(`  commit ${hit.sha.slice(0, PREFIX_LENGTH)} ${hit.field}: ${what(hit)}`);
  }
  lines.push('');
  lines.push('Remove the text (or rename the file) and push again. For a commit, reword or re-author it');
  lines.push('(git rebase -i <base>, git commit --amend --reset-author) and force-push the branch.');
  lines.push('To check which word a hash prefix stands for, run node scripts/ci/hash-token.mjs <guess> locally.');
  return lines;
}

export async function main(argv, env) {
  const opts = parseCli(
    argv,
    {
      base: { type: 'string' },
      untracked: { type: 'boolean', default: false },
      files: { type: 'string' },
      hashes: { type: 'string' },
      help: { type: 'boolean', short: 'h' },
    },
    USAGE,
  );
  if (opts.help) {
    console.log(USAGE);
    return EXIT_OK;
  }

  if (opts.base !== undefined) checkRef(opts.base);
  const hashesSource = opts.hashes ?? HASHES_FILE;
  const hashesPath = resolve(REPO_ROOT, hashesSource);
  const scanner = createScanner(loadHashes(hashesPath, hashesSource), parsePatterns(env.SCRUB_PATTERNS));
  if (scanner.hashCount === 0 && scanner.patternCount === 0) {
    console.log(`${NAME}: warning: ${hashesSource} is empty and SCRUB_PATTERNS is not set; nothing to look for.`);
  }

  const list = opts.files !== undefined ? splitList(opts.files) : listFiles({ untracked: opts.untracked });
  const files = scanFiles(list, {
    root: REPO_ROOT,
    scanner,
    skip: [resolve(REPO_ROOT, HASHES_FILE), hashesPath],
  });
  const commits = opts.base !== undefined ? scanCommits(opts.base, scanner) : null;

  for (const line of formatScrub({ files, commits, scanner }, env)) console.log(line);
  return files.hits.length || commits?.hits.length ? EXIT_VIOLATION : EXIT_OK;
}

await runIfMain(import.meta.url, NAME, main);
