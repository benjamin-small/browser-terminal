# browser-terminal

A terminal/tmux experience for any web page, powered by a Rust core compiled to
WebAssembly.

A floating terminal panel docks at the bottom of your page and runs a shell-like
language with **structured-value pipes** (nushell/PowerShell style). Commands can
be built into the Rust core or registered from TypeScript in a few lines — so a
terminal pane can query the DOM, call your app's APIs, and pipe the results
through filters into box-drawn tables. `Ctrl-B %` splits panes, `session new`
forks shells, tmux-style.

```ts
import { BrowserTerminal } from 'browser-terminal';

const bt = await BrowserTerminal.create();
bt.registerCommand(
  { name: 'links', summary: 'List links on the host page', flags: [{ long: 'limit', shape: 'int' }] },
  ({ flags }) => [...document.querySelectorAll('a')]
      .slice(0, Number(flags.limit ?? 100))
      .map(a => ({ text: a.textContent?.trim() ?? '', href: a.href }))
);
```

Then, in the terminal panel:

```
❯ links --limit 20 | where text ne '' | head 5
┌───────────────┬──────────────────────────────┐
│ text          │ href                         │
├───────────────┼──────────────────────────────┤
│ Rust language │ https://www.rust-lang.org/   │
│ …             │ …                            │
└───────────────┴──────────────────────────────┘
```

And from host-page code, the terminal doubles as a scripting engine:

```ts
const count = await bt.run("links | where text ne '' | length");  // → 5
```

## Features

- **Structured pipes**: values (strings, numbers, lists, records) flow between
  commands; tables render automatically at the end of a pipeline.
- **Commands in Rust or TypeScript**: builtins (`where`, `grep`, `sort-by`,
  `get`, `head`, `to json`, …) plus `registerCommand` for page-side commands.
  TS commands get an `AbortSignal` (Ctrl-C cancels in-flight `fetch`es) and an
  `emit()` for progressive output.
- **tmux surface**: `Ctrl-B` prefix — `%`/`"` split, `c`/`n`/`p` windows,
  `x` kill, `o`/arrows focus, `z` zoom, `(`/`)`/`s` sessions, `d` hide.
  Every keybinding is just a shell command (`mux split --right`, `session new`).
- **Floating panel**: Shadow-DOM isolated (no CSS bleed either way), drag by
  header, edge resize, divider drag, window tabs, session pill dock,
  minimize-to-pill. No global listeners unless you opt into `globalToggle`.
- **Real shell feel**: sync keystroke echo (no async hop), line editor with
  history/cursor ops, caret diagnostics with did-you-mean, bracketed paste
  (pasted newlines never auto-execute), red/green status prompt.

## Install

```sh
npm install browser-terminal
```

Works out of the box with Vite / webpack 5 (the `.wasm` loads via
`new URL(..., import.meta.url)`).

**No bundler?** Load from a CDN and point at the wasm explicitly:

```html
<script type="module">
  import { BrowserTerminal } from 'https://esm.sh/browser-terminal';
  const bt = await BrowserTerminal.create({
    wasmUrl: 'https://esm.sh/browser-terminal/dist/wasm/bterm_wasm_bg.wasm',
  });
</script>
```

This library requires a browser (no SSR/Jest); guard imports accordingly.
One instance per page in v1 — `create()` throws if one is live; `dispose()`
first.

**Everything runs client-side.** There is no server component — a production
build is three static files (HTML, one JS bundle, one `.wasm`) that any file
host will serve. A dev server is needed only because `file://` pages have an
opaque origin and cannot `fetch()` ES modules or the wasm binary.

To remove even that, pass the wasm as bytes and inline everything:

```ts
BrowserTerminal.create({ wasmBinary: myUint8Array });  // never fetches
```

`just demo-standalone` does exactly this, producing one self-contained
`.html` that opens by double-click with no server at all (verified: a single
`file://` request, no network). Base64 inflates the wasm ~33% and you lose
streaming compilation, so this is for demos you can email — not how you'd
ship to the web.

## API sketch

```ts
const bt = await BrowserTerminal.create({
  mount?: HTMLElement;        // bring your own container (skips panel chrome)
  wasmUrl?: string | URL;     // custom .wasm location
  globalToggle?: boolean;     // opt-in Ctrl+` show/hide
});

bt.registerCommand(spec, (args, input, ctx) => value | Promise<value>);
//  spec: { name, summary?, required?, optional?, rest?, flags? }
//  args: { positionals: Value[], flags: Record<string, Value> }
//  ctx:  { signal: AbortSignal, emit(line: string): void }
//  throw { message, help? } for rich diagnostics
bt.unregisterCommand(name);
bt.registerFn(name, (item) => value);  // usable as @name in any selector
bt.unregisterFn(name);
bt.run(line): Promise<Value>;  // programmatic execution, typed result
bt.snapshot;                   // sessions/windows/pane rects
bt.show(); bt.hide(); bt.toggle(); bt.dispose();
```

## The shell language (v1)

`;`-separated pipelines; multi-word commands (`str upcase`); flags
(`--limit 5`, `--limit=5`, `-l 5`); `'raw'` and `"interpolated $var"` strings;
`#` comments. `where` uses word operators so everything parses cleanly:
`where text ne ''`, `where n gt 4`, `where href starts-with https`.
Operators: `eq ne gt lt ge le contains starts-with ends-with`.

Where `where` tests one column, **`grep` searches everything** — it matches
against the text each value *displays as*, so what you see in a table is what
you match:

```
links | grep 'rust|xterm' -i        # any cell, case-insensitive
links | grep '^https' --column href # one column only
links | grep tmux -v                # invert
```

`grep` uses **JavaScript's native `RegExp`** in the browser — full regex
(including lookahead and backreferences, which Rust's `regex` crate can't do)
for **zero added binary size**, since the engine is already in every JS
runtime. The native CLI has no JS engine, so it falls back to substring
matching: patterns that work in the CLI always work in the browser, but not
the reverse.

### Selectors: `--on`, `map`, `filter`

Anywhere a command needs to know *which part* of an item to look at, it takes
the same three-form selector — learned once, identical everywhere:

| form | meaning |
|---|---|
| `href`, `user.name` | a field, or a dotted path into nested records |
| `'(o) => o.id > 5'` | inline JavaScript, compiled by the browser |
| `@slug` | a function registered from TypeScript |

`--on` is the **common parameter**: it changes what a command *examines*
while keeping the whole row — something you can't express by piping through
`get`, which would discard the other columns.

```
links | grep '^https' --on href          # test one field, keep whole rows
links | sort-by --on '(o) => o.text.length'   # computed sort key
links | map '(o) => ({ site: o.text, host: o.href })'   # reshape rows
links | filter '(o) => o.text.length > 4' | tail 5
```

`map` and `filter` are the composable half: `--on` narrows what a command
looks at, `map` changes what flows downstream.

Commands where `--on` would be meaningless (`length`, `head`) simply don't
declare it and reject it with the usual did-you-mean — no silent no-ops.

**Inline functions use `eval`**, so a page whose Content-Security-Policy
omits `unsafe-eval` will refuse them. Register the function instead — no
`eval`, works under any CSP, and stays type-checked and debuggable:

```ts
bt.registerFn('host', (link) => new URL(link.href).hostname);
// then:  links | map @host
```

The engine detects a blocking CSP once and says so, pointing at
`registerFn` rather than surfacing a raw JS error.
`( ) { } && || > <` are reserved for v2 (closures, operators, redirects).

Run `help` in the panel for the full command list; `help <command>` /
`<command> --help` for usage.

## Layout

- `crates/bterm-core` — the engine: language, values, eval, line editor,
  renderer, multiplexer. Zero wasm deps; ~120 native tests.
- `crates/bterm-cli` — native REPL over the core (dev harness).
- `crates/bterm-wasm` — the wasm-bindgen boundary (+8 boundary tests).
- `packages/browser-terminal` — the npm package: TS wrapper, xterm.js panes,
  Shadow-DOM panel.
- `packages/demo` — Vite demo + Playwright smoke suite (`npx playwright test`).

Architecture spec:
[docs/superpowers/specs/2026-07-16-browser-terminal-design.md](docs/superpowers/specs/2026-07-16-browser-terminal-design.md)

## Development

Requires: Rust (`wasm32-unknown-unknown` target), Node 20+, `just`,
`wasm-bindgen-cli` (matching the pinned crate version), `binaryen`.

```sh
just test       # native Rust tests (fast, the bulk)
just test-wasm  # wasm-bindgen boundary tests under Node
just build      # wasm + TS package build
just demo       # run the Vite demo
just test-e2e   # Playwright smoke suite
just pack       # npm pack dry-run
```

Current wasm size: ~392 KB raw, ~174 KB gzipped.
