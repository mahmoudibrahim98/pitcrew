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

export const apiBaseUrl: string = resolveApi(import.meta.env);
// The literal `import.meta.env.DEV` check lets the bundler drop the dev token from production code.
export const apiToken: string | undefined = import.meta.env.DEV
  ? resolveToken(import.meta.env)
  : undefined;
