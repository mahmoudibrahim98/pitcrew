// Writes THIRD-PARTY-NOTICES.txt for what PitCrew's programs contain: every crate the shipped
// binaries link (cargo metadata, normal dependencies, for one target) and every JavaScript package
// the app's window bundles (apps/ui's production dependencies), each with its licence and the
// licence files its package carries. Packages that share a text are listed under it once.
//
//   node packaging/notices.mjs --target TRIPLE --out FILE [--title TEXT]
//
// Needs cargo (crate sources are fetched by `cargo metadata` if missing) and apps/ui's
// node_modules (`pnpm install`). Paths never reach the output. packaging/notices.test.mjs tests
// it on synthetic packages. See packaging/README.md, "The portable Windows zip".
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { existsSync, readFileSync, readdirSync, realpathSync, statSync, writeFileSync } from 'node:fs';
import { basename, dirname, join, relative, resolve, sep } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');

/** The packages of each Rust workspace whose binaries PitCrew ships. */
export const RUST = [
  { manifest: 'Cargo.toml', packages: ['pitcrew-daemon', 'pitcrew-cli', 'pitcrew-ptyd', 'pitcrew-remote'] },
  { manifest: 'apps/desktop/src-tauri/Cargo.toml', packages: ['pitcrew-desktop'] },
];
/** The app whose production dependencies the desktop bundles. */
export const UI = 'apps/ui';

const LICENCE_FILE = /^(licen[cs]e|copying|copyright|notice|unlicense|third[-_]party)([-_. ].*)?$/i;
const MAX_TEXT = 512 * 1024;

/** A licence file's text: UTF-8, no BOM, LF line ends, no trailing blank lines. */
export function cleanText(text) {
  return text.replace(/^﻿/, '').replace(/\r\n?/g, '\n').replace(/\s+$/, '') + '\n';
}

/** The licence files in a package's folder (and a REUSE-style LICENSES/), as texts. */
export function licenceTexts(dir, extra = []) {
  const files = new Set(extra.map((f) => resolve(dir, f)));
  const look = (folder) => {
    let names;
    try { names = readdirSync(folder); } catch { return; }
    for (const name of names) {
      const path = join(folder, name);
      let stat;
      try { stat = statSync(path); } catch { continue; }
      if (stat.isFile() && (folder !== dir || LICENCE_FILE.test(name))) files.add(path);
      else if (stat.isDirectory() && folder === dir && /^licen[cs]es$/i.test(name)) look(path);
    }
  };
  look(dir);
  const texts = [];
  for (const path of [...files].sort()) {
    let stat;
    try { stat = statSync(path); } catch { continue; }
    if (!stat.isFile() || stat.size > MAX_TEXT) continue;
    texts.push({ name: relative(dir, path).split(sep).join('/'), text: cleanText(readFileSync(path, 'utf8')) });
  }
  return texts;
}

/**
 * The third-party crates `roots` (package names of the workspace in `metadata`, `cargo metadata
 * --format-version 1` output) link: their normal dependencies, transitively. Path packages are
 * the repository's own and are left out.
 */
export function rustPackages(metadata, roots) {
  const byId = new Map(metadata.packages.map((p) => [p.id, p]));
  const nodes = new Map(metadata.resolve.nodes.map((n) => [n.id, n]));
  const members = new Set(metadata.workspace_members);
  const queue = [];
  for (const name of roots) {
    const pkg = metadata.packages.find((p) => p.name === name && members.has(p.id));
    if (!pkg) throw new Error(`no workspace package ${name}`);
    queue.push(pkg.id);
  }
  const seen = new Set();
  while (queue.length > 0) {
    const id = queue.pop();
    if (seen.has(id)) continue;
    seen.add(id);
    for (const dep of nodes.get(id)?.deps ?? []) {
      if (dep.dep_kinds.some((k) => k.kind === null)) queue.push(dep.pkg);
    }
  }
  return [...seen]
    .map((id) => byId.get(id))
    .filter((p) => p && p.source !== null)
    .map((p) => ({
      kind: 'rust',
      name: p.name,
      version: p.version,
      license: p.license ?? (p.license_file ? `see ${p.license_file}` : 'not stated'),
      repository: p.repository ?? p.homepage ?? `https://crates.io/crates/${p.name}`,
      texts: licenceTexts(dirname(p.manifest_path), p.license_file ? [p.license_file] : []),
    }));
}

function readJson(path) {
  return JSON.parse(readFileSync(path, 'utf8'));
}

/** Where `name` resolves from `dir`, as Node resolves it: the nearest node_modules/name up. */
function resolvePackage(dir, name) {
  for (let at = dir; ; at = dirname(at)) {
    const modules = basename(at) === 'node_modules' ? at : join(at, 'node_modules');
    const candidate = join(modules, name);
    if (existsSync(join(candidate, 'package.json'))) return realpathSync(candidate);
    if (dirname(at) === at) return null;
  }
}

function npmLicense(pkg) {
  if (typeof pkg.license === 'string') return pkg.license;
  if (pkg.license?.type) return pkg.license.type;
  if (Array.isArray(pkg.licenses)) return pkg.licenses.map((l) => l.type ?? l).join(' OR ');
  return 'not stated';
}

function npmRepository(pkg) {
  const repo = typeof pkg.repository === 'string' ? pkg.repository : pkg.repository?.url;
  return (repo ?? pkg.homepage ?? `https://www.npmjs.com/package/${pkg.name}`)
    .replace(/^git\+/, '')
    .replace(/^git:\/\//, 'https://')
    .replace(/\.git$/, '');
}

/**
 * The third-party packages the app at `appDir` bundles: its `dependencies` and theirs,
 * transitively. Optional dependencies (native add-ons for Node, such as pdfjs-dist's canvas) are
 * not bundled for the window and are left out, as are packages outside any node_modules (the
 * workspace's own, whose dependencies are followed).
 */
export function npmPackages(appDir) {
  const found = new Map();
  const seen = new Set();
  const queue = [{ dir: realpathSync(appDir), names: Object.keys(readJson(join(appDir, 'package.json')).dependencies ?? {}) }];
  while (queue.length > 0) {
    const { dir, names } = queue.pop();
    for (const name of names) {
      const at = resolvePackage(dir, name);
      if (at === null) throw new Error(`${name} is not installed (run pnpm install)`);
      if (seen.has(at)) continue;
      seen.add(at);
      const pkg = readJson(join(at, 'package.json'));
      queue.push({ dir: at, names: Object.keys(pkg.dependencies ?? {}) });
      if (!at.split(sep).includes('node_modules')) continue;
      const key = `${pkg.name}@${pkg.version}`;
      if (found.has(key)) continue;
      found.set(key, {
        kind: 'npm',
        name: pkg.name,
        version: pkg.version,
        license: npmLicense(pkg),
        repository: npmRepository(pkg),
        texts: licenceTexts(at),
      });
    }
  }
  return [...found.values()];
}

/** `words`, joined by ", " and wrapped at 78 columns, the lines after the first indented. */
function wrap(first, words) {
  const lines = [first];
  for (const [i, word] of words.entries()) {
    const piece = i < words.length - 1 ? `${word},` : word;
    const line = lines[lines.length - 1];
    if (line.length + 1 + piece.length > 78 && line.trim() !== '') lines.push(`  ${piece}`);
    else lines[lines.length - 1] = `${line} ${piece}`;
  }
  return lines.join('\n');
}

const byName = (a, b) => (a.name < b.name ? -1 : a.name > b.name ? 1 : a.version < b.version ? -1 : a.version > b.version ? 1 : 0);

/** Each package once (by name and version), sorted. */
export const dedupe = (list) => [...new Map(list.map((p) => [`${p.name}@${p.version}`, p])).values()].sort(byName);

/** The notices file: the packages, then each distinct licence text with who uses it. */
export function render(title, rust, npm) {
  const groups = [
    ['Rust crates in the programs', dedupe(rust)],
    ["JavaScript packages in the app's window", dedupe(npm)],
  ];
  const texts = new Map(); // sha256 -> { n, text, users: [] }
  const refs = new Map(); // package -> [n]
  for (const [, list] of groups) {
    for (const p of list) {
      const ns = [];
      for (const { text } of p.texts) {
        const hash = createHash('sha256').update(text).digest('hex');
        if (!texts.has(hash)) texts.set(hash, { n: texts.size + 1, text, users: [] });
        const entry = texts.get(hash);
        if (!entry.users.includes(`${p.name} ${p.version}`)) entry.users.push(`${p.name} ${p.version}`);
        if (!ns.includes(entry.n)) ns.push(entry.n);
      }
      refs.set(p, ns);
    }
  }
  const out = [
    title,
    '',
    'PitCrew is licensed under the Apache License 2.0 (LICENSE and NOTICE). Its programs are also',
    'built from the open-source packages below, each under its own licence. The licence texts the',
    'packages carry follow the list, numbered; packages that share a text are listed under it.',
    '',
  ];
  for (const [heading, list] of groups) {
    out.push(`${heading} (${list.length})`, '');
    for (const p of list) {
      const ns = refs.get(p);
      out.push(`  ${p.name} ${p.version}`, `    licence: ${p.license}`, `    source: ${p.repository}`);
      out.push(ns.length > 0 ? `    texts: ${ns.join(', ')}` : '    texts: none in the package; see its source');
    }
    out.push('');
  }
  for (const { n, text, users } of texts.values()) {
    out.push('='.repeat(78), wrap(`Text ${n}, carried by:`, users), '-'.repeat(78), text);
  }
  return out.join('\n').replace(/\n*$/, '\n');
}

function cargoMetadata(manifest, target) {
  const out = execFileSync(
    'cargo',
    ['metadata', '--format-version', '1', '--locked', '--filter-platform', target, '--manifest-path', join(ROOT, manifest)],
    { cwd: ROOT, maxBuffer: 512 * 1024 * 1024, encoding: 'utf8', stdio: ['ignore', 'pipe', 'inherit'] },
  );
  return JSON.parse(out);
}

function main(argv) {
  const args = { title: 'PitCrew: third-party notices' };
  for (let i = 0; i < argv.length; i += 2) {
    const [flag, value] = [argv[i], argv[i + 1]];
    if (value === undefined) throw new Error(`${flag} needs a value`);
    if (flag === '--target') args.target = value;
    else if (flag === '--out') args.out = value;
    else if (flag === '--title') args.title = value;
    else throw new Error(`unknown option ${flag}`);
  }
  if (!args.target || !args.out) throw new Error('usage: node packaging/notices.mjs --target TRIPLE --out FILE [--title TEXT]');
  const rust = dedupe(RUST.flatMap(({ manifest, packages }) => rustPackages(cargoMetadata(manifest, args.target), packages)));
  const npm = dedupe(npmPackages(join(ROOT, UI)));
  writeFileSync(args.out, render(args.title, rust, npm));
  const missing = [...rust, ...npm].filter((p) => p.texts.length === 0).length;
  console.log(`${args.out}: ${rust.length} crates and ${npm.length} JavaScript packages; ${missing} carry no licence file`);
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  try {
    main(process.argv.slice(2));
  } catch (e) {
    console.error(`::error::notices: ${e.message}`);
    process.exit(1);
  }
}
