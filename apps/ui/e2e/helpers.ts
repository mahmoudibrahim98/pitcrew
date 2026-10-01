// Shared between playwright.config.ts and the specs in this folder: which hub the run talks to,
// and the bearer token for a direct call to it (not through the UI, which gets its own copy via
// VITE_PITCREW_TOKEN — see playwright.config.ts).
//
// By default the suite talks to the mock hub playwright.config.ts starts for it. Set
// `E2E_HUB_URL` to point the whole suite at an already-running hub instead (a real `pitcrewd`,
// typically) — then playwright.config.ts does not start the mock hub — and `E2E_HUB_TOKEN` to
// that hub's device token, since the mock hub's fixed `dev-device-token` will not be accepted by
// anything else. See README.md, "Running against a real pitcrewd".

/** Set when the suite should talk to a hub it did not start itself. */
export const EXTERNAL_HUB_URL = process.env.E2E_HUB_URL;

/** The mock hub's port, when playwright.config.ts is the one starting it. Moves it so several
 * checkouts can run the suite at once. Unused once `E2E_HUB_URL` is set. */
export const MOCK_HUB_PORT = Number(process.env.E2E_HUB_PORT ?? 47399);

/** The hub this run talks to: `E2E_HUB_URL`, or the mock hub on `MOCK_HUB_PORT`. */
export const HUB_URL = EXTERNAL_HUB_URL ?? `http://127.0.0.1:${MOCK_HUB_PORT}`;

/** The bearer token for a direct call to the hub. Defaults to the mock hub's fixed device token;
 * set `E2E_HUB_TOKEN` to a real daemon's token (`pitcrewd token show-path` names the file that
 * holds it) when pointing the suite at one. */
export const HUB_TOKEN = process.env.E2E_HUB_TOKEN ?? 'dev-device-token';

/** `Authorization: Bearer <token>` header for a direct `request`/`fetch` call to the hub. */
export const HUB_AUTH = { Authorization: `Bearer ${HUB_TOKEN}` } as const;
