# Configuration

The published `@benjamin-small/browser-terminal` library requires no
environment variables or server-side configuration. Configure an instance in
browser code through `BrowserTerminal.create`:

```ts
const terminal = await BrowserTerminal.create({
  mount: document.querySelector('#terminal') ?? undefined,
  wasmUrl: new URL('/assets/bterm_wasm_bg.wasm', location.href),
  globalToggle: true,
  dock: 'right',
  dockWidth: 480,
  dockTarget: document.body,
});
```

The supported options are:

- `mount`: render inside an existing element instead of using the panel chrome;
- `wasmUrl`: load the WebAssembly module from a custom URL;
- `wasmBinary`: initialize from preloaded WebAssembly bytes instead of a URL;
- `globalToggle`: install the opt-in `Ctrl+\`` window shortcut;
- `dock`: choose `right`, `left`, or `float` panel placement;
- `dockWidth`: set the initial docked width in pixels;
- `dockTarget`: choose the element whose padding is adjusted while docked.

`wasmBinary` takes precedence over `wasmUrl`. Without either option, the
package loads its bundled WebAssembly module. Call `dispose()` before creating
another instance on the same page.

## Development and demo settings

No `.env` file is required for development. The native `bterm` CLI optionally
honors the conventional `COLUMNS` environment variable when determining its
output width. The demo build accepts `BASE` to set the Vite base path; the
GitHub Pages workflow supplies it automatically.
