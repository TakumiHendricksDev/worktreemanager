import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

// Tauri drives this dev server, so the port must be fixed and failure must be loud:
// if Vite silently moved to 5174, the webview would load nothing and the app would
// come up blank with no explanation.
const HOST = process.env.TAURI_DEV_HOST;

export default defineConfig({
  plugins: [svelte()],

  // Vite reads .env files by default and exposes VITE_* to the client. This app has no
  // client-side configuration — everything comes from Rust over IPC — so pointing the
  // env dir at a directory with no .env files keeps a stray repo-root .env from ever
  // being bundled into the frontend.
  envDir: './.vite-env',

  clearScreen: false,
  server: {
    port: 5173,
    strictPort: true,
    host: HOST || false,
    hmr: HOST ? { protocol: 'ws', host: HOST, port: 5174 } : undefined,
    watch: {
      // Rust changes are the Tauri CLI's business; watching target/ would thrash.
      ignored: ['**/src-tauri/**', '**/target/**'],
    },
  },

  build: {
    // macOS 13+ ships a modern WebKit, so there is no reason to down-level.
    target: 'safari16',
    sourcemap: !!process.env.TAURI_ENV_DEBUG,
    // `oxc`, not `esbuild`: Vite 8 moved to Rolldown/Oxc, and asking for the esbuild
    // minifier now fails unless esbuild is installed as a separate dependency.
    minify: process.env.TAURI_ENV_DEBUG ? false : 'oxc',
  },
});
