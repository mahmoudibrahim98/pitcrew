import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import { checkHtml, checkPolicy } from '../csp-check.mjs';

const config = (csp) => ({ app: { security: { csp } } });
const policy = { 'default-src': "'self'", 'script-src': "'self'", 'connect-src': 'ipc: http://ipc.localhost' };

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
  assert.deepEqual(checkHtml('<textarea><img onerror="not markup here"></textarea><style>/* onclick */</style>'), []);
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
