import js from '@eslint/js';
import reactHooks from 'eslint-plugin-react-hooks';
import globals from 'globals';
import tseslint from 'typescript-eslint';

export default tseslint.config(
  { ignores: ['dist', 'node_modules', 'test-results', 'playwright-report'] },
  {
    files: ['**/*.{ts,tsx}'],
    extends: [js.configs.recommended, ...tseslint.configs.strict],
    plugins: { 'react-hooks': reactHooks },
    rules: { ...reactHooks.configs.recommended.rules },
  },
  // The app runs in a webview: browser globals only.
  { files: ['src/**/*.{ts,tsx}'], languageOptions: { globals: globals.browser } },
  // Server state goes through src/data, whose useLiveQuery waits for the stream to sync.
  {
    files: ['src/**/*.{ts,tsx}'],
    ignores: ['src/data/**'],
    rules: {
      'no-restricted-imports': [
        'error',
        {
          paths: [
            {
              name: '@tanstack/react-query',
              importNames: [
                'useQuery',
                'useQueries',
                'useInfiniteQuery',
                'useSuspenseQuery',
                'useSuspenseQueries',
                'useSuspenseInfiniteQuery',
                'usePrefetchQuery',
                'usePrefetchInfiniteQuery',
              ],
              message:
                'Use useLiveQuery from src/data (or a hook built on it): plain useQuery can fetch before the stream syncs and miss events.',
            },
          ],
        },
      ],
    },
  },
  // Config, tests and specs run in Node (the provider test also gets a DOM from happy-dom).
  {
    files: ['*.{ts,js}', 'tests/**/*.{ts,tsx}', 'e2e/**/*.ts'],
    languageOptions: { globals: { ...globals.node, ...globals.browser } },
  },
);
