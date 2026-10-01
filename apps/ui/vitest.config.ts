import { cpus } from 'node:os';
import { defineConfig } from 'vitest/config';

// Tests run in Node against the real mock hub; files that render opt into happy-dom with a
// `@vitest-environment` comment. They live in tests/ or next to the code, as src/**/*.test.ts(x)
// (features keep theirs in their own folders, e.g. src/projects/tests/). No React Compiler here.
//
// Each test file that talks to the mock hub starts its own instance. Left unbounded, Vitest
// forks one worker per CPU, and that many mock hubs starting at once flakes with ECONNRESET
// under heavy machine load. Cap it well below the core count; 4 is plenty of parallelism for
// this suite's size without piling on concurrent hub startups.
export default defineConfig({
  test: {
    include: ['tests/**/*.test.{ts,tsx}', 'src/**/*.test.{ts,tsx}'],
    environment: 'node',
    testTimeout: 10_000,
    maxWorkers: Math.min(4, cpus().length),
  },
});
