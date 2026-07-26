/**
 * Pre-publish gate: install the packed tarball as a stranger would and prove
 * it works, before it becomes permanent on the registry.
 *
 * The workspace hides packaging bugs by construction. Every demo resolves
 * the library through a node_modules symlink into packages/browser-terminal,
 * so it reads whatever is on disk regardless of what `files`, `exports`, or
 * `dependencies` say. A tarball missing dist/wasm, an exports map pointing
 * at a file that was never emitted, a runtime dep declared as a devDep — all
 * of it builds and passes locally and fails on the first real install.
 *
 * So this consumes the actual tarball, from outside the repo, with its own
 * npm install: type resolution through the shipped .d.ts, a bundler
 * resolving the wasm asset URL out of node_modules, and finally a browser
 * instantiating the thing and running a pipeline.
 *
 * Usage: node scripts/verify-tarball.mjs <path-to-tarball>
 */
import { createServer } from 'node:http';
import { execFileSync } from 'node:child_process';
import { mkdtempSync, mkdirSync, writeFileSync, readFile as _r, existsSync, statSync } from 'node:fs';
import { readFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { extname, join, normalize, resolve } from 'node:path';
import { chromium } from '@playwright/test';

const tarball = resolve(process.argv[2] ?? '');
if (!tarball || !existsSync(tarball)) {
  console.error(`usage: node scripts/verify-tarball.mjs <path-to-tarball>\ngot: ${tarball || '(nothing)'}`);
  process.exit(1);
}

const PKG = '@benjamin-small/browser-terminal';
const PORT = 8912;
const dir = mkdtempSync(join(tmpdir(), 'bterm-consumer-'));
console.log(`consumer app: ${dir}`);
console.log(`tarball:      ${tarball}\n`);

const run = (cmd, args, cwd = dir) =>
  execFileSync(cmd, args, { cwd, stdio: 'inherit', env: { ...process.env, CI: '1' } });

// A hand-written app rather than `npm create vite`: no scaffolding download,
// no prompts, and nothing in it that could pass for a reason the check failed.
mkdirSync(join(dir, 'src'), { recursive: true });

writeFileSync(
  join(dir, 'package.json'),
  JSON.stringify(
    {
      name: 'tarball-consumer',
      private: true,
      version: '0.0.0',
      type: 'module',
      scripts: { build: 'vite build' },
      devDependencies: { vite: '^7.0.0', typescript: '^5.6.0' },
    },
    null,
    2,
  ),
);

writeFileSync(
  join(dir, 'tsconfig.json'),
  JSON.stringify(
    {
      compilerOptions: {
        target: 'ES2022',
        module: 'ESNext',
        // `bundler` is what a Vite/webpack consumer actually uses, and it is
        // the mode that reads the "exports" map. Under `node16` a broken
        // exports map can still resolve via "types", so this is the setting
        // that tests what we ship.
        moduleResolution: 'bundler',
        lib: ['ES2022', 'DOM', 'DOM.Iterable'],
        strict: true,
        noEmit: true,
        skipLibCheck: false,
      },
      include: ['src'],
    },
    null,
    2,
  ),
);

writeFileSync(
  join(dir, 'index.html'),
  `<!doctype html><html><body><h1>consumer</h1><h2>second</h2><pre id="out">running…</pre><script type="module" src="/src/main.ts"></script></body></html>`,
);

// Exercises the surface a first-time user touches: the value export, a type
// export, registering a command, and piping its output through builtins.
writeFileSync(
  join(dir, 'src/main.ts'),
  `import { BrowserTerminal } from '${PKG}';
import type { CommandSpec } from '${PKG}';

const spec: CommandSpec = { name: 'headings', summary: 'List headings' };

const bt = await BrowserTerminal.create({ dock: 'right' });
bt.registerCommand(spec, () =>
  [...document.querySelectorAll('h1,h2')].map((h) => ({
    tag: h.tagName.toLowerCase(),
    text: h.textContent?.trim() ?? '',
  })),
);

const upper = await bt.run('echo hi | str upcase');
const count = await bt.run('headings | length');
// The single-match case: it regressed once and errored instead of counting.
const one = await bt.run('headings | grep second --on text | length');

(window as unknown as Record<string, unknown>).__probe = {
  upper: upper.value,
  count: count.value,
  one: one.value,
};
document.querySelector('#out')!.textContent = 'done';
`,
);

console.log('--- installing the tarball (and its declared dependencies) ---');
// Not `npm install <tarball>` alone: this also proves @xterm/* are declared
// as real dependencies rather than devDependencies, since nothing else here
// would pull them in.
run('npm', ['install', '--no-audit', '--no-fund', tarball]);
run('npm', ['install', '--no-audit', '--no-fund']);

console.log('\n--- type-check against the shipped .d.ts ---');
run('npx', ['tsc', '--noEmit']);

console.log('\n--- bundle (resolves the wasm asset out of node_modules) ---');
run('npm', ['run', 'build']);

// A bundle that emits no .wasm means the asset URL silently resolved to
// nothing, which is the failure that looks like success right up until a
// browser asks for it.
const distAssets = join(dir, 'dist', 'assets');
const wasmFiles = existsSync(distAssets)
  ? (await import('node:fs')).readdirSync(distAssets).filter((f) => f.endsWith('.wasm'))
  : [];
if (wasmFiles.length === 0) {
  console.error('\n✗ the build emitted no .wasm asset — the wasm URL did not resolve');
  process.exit(1);
}
const wasmSize = statSync(join(distAssets, wasmFiles[0])).size;
console.log(`\n✓ emitted ${wasmFiles[0]} (${wasmSize} bytes)`);
if (wasmSize < 100_000) {
  console.error('✗ that is far too small to be the real binary');
  process.exit(1);
}

// --- boot it ---
const MIME = {
  '.html': 'text/html',
  '.js': 'text/javascript',
  '.css': 'text/css',
  '.wasm': 'application/wasm',
  '.json': 'application/json',
  '.svg': 'image/svg+xml',
};

const siteDir = join(dir, 'dist');
const server = createServer(async (req, res) => {
  try {
    const path = decodeURIComponent(new URL(req.url, 'http://x').pathname);
    let file = join(siteDir, normalize(path).replace(/^(\.\.[/\\])+/, ''));
    if (existsSync(file) && statSync(file).isDirectory()) file = join(file, 'index.html');
    if (!existsSync(file)) {
      res.writeHead(404).end('not found');
      return;
    }
    res.writeHead(200, { 'content-type': MIME[extname(file)] ?? 'application/octet-stream' });
    res.end(await readFile(file));
  } catch (e) {
    res.writeHead(500).end(String(e));
  }
});
await new Promise((r) => server.listen(PORT, r));

console.log('\n--- booting the built consumer app in chromium ---');
const browser = await chromium.launch();
const page = await browser.newPage();
const errors = [];
page.on('console', (m) => m.type() === 'error' && errors.push(m.text().slice(0, 300)));
page.on('pageerror', (e) => errors.push(String(e).slice(0, 300)));

let probe = null;
try {
  await page.goto(`http://localhost:${PORT}/`, { waitUntil: 'networkidle', timeout: 30000 });
  await page.waitForFunction(() => !!window.__probe, null, { timeout: 20000 });
  probe = await page.evaluate(() => window.__probe);
} catch (e) {
  errors.push(`FAILED: ${String(e).split('\n')[0].slice(0, 200)}`);
}

await browser.close();
server.close();

console.log('probe:', JSON.stringify(probe));
if (errors.length) console.log('errors:', errors.slice(0, 5));

const expected = { upper: 'HI', count: 2, one: 1 };
const mismatches = Object.entries(expected).filter(([k, v]) => probe?.[k] !== v);
if (!probe || mismatches.length > 0) {
  console.error('\n✗ consumer app did not produce the expected results — refusing to publish.');
  for (const [k, v] of mismatches) {
    console.error(`    ${k}: expected ${JSON.stringify(v)}, got ${JSON.stringify(probe?.[k])}`);
  }
  process.exit(1);
}

console.log('\n✓ the tarball installs, type-checks, bundles, boots, and runs pipelines.');
