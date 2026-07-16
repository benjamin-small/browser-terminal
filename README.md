# browser-terminal

A terminal/tmux experience for any web page, powered by a Rust core compiled to
WebAssembly.

Floating terminal panels dock at the bottom of your page and run a shell-like
language with **structured-value pipes** (nushell/PowerShell style). Commands can be
built into the Rust core or registered from TypeScript in a few lines — so a
terminal pane can query the DOM, call your app's APIs, and pipe the results through
filters and tables:

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
❯ links --limit 20 | where text ne '' | first 5
```

…and you get a box-drawn table. `Ctrl-B %` splits the pane, `session new` forks a
fresh shell, tmux-style.

## Status

Early development — building toward v1. See
[docs/superpowers/specs/2026-07-16-browser-terminal-design.md](docs/superpowers/specs/2026-07-16-browser-terminal-design.md)
for the full architecture design.

## Layout

- `crates/bterm-core` — the engine: shell language, structured values, pipeline
  evaluation, line editor, renderer, multiplexer state. Zero WASM deps; tested
  natively.
- `crates/bterm-cli` — native REPL over the core (dev harness).
- `crates/bterm-wasm` — the wasm-bindgen boundary crate.
- `packages/browser-terminal` — the npm package: TS wrapper, xterm.js panes,
  floating panel manager.
- `packages/demo` — Vite demo app.

## Development

Requires: Rust (with `wasm32-unknown-unknown` target), Node 20+, `just`,
`wasm-bindgen-cli`, `binaryen` (wasm-opt).

```sh
just test     # native Rust tests
just wasm     # build the WASM artifact into packages/browser-terminal/dist/wasm
just demo     # run the Vite demo
```
