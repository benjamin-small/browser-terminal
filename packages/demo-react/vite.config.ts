import react from '@vitejs/plugin-react';
import { defineConfig } from 'vite';

export default defineConfig({
  plugins: [react()],
  // Don't pre-bundle the library: it must keep its
  // `new URL('./wasm/…', import.meta.url)` intact to locate the .wasm file.
  optimizeDeps: { exclude: ['browser-terminal'] },
  server: { host: true, port: 5174, fs: { allow: ['../..'] } },
});
