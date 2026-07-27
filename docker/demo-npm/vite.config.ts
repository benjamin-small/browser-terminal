import { defineConfig } from 'vite';

export default defineConfig({
  // The one setting that matters here. esbuild's pre-bundling rewrites
  // `new URL('./wasm/…', import.meta.url)` and the library then cannot
  // locate its own .wasm — the failure looks like a 404 for a hashed asset
  // that was never emitted. The repo's demo needs this too; a consumer
  // installing from npm needs it for exactly the same reason, which is
  // itself worth proving.
  optimizeDeps: { exclude: ['@benjamin-small/browser-terminal'] },
});
