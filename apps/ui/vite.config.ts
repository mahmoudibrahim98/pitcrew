import babel from '@rolldown/plugin-babel';
import tailwindcss from '@tailwindcss/vite';
import react, { reactCompilerPreset } from '@vitejs/plugin-react';
import { defineConfig, type Plugin } from 'vite';

/**
 * Vite inlines VITE_* variables into the bundle, so a token there would ship to every user. Any
 * build (whatever `--mode`) fails while one is set, from the shell or an `.env*` file.
 */
export function noTokenInBuilds(): Plugin {
  return {
    name: 'pitcrew:no-token-in-builds',
    apply: 'build',
    configResolved(config) {
      if (config.env.VITE_PITCREW_TOKEN) {
        throw new Error(
          'VITE_PITCREW_TOKEN is set for a build. It is for the dev server only; ' +
            'unset it (check .env.local and .env.<mode>*).',
        );
      }
    },
  };
}

export default defineConfig({
  plugins: [noTokenInBuilds(), react(), babel({ presets: [reactCompilerPreset()] }), tailwindcss()],
  server: { host: '127.0.0.1', port: 5173, strictPort: true },
  preview: { host: '127.0.0.1', port: 4173, strictPort: true },
  build: {
    target: 'es2023',
    // Fonts and icons stay files: inlined data URIs would need a looser CSP.
    assetsInlineLimit: 0,
    // Every webview we ship supports modulepreload, so the polyfill is dead weight in the entry.
    modulePreload: { polyfill: false },
  },
});
