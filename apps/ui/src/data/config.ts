// Where the API is. In the browser during development the UI talks to the mock hub with the dev
// device token; the desktop app's gateway adds the real token, so production sets none.

export const DEFAULT_API = 'http://127.0.0.1:47317';
export const DEV_TOKEN = 'dev-device-token';

export const apiBaseUrl: string = import.meta.env.VITE_PITCREW_API || DEFAULT_API;

export const apiToken: string | undefined =
  import.meta.env.VITE_PITCREW_TOKEN || (import.meta.env.DEV ? DEV_TOKEN : undefined);
