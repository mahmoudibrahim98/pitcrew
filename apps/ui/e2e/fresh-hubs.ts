// Shared between fresh.config.ts and the `*.fresh.ts` specs: the fresh mock hubs (each set up once
// only, so each spec that sets one up has its own) and the UI's dev server.
//
// - FIRST_RUN_HUB: the hub the UI's dev server talks to in a browser (`VITE_PITCREW_API`): the
//   first run from an empty workspace to Home (`first-run.fresh.ts`).
// - DESKTOP_HUB: the hub behind a simulated desktop gateway (`fake-desktop.ts`), as the remote
//   machine the connect wizard adds and then sets up (`connect.fresh.ts`).
//
// E2E_FRESH_HUB_PORT, E2E_FRESH_DESKTOP_HUB_PORT and E2E_FRESH_UI_PORT move them.

export const FIRST_RUN_HUB_PORT = Number(process.env.E2E_FRESH_HUB_PORT ?? 47433);
export const DESKTOP_HUB_PORT = Number(process.env.E2E_FRESH_DESKTOP_HUB_PORT ?? 47434);
export const UI_PORT = Number(process.env.E2E_FRESH_UI_PORT ?? 5433);

export const FIRST_RUN_HUB = `http://127.0.0.1:${FIRST_RUN_HUB_PORT}`;
export const DESKTOP_HUB = `http://127.0.0.1:${DESKTOP_HUB_PORT}`;

/** The mock hub's fixed device token: public on purpose, for the mock only. */
export const MOCK_DEVICE_TOKEN = 'dev-device-token';
