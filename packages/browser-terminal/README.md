# @benjamin-small/browser-terminal

A terminal/tmux experience for any web page, powered by a Rust core compiled to
WebAssembly.

A terminal panel docks to the edge of your page and runs a shell-like language
with **structured-value pipes** (nushell/PowerShell style). Commands are built
into the Rust core or registered from TypeScript in a few lines — so a pane can
query the DOM, call your app's APIs, and pipe the results through filters into
box-drawn tables. `Ctrl-B %` splits panes, `session new` forks shells.

**[Try the live demos →](https://benjamin-small.github.io/browser-terminal/)**

## Install

```sh
npm install @benjamin-small/browser-terminal
```

Works out of the box with Vite and webpack 5 — the `.wasm` loads via
`new URL(..., import.meta.url)`.

## Quickstart

```ts
import { BrowserTerminal } from '@benjamin-small/browser-terminal';

const bt = await BrowserTerminal.create();

bt.registerCommand(
  {
    name: 'links',
    summary: 'List links on the host page',
    flags: [{ long: 'limit', shape: 'int' }],
  },
  ({ flags }) =>
    [...document.querySelectorAll('a')]
      .slice(0, Number(flags.limit ?? 100))
      .map((a) => ({ text: a.textContent?.trim() ?? '', href: a.href })),
);
```

Then, in the panel:

```
❯ links --limit 20 | filter {|o| $o.text != ''} | head 5
┌───────────────┬──────────────────────────────┐
│ text          │ href                         │
├───────────────┼──────────────────────────────┤
│ Rust language │ https://www.rust-lang.org/   │
│ …             │ …                            │
└───────────────┴──────────────────────────────┘
```

A registered command is indistinguishable from a builtin: same `--help`, same
piping, same did-you-mean on typos.

The terminal also works as a scripting engine for the host page:

```ts
const { value } = await bt.run("links | filter {|o| $o.text != ''} | length");
```

`run()` resolves `{ value, log, err }` — diagnostics come back as arrays
instead of printing, so a background call never writes on whatever pane the
user is looking at. On failure or Ctrl-C it rejects with an `Error` carrying
the same two arrays, because a failed run is when its log matters most.

## Options

```ts
await BrowserTerminal.create({
  mount,          // HTMLElement — your own container; skips the panel chrome
  wasmUrl,        // string | URL — custom .wasm location (CDN, no bundler)
  wasmBinary,     // BufferSource — pre-loaded bytes; beats wasmUrl, enables
                  //   single-file builds that run from file://
  globalToggle,   // boolean — opt into a window-level Ctrl+` toggle
  dock,           // 'right' (default) | 'left' | 'float'
  dockWidth,      // number — docked width in px, default 480
  dockTarget,     // HTMLElement — what gets padded, default document.body
});
```

Other surface: `registerCommand` / `unregisterCommand`, `registerFn` /
`unregisterFn` (a named function usable as `@name` in any selector position —
the CSP-safe alternative to inline closures), `run`, `snapshot`,
`setPanelMode`, `panelMode`, `show` / `hide` / `toggle`, `dispose`, and the
variable API below (`setVariable` / `setVariables`, `getVariable`,
`unsetVariable`, `variables`).

## Writing commands

A command receives `(args, input, ctx)` and may return a value or a promise;
arrays of objects render as tables automatically. Async generators stream.

```ts
bt.registerCommand({ name: 'fetch-users' }, async (_args, _input, ctx) => {
  ctx.log('fetching…');                       // channel 3: plain
  const res = await fetch('/api/users', { signal: ctx.signal });
  if (!res.ok) ctx.err(`HTTP ${res.status}`); // channel 2: red
  return res.json();                          // channel 1: the pipe
});
```

`ctx.signal` is an `AbortSignal` — Ctrl-C cancels in-flight work. The three
channels are separate by construction: **nothing written to `ctx.log` or
`ctx.err` can enter the pipe**, so a downstream `| length` is unaffected by
anything a command logs.

For partial output — progress bars, in-place redraws — `ctx.log` and `ctx.err`
are also writer objects:

```ts
ctx.log.mode('byte');            // 'byte' | 'line' (default) | 'block'
ctx.log.write(`\r${bar} ${pct}%`);
ctx.log.flush();
```

Raw writes pass through a control-character allowlist: `\r`, `\b`, `\t`,
cursor moves within the line, erase-to-end-of-line, and colors survive;
anything that could clear the screen, position absolutely, or open an OS
command sequence is stripped.

Note that `ctx.log('line')` and `ctx.log.write(bytes)` are different APIs
rather than two spellings of one. The first passes a *message* — the shell
frames and sanitizes it. The second passes *terminal bytes* — you own the
framing. Mixing both on one channel can interleave out of order, since the
line call bypasses the buffer.

## Host state as shell variables

Inject application state and reference it as `$name`, instead of pasting it
into the command text:

```ts
bt.setVariables({
  game: gameDefinition,      // any Value: string, number, record, list, null
  build: currentBuild,
});

await bt.run('simulate --game $game --build $build');
```

Variables are visible in every pane and session, including ones created
afterwards, and inside interpolation (`"run-$game"`). Values cross as typed
values and are never parsed as shell source, so a string containing `;` or
`|` stays a string.

A change takes effect from the next command line — a pipeline already
running keeps the values it started with, so a long command cannot see one
of its arguments change halfway through.

```ts
bt.getVariable('game');     // the value, or undefined if unset
bt.variables();             // everything you injected
bt.unsetVariable('game');   // true if it was set
```

`undefined` from `getVariable` means "not set"; `null` means you injected
`null`. In the terminal, `vars` lists what is visible and pipes like any
other table (`vars | grep game`).

### Scoping a variable to one session

By default a variable is engine-wide. To scope one to a single session,
pass an id from `bt.snapshot.sessions`:

```ts
const [first] = bt.snapshot!.sessions;
bt.setVariable('scratch', 'local', { scope: 'session', session: first.id });
```

A session value shadows a host value of the same name, in that session
only. Reads name a layer rather than merging, so both remain readable:

```ts
bt.getVariable('scratch');                                          // the host value
bt.getVariable('scratch', { scope: 'session', session: first.id }); // that session's
```

For "what would `$scratch` actually be here?", the shell's `vars` shows
the resolved view with a `scope` column saying where each value came from:
`vars | filter {|v| $v.scope == 'session'}`.

Sessions are addressed by explicit id rather than "whichever is active",
because a user can switch sessions between your read and your write. An
unknown id throws from `setVariable`, `setVariables`, `getVariable` and
`variables`. `unsetVariable` is the one exception, returning `false`,
because its `boolean` has nowhere to put an error. So `undefined` from
`getVariable` means one thing only: the name is not set in that layer.

## Limitations

- **One instance per page.** `create()` throws if one is already live; call
  `dispose()` first.
- **Browser only.** No SSR or Jest — guard your imports accordingly.
- A failing command's buffered `.write()` output currently prints *below* its
  error message rather than above it. Nothing is lost, only ordered oddly, and
  only on the error path.
- `grep` uses JavaScript's native `RegExp`, so full regex works in the browser.
  The native CLI has no JS engine and falls back to substring matching:
  patterns that work in the CLI always work in the browser, not the reverse.

## Links

- [Live demos](https://benjamin-small.github.io/browser-terminal/) — vanilla,
  React, and Svelte
- [Source and development docs](https://github.com/benjamin-small/browser-terminal)
- [Issues](https://github.com/benjamin-small/browser-terminal/issues)

Apache-2.0
