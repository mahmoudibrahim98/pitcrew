import { defineConfig } from '@playwright/test';

// Runs the UI's dev server against its own mock hub, on ports that do not clash with a developer's
// `npm run mock-hub` and `pnpm dev`. E2E_HUB_PORT and E2E_UI_PORT move them (several checkouts
// testing at once). PLAYWRIGHT_CHANNEL=msedge (or chrome) uses an installed browser instead of
// Playwright's download. The specs share one hub, so they run one at a time.
const HUB_PORT = Number(process.env.E2E_HUB_PORT ?? 47399);
const UI_PORT = Number(process.env.E2E_UI_PORT ?? 5199);
const HUB_URL = `http://127.0.0.1:${HUB_PORT}`;
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
    {
      command: 'node ../mock-hub/src/server.ts',
      env: { PORT: String(HUB_PORT) },
      url: `${HUB_URL}/v1/host/info`,
      reuseExistingServer: false,
    },
    {
      command: `node node_modules/vite/bin/vite.js --port ${UI_PORT}`,
      env: { VITE_PITCREW_API: HUB_URL },
      url: `http://127.0.0.1:${UI_PORT}`,
      reuseExistingServer: false,
    },
  ],
});
