import { svelte } from '@sveltejs/vite-plugin-svelte';
import { defineConfig } from 'vite';

export default defineConfig({
  plugins: [svelte()],
  // Keep `new URL('./wasm/…', import.meta.url)` intact so the .wasm resolves.
  optimizeDeps: { exclude: ['browser-terminal'] },
  server: { host: true, port: 5175, fs: { allow: ['../..'] } },
});
