import { defineConfig } from 'vitest/config';

// Tests cover the data layer, in Node against the real mock hub; the provider test uses happy-dom.
// No React Compiler here.
export default defineConfig({
  test: {
    include: ['tests/**/*.test.{ts,tsx}'],
    environment: 'node',
    testTimeout: 10_000,
  },
});
