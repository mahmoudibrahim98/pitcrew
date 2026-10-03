import { fileURLToPath } from 'node:url';
import { defineConfig } from '@playwright/test';

// The Projects layout's recap acceptance: the app's dev server against its own mock hub, like
// apps/ui/playwright.config.ts (stream L's, whose `e2e/` folder this stream does not own) but on
// ports of its own (47482 and 5482), so it can run beside the other suites and a developer's
// servers. E2E_HUB_PORT and E2E_UI_PORT move them; PLAYWRIGHT_CHANNEL=msedge (or chrome) uses an
// installed browser.
//
// The browser runs in UTC: the app sends the viewer's own offset as `tz`, and the mock hub only has
// recap days for `tz=0` (apps/mock-hub/README.md), so this is how the suite passes `tz: 0`.
//
//   corepack pnpm --filter @pitcrew/ui exec playwright test -c src/projects/tests/e2e/playwright.config.ts
const UI_DIR = fileURLToPath(new URL('../../../../', import.meta.url));
const HUB_PORT = Number(process.env.E2E_HUB_PORT ?? 47482);
const UI_PORT = Number(process.env.E2E_UI_PORT ?? 5482);
const HUB_URL = `http://127.0.0.1:${HUB_PORT}`;
const channel = process.env.PLAYWRIGHT_CHANNEL;

export default defineConfig({
  testDir: '.',
  outputDir: `${UI_DIR}test-results/projects-recaps`,
  timeout: 30_000,
  expect: { timeout: 10_000 },
  retries: 0,
  workers: 1,
  fullyParallel: false,
  reporter: [['list']],
  use: {
    baseURL: `http://127.0.0.1:${UI_PORT}`,
    viewport: { width: 1280, height: 800 },
    timezoneId: 'UTC',
    trace: 'off',
    screenshot: 'off',
    video: 'off',
    ...(channel === undefined ? {} : { channel }),
  },
  webServer: [
    {
      command: 'node ../mock-hub/src/server.ts',
      cwd: UI_DIR,
      env: { PORT: String(HUB_PORT) },
      url: `${HUB_URL}/v1/host/info`,
      reuseExistingServer: false,
    },
    {
      command: `node node_modules/vite/bin/vite.js --port ${UI_PORT} --strictPort`,
      cwd: UI_DIR,
      env: { VITE_PITCREW_API: HUB_URL },
      url: `http://127.0.0.1:${UI_PORT}`,
      reuseExistingServer: false,
    },
  ],
});
