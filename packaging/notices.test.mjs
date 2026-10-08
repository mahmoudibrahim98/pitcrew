// Tests for notices.mjs on synthetic packages: which crates and JavaScript packages count, their
// licence files, and the rendered file. Run: node --test packaging/notices.test.mjs
import assert from 'node:assert/strict';
import { mkdirSync, mkdtempSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import {
  cleanText, dedupe, extraTexts, htmlToText, licenceTexts, missingTexts, needsNotice, npmPackages, render, rustPackages, rustStd,
} from './notices.mjs';

function scratch(t) {
  const dir = mkdtempSync(join(tmpdir(), 'pitcrew-notices-'));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  return dir;
}

function write(path, text) {
  mkdirSync(join(path, '..'), { recursive: true });
  writeFileSync(path, text);
}

const MIT = 'MIT License\n\nCopyright (c) Example Authors\n';

test('licence files are read from the package and a LICENSES folder, cleaned', (t) => {
  const dir = scratch(t);
  write(join(dir, 'LICENSE-MIT'), `﻿${MIT.replace(/\n/g, '\r\n')}\r\n\r\n`);
  write(join(dir, 'COPYING'), 'copying\n');
  write(join(dir, 'LICENSES', 'Apache-2.0.txt'), 'apache\n');
  write(join(dir, 'src', 'LICENSE'), 'not read: a source folder');
  write(join(dir, 'README.md'), 'not read');
  const texts = licenceTexts(dir);
  assert.deepEqual(texts.map((t) => t.name), ['COPYING', 'LICENSE-MIT', 'LICENSES/Apache-2.0.txt']);
  assert.equal(texts.find((t) => t.name === 'LICENSE-MIT').text, MIT);
  assert.equal(cleanText('a\r\nb\r\n\n\n'), 'a\nb\n');
});

test('crates: normal dependencies of the shipped packages, not dev, build or our own', (t) => {
  const dir = scratch(t);
  const pkg = (name, source) => {
    const at = join(dir, name);
    write(join(at, 'Cargo.toml'), '');
    write(join(at, 'LICENSE'), `${name} licence\n`);
    return {
      id: `${name} 1.0.0`, name, version: '1.0.0', source, license: 'MIT', repository: `https://example.com/${name}`,
      license_file: null, manifest_path: join(at, 'Cargo.toml'),
    };
  };
  const registry = 'registry+https://github.com/rust-lang/crates.io-index';
  const packages = [
    pkg('pitcrew-daemon', null), pkg('pitcrew-store', null), pkg('pitcrew-test-only', null),
    pkg('serde', registry), pkg('criterion', registry), pkg('cc', registry), pkg('itoa', registry),
  ];
  const dep = (name, ...kinds) => ({ pkg: `${name} 1.0.0`, dep_kinds: kinds.map((kind) => ({ kind, target: null })) });
  const metadata = {
    packages,
    workspace_members: ['pitcrew-daemon 1.0.0', 'pitcrew-store 1.0.0', 'pitcrew-test-only 1.0.0'],
    resolve: {
      nodes: [
        { id: 'pitcrew-daemon 1.0.0', deps: [dep('pitcrew-store', null), dep('criterion', 'dev'), dep('cc', 'build')] },
        { id: 'pitcrew-store 1.0.0', deps: [dep('serde', null, 'dev')] },
        { id: 'serde 1.0.0', deps: [dep('itoa', null)] },
        { id: 'pitcrew-test-only 1.0.0', deps: [dep('criterion', null)] },
      ],
    },
  };
  const found = rustPackages(metadata, ['pitcrew-daemon']);
  assert.deepEqual(found.map((p) => p.name).sort(), ['itoa', 'serde']);
  const serde = found.find((p) => p.name === 'serde');
  assert.equal(serde.license, 'MIT');
  assert.deepEqual(serde.texts, [{ name: 'LICENSE', text: 'serde licence\n' }]);
  assert.throws(() => rustPackages(metadata, ['pitcrew-missing']), /no workspace package pitcrew-missing/);
});

test("JavaScript: the app's dependencies, resolved as Node does, without optional or workspace ones", (t) => {
  const dir = scratch(t);
  const app = join(dir, 'apps', 'ui');
  const manifest = (at, fields) => write(join(at, 'package.json'), JSON.stringify(fields));
  manifest(app, {
    name: '@example/ui',
    dependencies: { react: '1', '@scope/widgets': '2', '@example/tokens': 'workspace:*' },
    devDependencies: { vitest: '3' },
  });
  const modules = join(app, 'node_modules');
  manifest(join(modules, 'react'), { name: 'react', version: '1.0.0', license: 'MIT', dependencies: { scheduler: '1' } });
  write(join(modules, 'react', 'LICENSE'), MIT);
  // The nearest copy wins: react's own scheduler, not the hoisted one.
  manifest(join(modules, 'react', 'node_modules', 'scheduler'), { name: 'scheduler', version: '1.1.0', license: 'MIT' });
  manifest(join(modules, 'scheduler'), { name: 'scheduler', version: '9.9.9', license: 'MIT' });
  manifest(join(modules, '@scope', 'widgets'), {
    name: '@scope/widgets', version: '2.0.0', license: { type: 'Apache-2.0' },
    repository: { url: 'git+https://example.com/widgets.git' },
    optionalDependencies: { 'native-addon': '1' },
  });
  manifest(join(modules, 'native-addon'), { name: 'native-addon', version: '1.0.0', license: 'MIT' });
  manifest(join(modules, 'vitest'), { name: 'vitest', version: '3.0.0', license: 'MIT' });
  // A workspace package, linked as pnpm links it: followed, not listed.
  const tokens = join(dir, 'packages', 'tokens');
  manifest(tokens, { name: '@example/tokens', version: '0.0.0', dependencies: { colord: '2' } });
  manifest(join(tokens, 'node_modules', 'colord'), { name: 'colord', version: '2.0.0', license: 'MIT' });
  mkdirSync(join(modules, '@example'), { recursive: true });
  symlinkSync(tokens, join(modules, '@example', 'tokens'), 'junction');

  const found = dedupe(npmPackages(app));
  assert.deepEqual(found.map((p) => `${p.name} ${p.version}`), [
    '@scope/widgets 2.0.0', 'colord 2.0.0', 'react 1.0.0', 'scheduler 1.1.0',
  ]);
  const widgets = found[0];
  assert.equal(widgets.license, 'Apache-2.0');
  assert.equal(widgets.repository, 'https://example.com/widgets');
  assert.deepEqual(found.find((p) => p.name === 'react').texts, [{ name: 'LICENSE', text: MIT }]);

  manifest(app, { name: '@example/ui', dependencies: { missing: '1' } });
  assert.throws(() => npmPackages(app), /missing is not installed/);
});

test('the file lists every package once and each shared text once, with who carries it', () => {
  const p = (name, version, texts, license = 'MIT') => ({ name, version, license, repository: `https://example.com/${name}`, texts });
  const shared = { name: 'LICENSE', text: MIT };
  const text = render(
    'PitCrew 1.2.3: third-party notices',
    [p('serde', '1.0.0', [shared]), p('itoa', '1.0.0', [shared, { name: 'LICENSE-APACHE', text: 'apache\n' }]), p('serde', '1.0.0', [shared])],
    [p('react', '19.0.0', []), p('zustand', '5.0.0', [shared])],
  );
  assert.ok(text.startsWith('PitCrew 1.2.3: third-party notices\n'));
  assert.match(text, /Rust crates in the programs \(2\)/);
  assert.match(text, /JavaScript packages in the app's window \(2\)/);
  assert.equal(text.match(/^ {2}serde 1\.0\.0$/gm).length, 1);
  assert.match(text, / {2}react 19\.0\.0\n {4}licence: MIT\n {4}source: https:\/\/example\.com\/react\n {4}texts: none needed for a compiled copy/);
  assert.match(text, / {2}itoa 1\.0\.0\n(.*\n){2} {4}texts: 1, 2\n/);
  assert.equal(text.match(/^MIT License$/gm).length, 1);
  assert.match(text, /Text 1, carried by: itoa 1\.0\.0, serde 1\.0\.0, zustand 5\.0\.0\n/);
  assert.match(text, /Text 2, carried by: itoa 1\.0\.0\n-+\napache\n/);
  assert.ok(text.endsWith('\n') && !text.endsWith('\n\n'));
});

test('which licences need a notice with a binary', () => {
  for (const expression of ['MIT', 'Apache-2.0', 'BSD-3-Clause', 'MPL-2.0', 'OFL-1.1', 'MIT/Apache-2.0', 'not stated', '',
    '(MIT OR Apache-2.0) AND Unicode-3.0', 'Unicode-3.0 AND (0BSD OR MIT)', 'Apache-2.0 WITH Swift-exception', 'GPL-2.0+']) {
    assert.equal(needsNotice(expression), true, expression);
  }
  for (const expression of ['0BSD', 'Unlicense OR MIT', '0BSD OR MIT OR Apache-2.0', 'Zlib', 'BSL-1.0', 'CC0-1.0',
    'Apache-2.0 WITH LLVM-exception', 'Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT', '(MIT OR CC0-1.0)']) {
    assert.equal(needsNotice(expression), false, expression);
  }
});

test('a package without a text gets the checked-in one; one that needs a text and has none is named', (t) => {
  const dir = scratch(t);
  const extra = join(dir, 'extra');
  write(join(extra, 'bare', 'LICENSE'), MIT);
  write(join(extra, 'bare', 'SDK-LICENSE.txt'), 'sdk\r\n');
  assert.deepEqual(extraTexts('bare', extra).map((t) => t.name), ['notices-extra/bare/LICENSE', 'notices-extra/bare/SDK-LICENSE.txt']);
  assert.deepEqual(extraTexts('none', extra), []);
  const at = join(dir, 'crates', 'bare');
  write(join(at, 'Cargo.toml'), '');
  const metadata = {
    packages: [
      { id: 'app 1.0.0', name: 'app', version: '1.0.0', source: null, manifest_path: join(dir, 'Cargo.toml') },
      { id: 'bare 1.0.0', name: 'bare', version: '1.0.0', source: 'registry', license: 'MIT', manifest_path: join(at, 'Cargo.toml') },
    ],
    workspace_members: ['app 1.0.0'],
    resolve: { nodes: [{ id: 'app 1.0.0', deps: [{ pkg: 'bare 1.0.0', dep_kinds: [{ kind: null, target: null }] }] }] },
  };
  const [bare] = rustPackages(metadata, ['app'], extra);
  assert.deepEqual(bare.texts.map((t) => t.text), [MIT, 'sdk\n']);
  assert.deepEqual(missingTexts([bare]), []);
  // Without the checked-in text it fails; a licence that needs none does not.
  const [alone] = rustPackages(metadata, ['app'], join(dir, 'nowhere'));
  assert.deepEqual(missingTexts([alone]), ['bare 1.0.0 (MIT)']);
  assert.deepEqual(missingTexts([{ ...alone, license: 'MIT OR 0BSD' }]), []);
});

test('the build tools whose code is bundled are listed, without their dependencies', (t) => {
  const dir = scratch(t);
  const app = join(dir, 'app');
  const manifest = (at, fields) => write(join(at, 'package.json'), JSON.stringify(fields));
  manifest(app, { name: 'app', dependencies: {}, devDependencies: { vite: '8', tailwindcss: '4' } });
  const modules = join(app, 'node_modules');
  manifest(join(modules, 'tailwindcss'), { name: 'tailwindcss', version: '4.0.0', license: 'MIT', dependencies: { other: '1' } });
  manifest(join(modules, 'vite'), { name: 'vite', version: '8.0.0', license: 'MIT', dependencies: { rolldown: '1' } });
  manifest(join(modules, 'vite', 'node_modules', 'rolldown'), { name: 'rolldown', version: '1.0.0', license: 'MIT' });
  write(join(modules, 'vite', 'LICENSE.md'), MIT);
  const tools = [{ name: 'tailwindcss' }, { name: 'vite' }, { name: 'rolldown', from: 'vite' }];
  const found = dedupe(npmPackages(app, tools));
  assert.deepEqual(found.map((p) => `${p.name} ${p.version}`), ['rolldown 1.0.0', 'tailwindcss 4.0.0', 'vite 8.0.0']);
  assert.deepEqual(found[2].texts, [{ name: 'LICENSE.md', text: MIT }]);
  assert.throws(() => npmPackages(app, [{ name: 'missing' }]), /missing is not installed/);
});

test("the standard library's notices come from the toolchain, as text", (t) => {
  const dir = scratch(t);
  const doc = join(dir, 'share', 'doc', 'rust');
  write(join(doc, 'COPYRIGHT-library.html'),
    '<html><head><style>p{}</style></head><body><h1>Copyright notices</h1><p>A &amp; B &lt;C&gt;</p>\n\n\n<pre>MIT\n</pre><p>&#169; x&#x2014;y</p></body></html>');
  const std = rustStd(dir, 'rustc 1.97.0 (0123456 2026-09-01)');
  assert.equal(std.name, 'Rust standard library');
  assert.equal(std.version, '1.97.0');
  assert.equal(std.texts[0].text, 'Copyright notices\nA & B <C>\n\nMIT\n\n\u00a9 x\u2014y\n');
  assert.equal(htmlToText('<p>a</p>'), 'a\n');
  // A removal never leaves another match behind: nested and broken-up scripts and tags are gone.
  const hostile = '<scr<script>x</script>ipt>alert(1)</script><st<style>p{}</style>yle>q{}</style>' +
    '<<b>img src=x>ok<</b>/p> <scr<b>ipt>y</scr</b>ipt>';
  const cleaned = htmlToText(hostile);
  assert.equal(cleaned, 'ok y\n');
  assert.doesNotMatch(cleaned, /<|>/);
  // Text that only looks like a tag once decoded stays text: the result is never HTML.
  assert.equal(htmlToText('<p>&lt;script&gt; a &lt; b</p>'), '<script> a < b\n');
  assert.throws(() => rustStd(join(dir, 'nowhere'), 'rustc 1.0.0'), /COPYRIGHT-library\.html is missing/);
});
