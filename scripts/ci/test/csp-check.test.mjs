import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import { checkHtml, checkPolicy } from '../csp-check.mjs';

const config = (csp) => ({ app: { security: { csp } } });
const policy = { 'default-src': "'self'", 'script-src': "'self'", 'connect-src': 'ipc: http://ipc.localhost' };

test('images allow local object URLs but reject data and remote sources', () => {
  assert.deepEqual(checkPolicy(config({ ...policy, 'img-src': "'self' blob:" })), []);
  for (const source of ['data:', 'https:', 'https://example.com', '*']) {
    assert.ok(checkPolicy(config({ ...policy, 'img-src': source })).some((error) => error.startsWith('img-src')));
  }
});

test('the actual desktop policy allows only local IPC and no executable inline sources', () => {
  const actual = JSON.parse(readFileSync(new URL('../../../apps/desktop/src-tauri/tauri.conf.json', import.meta.url), 'utf8'));
  assert.deepEqual(checkPolicy(actual), []);
  assert.deepEqual(checkPolicy(config(policy)), []);
  assert.deepEqual(checkPolicy(config("default-src 'self'; script-src 'self'; connect-src ipc: http://ipc.localhost")), []);
});

for (const directive of ['default-src', 'script-src', 'script-src-elem', 'script-src-attr', 'connect-src']) {
  for (const source of ['https://example.com', 'https:', '*.example.com', '*', '//example.com', 'data:', 'blob:']) {
    test(`${directive} rejects remote or unrestricted source ${source}`, () => {
      assert.ok(checkPolicy(config({ ...policy, [directive]: source })).some((error) => error.startsWith(directive)));
    });
  }
}

for (const source of ["'unsafe-inline'", "'unsafe-eval'", "'strict-dynamic'", "'unsafe-hashes'"]) {
  for (const directive of ['default-src', 'script-src', 'script-src-elem', 'script-src-attr']) {
    test(`${directive} rejects ${source}, including script fallbacks`, () => {
      assert.ok(checkPolicy(config({ ...policy, [directive]: source })).length);
    });
  }
}

test('local IPC exceptions do not extend to arbitrary localhost names, credentials or ports', () => {
  for (const source of ['http://ipc.localhost:1234', 'http://ipc.localhost.example.com', 'http://localhost', 'ipc://example.com']) {
    assert.ok(checkPolicy(config({ ...policy, 'connect-src': source })).length);
  }
});

test('missing, disabled, malformed and duplicate policies fail closed', () => {
  for (const csp of [undefined, null, false, [], {}, { 'default-src': [] }, { 'default-src': {} }]) {
    assert.ok(checkPolicy(config(csp)).length);
  }
  assert.ok(checkPolicy(config("default-src 'self'; script-src 'self'; SCRIPT-SRC 'unsafe-eval'"))
    .some((error) => error.includes('duplicate')));
});

test('script-src falls back to default-src; exact hashes and nonces remain local policies', () => {
  assert.deepEqual(checkPolicy(config({ 'default-src': "'self'" })), []);
  assert.ok(checkPolicy(config({ 'default-src': 'https://example.com' })).length);
  assert.deepEqual(checkPolicy(config({ ...policy, 'script-src': "'self' 'nonce-YWJjZA==' 'sha256-YWJjZA=='" })), []);
});

test('external module entrypoint and quoted attributes are not inline executable markup', () => {
  assert.deepEqual(checkHtml('<!doctype html><title>Example &lt;script&gt;</title><script type="module" src="/assets/app.js"></script>'), []);
  assert.deepEqual(checkHtml('<!-- <script>bad()</script> --><p title="onclick=not-an-attribute >">Text</p>'), []);
  assert.deepEqual(checkHtml('<textarea><img onerror="not markup here"></textarea>'), []);
  // A <style> is refused for itself (below), but its text is still not read as markup.
  assert.deepEqual(checkHtml('<style>/* <img onclick="x"> */</style>'), ['inline <style> in built UI']);
});

// Tauri adds a nonce to every <style> in the page, which switches off 'unsafe-inline' for the
// styles the app sets at run time; style="" is never covered by a nonce at all.
for (const html of ['<style>html { background: #fff }</style>', '<STYLE media="(prefers-color-scheme: dark)"></STYLE>', '<head><style></style></head>']) {
  test(`rejects inline <style>: ${html}`, () => assert.ok(checkHtml(html).includes('inline <style> in built UI')));
}
for (const html of ['<div style="display:grid">', "<p STYLE='color: red'>", '<span style>', '<div class="x" style=display:grid>']) {
  test(`rejects style attributes: ${html}`, () => assert.ok(checkHtml(html).some((error) => error.startsWith('style attribute'))));
}
test('style-like names and text are not inline styles', () => {
  assert.deepEqual(checkHtml('<div class="style" data-style="x" title="style=&quot;a&quot;">style=""</div><link rel="stylesheet" href="/splash.css">'), []);
});
test("the UI's entry page has no inline script, style or handler", () => {
  const html = readFileSync(new URL('../../../apps/ui/index.html', import.meta.url), 'utf8');
  assert.deepEqual(checkHtml(html), []);
});

for (const html of ['<script>bad()</script>', '<SCRIPT type="module">bad()</SCRIPT>', '<script src="">bad()</script>', '<script type="application/json">{}</script>']) {
  test(`rejects inline script: ${html}`, () => assert.ok(checkHtml(html).some((error) => error.includes('inline'))));
}
for (const html of ['<body onload="bad()">', "<button ONCLICK = 'bad()'>", '<svg/onload=bad()>', '<img onerror=bad()>']) {
  test(`rejects event-handler attributes: ${html}`, () => assert.ok(checkHtml(html).some((error) => error.includes('event-handler'))));
}
test('malformed tags and ambiguous duplicate attributes fail closed', () => {
  for (const html of ['<script src="unfinished', '<script src="/x.js">', '<!-- unfinished', '<script src="/x.js" src="/y.js"></script>']) {
    assert.ok(checkHtml(html).length);
  }
});
