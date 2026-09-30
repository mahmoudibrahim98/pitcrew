import { defineConfig } from '@playwright/test';

// Runs the UI's dev server against its own mock hub, on ports that do not clash with a developer's
// `npm run mock-hub` and `pnpm dev`. PLAYWRIGHT_CHANNEL=msedge (or chrome) uses an installed browser
// instead of Playwright's download.
const HUB_PORT = 47399;
const UI_PORT = 5199;
const channel = process.env.PLAYWRIGHT_CHANNEL;

export default defineConfig({
  testDir: 'e2e',
  timeout: 30_000,
  retries: 0,
  reporter: [['list']],
  use: {
    baseURL: `http://127.0.0.1:${UI_PORT}`,
    trace: 'retain-on-failure',
    ...(channel === undefined ? {} : { channel }),
  },
  webServer: [
    {
      command: 'node ../mock-hub/src/server.ts',
      env: { PORT: String(HUB_PORT) },
      url: `http://127.0.0.1:${HUB_PORT}/v1/host/info`,
      reuseExistingServer: false,
    },
    {
      command: `node node_modules/vite/bin/vite.js --port ${UI_PORT}`,
      env: { VITE_PITCREW_API: `http://127.0.0.1:${HUB_PORT}` },
      url: `http://127.0.0.1:${UI_PORT}`,
      reuseExistingServer: false,
    },
  ],
});
