// Builds a Tauri static manifest from signed final artifacts, before SHA256SUMS and attestations.
import { readdirSync, readFileSync, writeFileSync } from 'node:fs';
import { pathToFileURL } from 'node:url';
import { join } from 'node:path';
export function manifest(dir, version, repository) {
  if (!/^[\w-]+\/[\w.-]+$/.test(repository)) throw new Error('Invalid repository');
  if (!/^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$/.test(version)) throw new Error('Invalid release version');
  const platforms = {};
  for (const name of readdirSync(dir).sort()) {
    let targets;
    if (name.endsWith('.AppImage')) targets = ['linux-x86_64'];
    else if (name.endsWith('.app.tar.gz')) targets = ['darwin-x86_64', 'darwin-aarch64'];
    else if (name.endsWith('-setup.exe')) targets = ['windows-x86_64'];
    else continue;
    const signature = readFileSync(join(dir, `${name}.sig`), 'utf8').trim();
    if (!signature) throw new Error('Empty update signature');
    for (const target of targets) {
      if (platforms[target]) throw new Error(`Duplicate update artifact for ${target}`);
      platforms[target] = { signature, url: `https://github.com/${repository}/releases/download/v${version}/${encodeURIComponent(name)}` };
    }
  }
  for (const target of ['linux-x86_64', 'darwin-x86_64', 'darwin-aarch64', 'windows-x86_64']) {
    if (!platforms[target]) throw new Error(`Missing signed artifact for ${target}`);
  }
  return { version, notes: `Release notes: https://github.com/${repository}/releases/tag/v${version}`, platforms };
}
if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const [dir, version, repository] = process.argv.slice(2);
  const files = readdirSync(dir);
  if (!files.some((name) => name.endsWith('.sig') && !name.startsWith('SHA256SUMS'))) {
    console.log('::notice::No signed updater artifacts: latest.json omitted; updates disabled');
  } else {
    writeFileSync(join(dir, 'latest.json'), JSON.stringify(manifest(dir, version, repository), null, 2) + '\n');
  }
}
