import babel from '@rolldown/plugin-babel';
import tailwindcss from '@tailwindcss/vite';
import react, { reactCompilerPreset } from '@vitejs/plugin-react';
import { defineConfig, loadEnv } from 'vite';

export default defineConfig(({ mode }) => {
  // Vite inlines VITE_* variables into the bundle; a token there would ship to every user.
  if (mode === 'production' && loadEnv(mode, import.meta.dirname, 'VITE_').VITE_PITCREW_TOKEN) {
    throw new Error(
      'VITE_PITCREW_TOKEN is set for a production build. It is for browser development only; ' +
        'unset it (check .env.local and .env.production*).',
    );
  }
  return {
    plugins: [react(), babel({ presets: [reactCompilerPreset()] }), tailwindcss()],
    server: { host: '127.0.0.1', port: 5173, strictPort: true },
    preview: { host: '127.0.0.1', port: 4173, strictPort: true },
    build: {
      target: 'es2023',
      // Fonts and icons stay files: inlined data URIs would need a looser CSP.
      assetsInlineLimit: 0,
      // Every webview we ship supports modulepreload, so the polyfill is dead weight in the entry.
      modulePreload: { polyfill: false },
    },
  };
});
