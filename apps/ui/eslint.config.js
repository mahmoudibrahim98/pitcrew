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
  // Environment values (the API's URL, the dev token) are read in src/data/config.ts only, which
  // the desktop app never loads. `import.meta.env.DEV`, a build flag, may be read anywhere: its
  // literal lets the bundler drop development code from builds.
  {
    files: ['src/**/*.{ts,tsx}'],
    ignores: ['src/data/config.ts'],
    rules: {
      'no-restricted-syntax': [
        'error',
        {
          selector:
            "MemberExpression[object.type='MetaProperty'][property.name='env']:not(MemberExpression[property.name='DEV'] > MemberExpression.object)",
          message: 'Read import.meta.env in src/data/config.ts only (import.meta.env.DEV is allowed anywhere).',
        },
      ],
    },
  },
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
  // Config, tests and specs run in Node (tests that render also get a DOM from happy-dom). Tests
  // live in tests/ or next to the code.
  {
    files: [
      '*.{ts,js,mjs}',
      'tests/**/*.{ts,tsx}',
      'e2e/**/*.ts',
      'src/**/*.test.{ts,tsx}',
      'src/**/tests/**/*.{ts,tsx}',
    ],
    languageOptions: { globals: { ...globals.node, ...globals.browser } },
  },
);
