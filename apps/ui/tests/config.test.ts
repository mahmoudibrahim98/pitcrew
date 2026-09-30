import { fileURLToPath } from 'node:url';
import { build, type ConfigEnv, type Rolldown, type UserConfig } from 'vite';
import { afterEach, describe, expect, it } from 'vitest';
import { DEFAULT_API, DEV_TOKEN, resolveApi, resolveToken } from '../src/data/config.ts';
import viteConfig from '../vite.config.ts';

const configFor = viteConfig as (env: ConfigEnv) => UserConfig;

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

describe('vite config', () => {
  const saved = process.env.VITE_PITCREW_TOKEN;

  afterEach(() => {
    if (saved === undefined) delete process.env.VITE_PITCREW_TOKEN;
    else process.env.VITE_PITCREW_TOKEN = saved;
  });

  it('refuses a production build while VITE_PITCREW_TOKEN is set', () => {
    process.env.VITE_PITCREW_TOKEN = 'local-token';
    expect(() => configFor({ mode: 'production', command: 'build' })).toThrow(/VITE_PITCREW_TOKEN/);
    // Development may use it.
    expect(() => configFor({ mode: 'development', command: 'serve' })).not.toThrow();
  });

  it('builds for production without it', () => {
    delete process.env.VITE_PITCREW_TOKEN;
    expect(configFor({ mode: 'production', command: 'build' }).build?.modulePreload).toEqual({ polyfill: false });
  });
});

describe('production build', () => {
  it('carries no token and no inline script', async () => {
    delete process.env.VITE_PITCREW_TOKEN;
    const root = fileURLToPath(new URL('..', import.meta.url));
    // Vitest sets NODE_ENV=test, which would make this a development build.
    const nodeEnv = process.env.NODE_ENV;
    process.env.NODE_ENV = 'production';
    let result: Awaited<ReturnType<typeof build>>;
    try {
      result = await build({ root, mode: 'production', logLevel: 'silent', build: { write: false } });
    } finally {
      process.env.NODE_ENV = nodeEnv;
    }
    const outputs = (Array.isArray(result) ? result : [result]) as Rolldown.RolldownOutput[];
    const files = outputs.flatMap((o) => o.output);
    const text = (name: string) => {
      const file = files.find((f) => f.fileName === name);
      if (file === undefined) throw new Error(`no ${name}`);
      return file.type === 'chunk' ? file.code : String(file.source);
    };
    for (const chunk of files.filter((f) => f.type === 'chunk')) {
      expect(chunk.type === 'chunk' && chunk.code.includes('dev-device-token')).toBe(false);
    }
    const scripts = [...text('index.html').matchAll(/<script\b[^>]*>([\s\S]*?)<\/script>/g)];
    expect(scripts.length).toBeGreaterThan(0);
    for (const [tag, body] of scripts) {
      expect(tag).toMatch(/\ssrc="/);
      expect(body?.trim()).toBe('');
    }
  }, 60_000);
});