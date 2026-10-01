import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { defineConfig } from '@playwright/test';

// A scoped Playwright config for this stream's own e2e spec, since `apps/ui/playwright.config.ts`
// (owned by stream L) has `testDir: 'e2e'` and `apps/ui/e2e/**` is L's path too — see README.md,
// "Running this stream's tests". Same shape and ports as the root config; every path is absolute
// (computed from this file's own location) so it does not depend on the runner's cwd or on
// Playwright's webServer `cwd` support.
//
// This file (and its sibling spec) sit under `src/`, so `tsc -b` checks them against
// `tsconfig.app.json` (browser-only `types`), not `tsconfig.node.json` (which has Node's types but
// only looks in `e2e/`, `tests/` and `src/**/tests/**` — not `src/onboarding/e2e/`, and neither
// config is this stream's to edit). The reference directive above pulls in `@types/node` for this
// file alone, regardless of `tsconfig.app.json`'s `types` array.
// Run with:
//   corepack pnpm --filter @pitcrew/ui exec playwright test --config src/onboarding/e2e/playwright.config.ts
const here = path.dirname(fileURLToPath(import.meta.url));
const uiRoot = path.resolve(here, '../../../');
const monorepoApps = path.resolve(uiRoot, '..');
/** Forward slashes everywhere, and quoted: safe to paste into a shell command on any OS. */
const q = (p: string) => `"${p.split(path.sep).join('/')}"`;

const HUB_PORT = Number(process.env.E2E_HUB_PORT ?? 47431);
const UI_PORT = Number(process.env.E2E_UI_PORT ?? 5431);
const HUB_URL = `http://127.0.0.1:${HUB_PORT}`;
const channel = process.env.PLAYWRIGHT_CHANNEL;

export default defineConfig({
  testDir: here,
  // Each full wizard walkthrough runs a dozen-plus axe scans on top of the wizard's own real
  // (if fast) delays; generous relative to the root config's 30s.
  timeout: 90_000,
  // The wizard's fake API uses real (if short) timers for its streamed steps; a little slack
  // beyond Playwright's 5s default avoids flaking under load.
  expect: { timeout: 10_000 },
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
      command: `node ${q(path.join(monorepoApps, 'mock-hub/src/server.ts'))}`,
      env: { PORT: String(HUB_PORT) },
      url: `${HUB_URL}/v1/host/info`,
      reuseExistingServer: false,
    },
    {
      command: `node ${q(path.join(uiRoot, 'node_modules/vite/bin/vite.js'))} ${q(uiRoot)} --port ${UI_PORT}`,
      env: { VITE_PITCREW_API: HUB_URL },
      url: `http://127.0.0.1:${UI_PORT}`,
      reuseExistingServer: false,
    },
  ],
});
