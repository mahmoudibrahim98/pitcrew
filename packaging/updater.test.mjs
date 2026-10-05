import assert from 'node:assert/strict';
import { test } from 'node:test';
import { mkdtempSync, writeFileSync, rmSync, readFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { spawnSync } from 'node:child_process';
import { manifest } from './updater-feed.mjs';
test('feed requires all signed platform artifacts and URL-encodes filenames', () => {
  const dir = mkdtempSync(join(tmpdir(), 'pitcrew-updater-'));
  try {
    for (const name of ['PitCrew.AppImage', 'PitCrew.app.tar.gz', 'PitCrew setup-setup.exe']) {
      writeFileSync(join(dir, name), 'synthetic artifact');
      writeFileSync(join(dir, `${name}.sig`), 'synthetic-signature');
    }
    const feed = manifest(dir, '1.2.3-beta.1', 'example/pitcrew');
    assert.equal(feed.version, '1.2.3-beta.1');
    assert.equal(Object.keys(feed.platforms).length, 4);
    assert.equal(feed.platforms['darwin-aarch64'].url, feed.platforms['darwin-x86_64'].url);
    assert.match(feed.platforms['windows-x86_64'].url, /PitCrew%20setup-setup.exe$/);
    rmSync(join(dir, 'PitCrew.AppImage.sig'));
    assert.throws(() => manifest(dir, '1.2.3', 'example/pitcrew'));
  } finally { rmSync(dir, { recursive: true }); }
});
test('unsigned builds disable updates; partial signing configuration fails', () => {
  const dir = mkdtempSync(join(tmpdir(), 'pitcrew-updater-'));
  const file = join(dir, 'config.json');
  const env = { ...process.env, TAURI_SIGNING_PRIVATE_KEY: '', TAURI_SIGNING_PRIVATE_KEY_PASSWORD: '', TAURI_UPDATER_PUBLIC_KEY: '', RELEASE_VERSION: '1.2.3' };
  try {
    writeFileSync(file, JSON.stringify({ bundle: {} }));
    const run = spawnSync(process.execPath, ['packaging/updater-config.mjs', file], { env });
    assert.equal(run.status, 0, run.stderr.toString());
    const config = JSON.parse(readFileSync(file));
    assert.equal(config.bundle.createUpdaterArtifacts, false);
    assert.equal(config.plugins.updater.requireSignedVersion, true);
    assert.equal(config.plugins.updater.pubkey, '');
    const partial = spawnSync(process.execPath, ['packaging/updater-config.mjs', file], { env: { ...env, TAURI_SIGNING_PRIVATE_KEY: 'synthetic-key' } });
    assert.notEqual(partial.status, 0);
    const signed = spawnSync(process.execPath, ['packaging/updater-config.mjs', file], { env: { ...env, TAURI_SIGNING_PRIVATE_KEY: 'synthetic-key', TAURI_UPDATER_PUBLIC_KEY: 'synthetic-public-key' } });
    assert.equal(signed.status, 0);
    assert.equal(JSON.parse(readFileSync(file)).bundle.createUpdaterArtifacts, true);
  } finally { rmSync(dir, { recursive: true }); }
});
