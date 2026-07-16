import { defineConfig } from 'vite';

export default defineConfig({
  // Don't let esbuild pre-bundle the library: it must keep its
  // `new URL('./wasm/…', import.meta.url)` intact to locate the .wasm file.
  optimizeDeps: { exclude: ['browser-terminal'] },
  server: { fs: { allow: ['../..'] } },
});
