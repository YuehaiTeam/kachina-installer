import { defineConfig } from 'vitest/config';

export default defineConfig({
  esbuild: {
    jsx: 'automatic',
    jsxImportSource: 'preact',
  },
  test: {
    environment: 'jsdom',
    setupFiles: ['./web/__tests__/setup.ts'],
    include: ['web/**/*.test.ts', 'web/**/*.test.tsx'],
  },
});
