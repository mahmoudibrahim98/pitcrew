// Tests for notices.mjs on synthetic packages: which crates and JavaScript packages count, their
// licence files, and the rendered file. Run: node --test packaging/notices.test.mjs
import assert from 'node:assert/strict';
import { mkdirSync, mkdtempSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { cleanText, dedupe, licenceTexts, npmPackages, render, rustPackages } from './notices.mjs';

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
  assert.match(text, / {2}react 19\.0\.0\n {4}licence: MIT\n {4}source: https:\/\/example\.com\/react\n {4}texts: none in the package; see its source/);
  assert.match(text, / {2}itoa 1\.0\.0\n(.*\n){2} {4}texts: 1, 2\n/);
  assert.equal(text.match(/^MIT License$/gm).length, 1);
  assert.match(text, /Text 1, carried by: itoa 1\.0\.0, serde 1\.0\.0, zustand 5\.0\.0\n/);
  assert.match(text, /Text 2, carried by: itoa 1\.0\.0\n-+\napache\n/);
  assert.ok(text.endsWith('\n') && !text.endsWith('\n\n'));
});
