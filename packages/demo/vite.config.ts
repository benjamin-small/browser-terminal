import { defineConfig } from 'vite';

export default defineConfig({
  // Don't let esbuild pre-bundle the library: it must keep its
  // `new URL('./wasm/…', import.meta.url)` intact to locate the .wasm file.
  optimizeDeps: { exclude: ['browser-terminal'] },
  // `host: true` binds 0.0.0.0 instead of localhost, so the dev server is
  // reachable from a phone on the same network. Without it, Vite listens on
  // localhost only and a LAN IP just refuses the connection — which looks
  // exactly like "the app is broken on mobile".
  server: { host: true, fs: { allow: ['../..'] } },
});
