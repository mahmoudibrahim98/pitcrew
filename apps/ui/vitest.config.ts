import { defineConfig } from 'vitest/config';

// Tests run in Node against the real mock hub; files that render opt into happy-dom with a
// `@vitest-environment` comment. They live in tests/ or next to the code, as src/**/*.test.ts(x)
// (features keep theirs in their own folders, e.g. src/projects/tests/). No React Compiler here.
export default defineConfig({
  test: {
    include: ['tests/**/*.test.{ts,tsx}', 'src/**/*.test.{ts,tsx}'],
    environment: 'node',
    testTimeout: 10_000,
  },
});
