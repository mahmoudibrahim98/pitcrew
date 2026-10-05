// Creates the public Tauri configuration overlay; signing material never enters this file.
import { readFileSync, writeFileSync } from 'node:fs';
const [file] = process.argv.slice(2);
const config = JSON.parse(readFileSync(file, 'utf8'));
const signed = Boolean(process.env.TAURI_SIGNING_PRIVATE_KEY);
const pubkey = process.env.TAURI_UPDATER_PUBLIC_KEY ?? '';
if (signed && !pubkey) throw new Error('Set TAURI_UPDATER_PUBLIC_KEY before signing updates');
if (!signed && process.env.TAURI_SIGNING_PRIVATE_KEY_PASSWORD) throw new Error('Updater password configured without a private key');
const version = process.env.RELEASE_VERSION || JSON.parse(readFileSync('apps/desktop/src-tauri/tauri.conf.json', 'utf8')).version;
if (!/^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$/.test(version)) throw new Error('Invalid release version');
config.version = version;
config.bundle.createUpdaterArtifacts = signed;
config.plugins = { updater: { pubkey: signed ? pubkey : '', requireSignedVersion: true } };
writeFileSync(file, JSON.stringify(config, null, 2) + '\n');
if (!signed) console.log('::notice::Updater key absent: updates are unsigned and disabled');
