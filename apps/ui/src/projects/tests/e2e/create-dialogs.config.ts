import { defineConfig } from '@playwright/test';
import config from './playwright.config.ts';

// The same dialogs run against a disposable real demo hub when E2E_HUB_URL is set.
const external = process.env.E2E_HUB_URL;
export default defineConfig({
  ...config,
  testMatch: 'create-dialogs.spec.ts',
  webServer: external === undefined ? config.webServer : [{
    ...(Array.isArray(config.webServer) ? config.webServer[1] : {}),
    command: `node node_modules/vite/bin/vite.js --port ${process.env.E2E_UI_PORT ?? 5482} --strictPort`,
    env: { VITE_PITCREW_API: external, VITE_PITCREW_TOKEN: process.env.E2E_HUB_TOKEN ?? '' },
    url: `http://127.0.0.1:${process.env.E2E_UI_PORT ?? 5482}`,
  }],
});
