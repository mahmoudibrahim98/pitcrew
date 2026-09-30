import assert from 'node:assert/strict';
import test from 'node:test';
import { globToRegExp, matches, normalizePath } from '../lib/glob.mjs';

const cases = [
  // ** at the end: one or more segments below
  ['a/**', 'a/x', true],
  ['a/**', 'a/x/y', true],
  ['a/**', 'a', false],
  ['a/**', 'ab/x', false],
  ['crates/api/**', 'crates/api/src/deep/mod.rs', true],
  ['crates/api/**', 'crates/api-extra/src/lib.rs', false],
  // ** at the start: zero or more segments above
  ['**/Cargo.toml', 'Cargo.toml', true],
  ['**/Cargo.toml', 'x/Cargo.toml', true],
  ['**/Cargo.toml', 'x/y/Cargo.toml', true],
  ['**/Cargo.toml', 'xCargo.toml', false],
  ['**/Cargo.toml', 'x/Cargo.toml.bak', false],
  // ** in the middle: zero or more segments between
  ['a/**/b', 'a/b', true],
  ['a/**/b', 'a/x/b', true],
  ['a/**/b', 'a/x/y/b', true],
  ['a/**/b', 'a/xb', false],
  ['a/**/b', 'ab', false],
  ['a/**/b', 'a/b/c', false],
  ['**', 'any/path/at/all.txt', true],
  // * stays inside one segment
  ['*', 'top.txt', true],
  ['*', 'dir/file', false],
  ['*.md', 'README.md', true],
  ['*.md', 'docs/README.md', false],
  ['apps/ui/*', 'apps/ui/package.json', true],
  ['apps/ui/*', 'apps/ui/src/main.tsx', false],
  ['src/*.rs', 'src/lib.rs', true],
  ['src/*.rs', 'src/a/lib.rs', false],
  ['crates/store/migrations/00*', 'crates/store/migrations/0001_init.sql', true],
  ['crates/store/migrations/00*', 'crates/store/migrations/0105_x.sql', false],
  ['crates/store/migrations/00*', 'crates/store/migrations/00/x.sql', false],
  // ? is exactly one non-slash character
  ['file?.txt', 'file1.txt', true],
  ['file?.txt', 'file.txt', false],
  ['file?.txt', 'file12.txt', false],
  ['a?b', 'a/b', false],
  // dots are literal
  ['Cargo.toml', 'Cargo.toml', true],
  ['Cargo.toml', 'Cargoxtoml', false],
  ['.github/workflows/*.yml', '.github/workflows/ci.yml', true],
  ['.github/workflows/*.yml', 'xgithub/workflows/ci.yml', false],
  ['.github/workflows/*.yml', '.github/workflows/ci.yaml', false],
  ['.github/workflows/release*.yml', '.github/workflows/release.yml', true],
  ['.github/workflows/release*.yml', '.github/workflows/release-nightly.yml', true],
  ['.github/workflows/release*.yml', '.github/workflows/ci.yml', false],
  // regex special characters are literal
  ['a+b(c)[d]{e}$^|.txt', 'a+b(c)[d]{e}$^|.txt', true],
  ['a+b.txt', 'aab.txt', false],
  ['a[bc].txt', 'ab.txt', false],
  ['(x)', 'x', false],
  ['a\\b', 'a/b', true], // backslash in a glob is a separator, like in a path
  ['^x$', '^x$', true],
  // a ** that is not a whole segment acts like *
  ['a/**.rs', 'a/x.rs', true],
  ['a/**.rs', 'a/b/x.rs', false],
  // branch names use the same matcher
  ['dependabot/**', 'dependabot/cargo/serde-1.0.200', true],
  ['dependabot/**', 'dependabot', false],
];

test('glob matching table', () => {
  for (const [glob, path, expected] of cases) {
    assert.equal(matches(glob, path), expected, `matches(${JSON.stringify(glob)}, ${JSON.stringify(path)})`);
  }
});

test('paths with backslashes or ./ are normalised before matching', () => {
  assert.equal(matches('crates/store/src/**', 'crates\\store\\src\\lib.rs'), true);
  assert.equal(matches('**/x', './x'), true);
  assert.equal(matches('a/*', 'a//b'), true);
  assert.equal(normalizePath('.\\a\\\\b/c'), 'a/b/c');
});

test('globToRegExp is anchored at both ends', () => {
  const re = globToRegExp('src/*.rs');
  assert.ok(re instanceof RegExp);
  assert.equal(re.test('src/lib.rs'), true);
  assert.equal(re.test('x/src/lib.rs'), false);
  assert.equal(re.test('src/lib.rs.orig'), false);
});
