import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { defineConfig } from '@playwright/test';
import { DESKTOP_HUB, DESKTOP_HUB_PORT, FIRST_RUN_HUB, FIRST_RUN_HUB_PORT, MOCK_DEVICE_TOKEN, UI_PORT } from './fresh-hubs';

// The first run, against fresh mock hubs (`PITCREW_MOCK_FRESH=1`: no person yet, set up once only).
// Its specs are `*.fresh.ts`, which the demo-mode suite (`playwright.config.ts`, the same `e2e`
// folder) does not match, since they need hubs of their own:
//   corepack pnpm --filter @pitcrew/ui exec playwright test --config e2e/fresh.config.ts
// Every path is absolute, from this file's own place, so it does not depend on the runner's cwd.
// PLAYWRIGHT_CHANNEL=msedge (or chrome) uses an installed browser.

const here = path.dirname(fileURLToPath(import.meta.url));
const uiRoot = path.resolve(here, '..');
/** Forward slashes, and quoted: safe in a shell command on any OS. */
const q = (p: string) => `"${p.split(path.sep).join('/')}"`;
const hub = q(path.resolve(uiRoot, '../mock-hub/src/server.ts'));
const channel = process.env.PLAYWRIGHT_CHANNEL;

export default defineConfig({
  testDir: here,
  testMatch: '*.fresh.ts',
  // Each walk runs several axe scans, in two themes.
  timeout: 90_000,
  expect: { timeout: 10_000 },
  retries: 0,
  // Each hub is set up once: the specs run one at a time, in order.
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
      command: `node ${hub}`,
      env: { PORT: String(FIRST_RUN_HUB_PORT), PITCREW_MOCK_FRESH: '1' },
      url: `${FIRST_RUN_HUB}/v1/host/info`,
      reuseExistingServer: false,
    },
    {
      command: `node ${hub}`,
      env: { PORT: String(DESKTOP_HUB_PORT), PITCREW_MOCK_FRESH: '1' },
      url: `${DESKTOP_HUB}/v1/host/info`,
      reuseExistingServer: false,
    },
    {
      command: `node ${q(path.join(uiRoot, 'node_modules/vite/bin/vite.js'))} ${q(uiRoot)} --port ${UI_PORT}`,
      env: { VITE_PITCREW_API: FIRST_RUN_HUB, VITE_PITCREW_TOKEN: MOCK_DEVICE_TOKEN },
      url: `http://127.0.0.1:${UI_PORT}`,
      reuseExistingServer: false,
    },
  ],
});
