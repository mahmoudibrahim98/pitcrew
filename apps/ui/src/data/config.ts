// Where the API is. In the browser during development the UI talks to the mock hub with the dev
// device token. A production build never carries a token: the desktop app's gateway adds it
// (ADR-0003), and `vite.config.ts` refuses to build while VITE_PITCREW_TOKEN is set.

export const DEFAULT_API = 'http://127.0.0.1:47317';
export const DEV_TOKEN = 'dev-device-token';

export interface Env {
  DEV: boolean;
  VITE_PITCREW_API?: string | undefined;
  VITE_PITCREW_TOKEN?: string | undefined;
}

export function resolveApi(env: Env): string {
  return env.VITE_PITCREW_API || DEFAULT_API;
}

/** The bearer token, in development only. */
export function resolveToken(env: Env): string | undefined {
  return env.DEV ? env.VITE_PITCREW_TOKEN || DEV_TOKEN : undefined;
}

// Only the named keys: a bare `import.meta.env` would inline every VITE_* variable into the
// bundle. The literal `import.meta.env.DEV` checks let the bundler drop the token from builds.
const env: Env = {
  DEV: import.meta.env.DEV,
  VITE_PITCREW_API: import.meta.env.VITE_PITCREW_API,
  VITE_PITCREW_TOKEN: import.meta.env.DEV ? import.meta.env.VITE_PITCREW_TOKEN : undefined,
};

export const apiBaseUrl: string = resolveApi(env);
export const apiToken: string | undefined = import.meta.env.DEV ? resolveToken(env) : undefined;
