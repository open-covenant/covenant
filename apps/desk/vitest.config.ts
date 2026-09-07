import { defineConfig } from 'vitest/config';

const live = process.env.DESK_LIVE === '1';

export default defineConfig({
  test: {
    environment: 'node',
    include: ['src/**/__tests__/**/*.test.ts', 'test/**/*.test.ts'],
    // Live tests read mainnet and are opt-in with DESK_LIVE=1. They stay
    // read-only: no test signs or sends a transaction.
    exclude: ['node_modules/**', 'dist/**', ...(live ? [] : ['**/*.live.test.ts'])],
    testTimeout: live ? 120_000 : 15_000,
  },
});
