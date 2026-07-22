/**
 * Post-build gate: serve the assembled _site exactly as GitHub Pages will
 * (under /<repo>/) and prove each demo actually boots and runs a pipeline.
 *
 * This exists because a green build is not evidence of a working site. An
 * old wasm-opt once silently mangled the externref table that
 * `Closure::new` needs to grow, and every check we had still passed: the
 * Rust tests compile their own wasm, and the demos build fine — the artifact
 * only fails when a browser instantiates it. So we instantiate it here.
 *
 * Usage: node scripts/verify-site.mjs <siteDir> <basePath>
 */
import { createServer } from 'node:http';
import { readFile } from 'node:fs/promises';
import { existsSync, statSync } from 'node:fs';
import { extname, join, normalize } from 'node:path';
import { chromium } from '@playwright/test';

const siteDir = process.argv[2] ?? '_site';
const base = (process.argv[3] ?? '/browser-terminal').replace(/\/$/, '');
const PORT = 8911;

const MIME = {
  '.html': 'text/html',
  '.js': 'text/javascript',
  '.css': 'text/css',
  '.wasm': 'application/wasm',
  '.json': 'application/json',
  '.svg': 'image/svg+xml',
};

const server = createServer(async (req, res) => {
  try {
    let path = decodeURIComponent(new URL(req.url, 'http://x').pathname);
    if (!path.startsWith(base + '/') && path !== base) {
      res.writeHead(404).end('outside base');
      return;
    }
    path = path.slice(base.length) || '/';
    let file = join(siteDir, normalize(path).replace(/^(\.\.[/\\])+/, ''));
    if (existsSync(file) && statSync(file).isDirectory()) file = join(file, 'index.html');
    if (!existsSync(file)) {
      res.writeHead(404).end('not found');
      return;
    }
    const body = await readFile(file);
    res.writeHead(200, { 'content-type': MIME[extname(file)] ?? 'application/octet-stream' });
    res.end(body);
  } catch (e) {
    res.writeHead(500).end(String(e));
  }
});

await new Promise((r) => server.listen(PORT, r));

const DEMOS = [
  { name: 'vanilla', probe: "links | filter {|o| $o.text != ''} | length" },
  { name: 'react', probe: 'tasks | length' },
  { name: 'svelte', probe: 'tasks | length' },
];

const browser = await chromium.launch();
let failed = 0;

for (const demo of DEMOS) {
  const page = await browser.newPage();
  const errors = [];
  page.on('console', (m) => m.type() === 'error' && errors.push(m.text().slice(0, 300)));
  page.on('pageerror', (e) => errors.push(String(e).slice(0, 300)));

  const url = `http://localhost:${PORT}${base}/${demo.name}/`;
  let ok = false;
  let value = null;
  try {
    await page.goto(url, { waitUntil: 'networkidle', timeout: 30000 });
    // The panel only exists once the wasm engine has instantiated.
    await page.waitForFunction(() => !!document.querySelector('[data-browser-terminal]'), null, {
      timeout: 20000,
    });
    // And a pipeline only runs if the engine is genuinely alive.
    value = await page.evaluate(async (line) => {
      const bt = window.bt;
      if (!bt) throw new Error('window.bt missing');
      return await bt.run(line);
    }, demo.probe);
    ok = typeof value === 'number' && value > 0;
  } catch (e) {
    errors.push(`FAILED: ${String(e).split('\n')[0].slice(0, 200)}`);
  }

  console.log(`${ok ? '✓' : '✗'} ${demo.name.padEnd(8)} ${demo.probe} → ${JSON.stringify(value)}`);
  if (errors.length) console.log('    errors:', errors.slice(0, 3));
  if (!ok) failed++;
  await page.close();
}

await browser.close();
server.close();

if (failed) {
  console.error(`\n${failed} demo(s) failed to boot — refusing to publish.`);
  process.exit(1);
}
console.log('\nAll demos boot and execute pipelines.');
