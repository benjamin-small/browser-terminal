// Welds `vite build`'s three files into a single self-contained .html that
// opens straight from the filesystem — no server, no network.
//
// Why this needs to exist at all: a `file://` page can't `fetch()` anything
// (opaque origin), which normally rules out both ES module imports and
// loading the .wasm. Inlining the bundle removes the imports, and handing
// the library pre-decoded wasm bytes via `wasmBinary` removes the fetch.
//
// Cost: base64 inflates the wasm by ~33% and we lose streaming compilation.
// Worth it for a demo you can email; not how you'd ship to the web.
import { mkdirSync, readdirSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const demoRoot = join(dirname(fileURLToPath(import.meta.url)), '..');
const dist = join(demoRoot, 'dist');
const assetDir = join(dist, 'assets');

const assets = readdirSync(assetDir);
const jsName = assets.find((f) => f.endsWith('.js'));
const wasmName = assets.find((f) => f.endsWith('.wasm'));
if (!jsName || !wasmName) {
  throw new Error(`expected a .js and a .wasm in ${assetDir}; run \`vite build\` first`);
}

const html = readFileSync(join(dist, 'index.html'), 'utf8');
const js = readFileSync(join(assetDir, jsName), 'utf8');
const wasm = readFileSync(join(assetDir, wasmName));

// A single leftover import would defeat the whole point — it'd be fetched.
const bareImport = /(^|\n)\s*import\s[^;]*from\s*["']/.test(js);
if (bareImport) {
  throw new Error('bundle still contains external imports; cannot inline safely');
}

// `</script` inside any string literal would terminate our inline script
// early. The escaped form is equivalent to the JS parser.
const safeJs = js.replace(/<\/script/gi, '<\\/script');

const stripped = html.replace(/<script\b[^>]*\bsrc="[^"]*"[^>]*>\s*<\/script>/gi, '');
if (stripped === html) {
  throw new Error('did not find the bundle <script src> to replace');
}

const inlined = `
<script type="module">
  // Decoded before the bundle runs, so create() never touches the network.
  const b64 = "${wasm.toString('base64')}";
  const bin = Uint8Array.from(atob(b64), (c) => c.charCodeAt(0));
  globalThis.__BTERM_WASM__ = bin;
</script>
<script type="module">
${safeJs}
</script>
</body>`;

const outDir = join(demoRoot, 'dist-standalone');
mkdirSync(outDir, { recursive: true });
const outFile = join(outDir, 'browser-terminal-demo.html');
writeFileSync(outFile, stripped.replace('</body>', inlined));

const mb = (n) => `${(n / 1024 / 1024).toFixed(2)} MB`;
console.log(`wrote ${outFile}`);
console.log(`  wasm ${mb(wasm.length)} raw -> ${mb(wasm.toString('base64').length)} base64`);
console.log(`  single file: ${mb(readFileSync(outFile).length)} (open it directly, no server)`);
