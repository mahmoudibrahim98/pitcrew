import { defineConfig } from 'vitest/config';

// Tests cover the data layer, which runs in Node against the real mock hub. No React plugin.
export default defineConfig({
  test: {
    include: ['tests/**/*.test.ts'],
    environment: 'node',
    testTimeout: 10_000,
  },
});
