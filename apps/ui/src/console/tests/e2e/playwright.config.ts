import { fileURLToPath } from 'node:url';
import { defineConfig } from '@playwright/test';

// The Agent console's acceptance run: the app's dev server against its own mock hub, like
// apps/ui/playwright.config.ts but on the console's ports (47450 and 47451), so it can run beside
// the shell's specs and a developer's servers. E2E_HUB_PORT and E2E_UI_PORT move them;
// PLAYWRIGHT_CHANNEL=msedge (or chrome) uses an installed browser. The specs share one hub and
// change it (they send prompts and answer questions), so they run one at a time, in order.
//
//   pnpm exec playwright test -c src/console/tests/e2e/playwright.config.ts
const UI_DIR = fileURLToPath(new URL('../../../../', import.meta.url));
const HUB_PORT = Number(process.env.E2E_HUB_PORT ?? 47450);
const UI_PORT = Number(process.env.E2E_UI_PORT ?? 47451);
const HUB_URL = `http://127.0.0.1:${HUB_PORT}`;
const channel = process.env.PLAYWRIGHT_CHANNEL;

export default defineConfig({
  testDir: '.',
  outputDir: `${UI_DIR}test-results/console`,
  timeout: 30_000,
  expect: { timeout: 10_000 },
  retries: 0,
  workers: 1,
  fullyParallel: false,
  reporter: [['list']],
  use: {
    baseURL: `http://127.0.0.1:${UI_PORT}`,
    viewport: { width: 1280, height: 800 },
    trace: 'retain-on-failure',
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
