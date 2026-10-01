// A scoped Vitest config for this stream's own tests, since `apps/ui/vitest.config.ts` (owned by
// stream L) only looks in `apps/ui/tests/**` and this stream may only edit `src/onboarding/**`.
// Run with `corepack pnpm --filter @pitcrew/ui exec vitest run --config src/onboarding/vitest.config.ts`.
//
// See README.md, "Running this stream's tests": the root config's `include` needs
// `'src/**/*.test.{ts,tsx}'` added so `pnpm test` picks these up too; that edit is stream L's.
import { defineConfig } from 'vitest/config';

export default defineConfig({
  test: {
    include: ['src/onboarding/**/*.test.{ts,tsx}'],
    environment: 'happy-dom',
    testTimeout: 10_000,
  },
});
