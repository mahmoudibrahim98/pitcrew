import { defineConfig } from '@playwright/test';
import { EXTERNAL_HUB_URL, HUB_TOKEN, HUB_URL, MOCK_HUB_PORT } from './e2e/helpers';

// Runs the UI's dev server against a hub: by default its own mock hub, on ports that do not clash
// with a developer's `npm run mock-hub` and `pnpm dev`. Set E2E_HUB_URL to point the suite at an
// already-running hub instead (a real `pitcrewd` — see README.md, "Running against a real
// pitcrewd"); this config then does not start the mock hub, only the UI's dev server against that
// URL. E2E_HUB_TOKEN carries that hub's device token to both the UI (as VITE_PITCREW_TOKEN) and
// the specs' own direct calls (see e2e/helpers.ts). E2E_HUB_PORT and E2E_UI_PORT move the mock
// hub's and the UI's ports (several checkouts testing at once; E2E_HUB_PORT is unused once
// E2E_HUB_URL is set). PLAYWRIGHT_CHANNEL=msedge (or chrome) uses an installed browser instead of
// Playwright's download. The specs share one hub, so they run one at a time.
const UI_PORT = Number(process.env.E2E_UI_PORT ?? 5199);
const channel = process.env.PLAYWRIGHT_CHANNEL;

export default defineConfig({
  testDir: 'e2e',
  timeout: 30_000,
  retries: 0,
  workers: 1,
  reporter: [['list']],
  use: {
    baseURL: `http://127.0.0.1:${UI_PORT}`,
    viewport: { width: 1280, height: 800 },
    trace: 'retain-on-failure',
    ...(channel === undefined ? {} : { channel }),
  },
  webServer: [
    ...(EXTERNAL_HUB_URL === undefined
      ? [
          {
            command: 'node ../mock-hub/src/server.ts',
            env: { PORT: String(MOCK_HUB_PORT) },
            url: `${HUB_URL}/v1/host/info`,
            reuseExistingServer: false,
          },
        ]
      : []),
    {
      command: `node node_modules/vite/bin/vite.js --port ${UI_PORT}`,
      env: { VITE_PITCREW_API: HUB_URL, VITE_PITCREW_TOKEN: HUB_TOKEN },
      url: `http://127.0.0.1:${UI_PORT}`,
      reuseExistingServer: false,
    },
  ],
});
