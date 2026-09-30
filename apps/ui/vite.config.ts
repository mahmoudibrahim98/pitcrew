import babel from '@rolldown/plugin-babel';
import tailwindcss from '@tailwindcss/vite';
import react, { reactCompilerPreset } from '@vitejs/plugin-react';
import { defineConfig } from 'vite';

export default defineConfig({
  plugins: [react(), babel({ presets: [reactCompilerPreset()] }), tailwindcss()],
  server: { host: '127.0.0.1', port: 5173, strictPort: true },
  preview: { host: '127.0.0.1', port: 4173, strictPort: true },
  build: {
    target: 'es2023',
    // Fonts and icons stay files: inlined data URIs would need a looser CSP.
    assetsInlineLimit: 0,
    // Vite's modulepreload polyfill is an inline script; every webview we ship supports it.
    modulePreload: { polyfill: false },
  },
});
