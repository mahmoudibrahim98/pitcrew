import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { build, type Rolldown } from 'vite';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { DEFAULT_API, DEV_TOKEN, resolveApi, resolveToken } from '../src/data/config.ts';

describe('API config', () => {
  it('uses the dev token, or VITE_PITCREW_TOKEN, only in development', () => {
    expect(resolveToken({ DEV: true })).toBe(DEV_TOKEN);
    expect(resolveToken({ DEV: true, VITE_PITCREW_TOKEN: 'local-token' })).toBe('local-token');
    expect(resolveToken({ DEV: false })).toBeUndefined();
    expect(resolveToken({ DEV: false, VITE_PITCREW_TOKEN: 'local-token' })).toBeUndefined();
  });

  it('defaults the API to the mock hub', () => {
    expect(resolveApi({ DEV: false })).toBe(DEFAULT_API);
    expect(resolveApi({ DEV: true, VITE_PITCREW_API: 'http://127.0.0.1:4400' })).toBe('http://127.0.0.1:4400');
  });
});

describe('builds', () => {
  const root = fileURLToPath(new URL('..', import.meta.url));
  const saved = { token: process.env.VITE_PITCREW_TOKEN, nodeEnv: process.env.NODE_ENV };
  let envDir: string;

  beforeEach(() => {
    // An env folder of our own, so a developer's .env.local cannot change the outcome.
    envDir = mkdtempSync(join(tmpdir(), 'pitcrew-env-'));
    delete process.env.VITE_PITCREW_TOKEN;
    // Vitest sets NODE_ENV=test, which would make every build a development build.
    process.env.NODE_ENV = 'production';
  });

  afterEach(() => {
    rmSync(envDir, { recursive: true, force: true });
    if (saved.token === undefined) delete process.env.VITE_PITCREW_TOKEN;
    else process.env.VITE_PITCREW_TOKEN = saved.token;
    process.env.NODE_ENV = saved.nodeEnv;
  });

  const run = (mode: string) =>
    build({ root, mode, envDir, logLevel: 'silent', build: { write: false } });

  it.each(['production', 'staging', 'development'])(
    'refuses a %s build while VITE_PITCREW_TOKEN is set in an .env file',
    async (mode) => {
      writeFileSync(join(envDir, '.env.local'), 'VITE_PITCREW_TOKEN=local-token\n');
      await expect(run(mode)).rejects.toThrow(/VITE_PITCREW_TOKEN/);
    },
    60_000,
  );

  it('refuses a build while VITE_PITCREW_TOKEN is set in the shell', async () => {
    process.env.VITE_PITCREW_TOKEN = 'local-token';
    await expect(run('staging')).rejects.toThrow(/VITE_PITCREW_TOKEN/);
  }, 60_000);

  it('builds with no token, no other env values and no inline script', async () => {
    writeFileSync(join(envDir, '.env'), 'VITE_UNRELATED_SECRET=do-not-ship\n');
    const result = await run('production');
    const outputs = (Array.isArray(result) ? result : [result]) as Rolldown.RolldownOutput[];
    const files = outputs.flatMap((o) => o.output);
    for (const file of files) {
      if (file.type !== 'chunk') continue;
      expect(file.code).not.toContain('dev-device-token');
      expect(file.code).not.toContain('do-not-ship');
    }
    const html = files.find((f) => f.fileName === 'index.html');
    const source = html?.type === 'asset' ? String(html.source) : '';
    const scripts = [...source.matchAll(/<script\b[^>]*>([\s\S]*?)<\/script>/g)];
    expect(scripts.length).toBeGreaterThan(0);
    for (const [tag, body] of scripts) {
      expect(tag).toMatch(/\ssrc="/);
      expect(body?.trim()).toBe('');
    }
  }, 60_000);
});
