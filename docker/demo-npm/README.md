# demo-npm — the demo, built from the published package

Proves that `npm install @benjamin-small/browser-terminal` is sufficient:
no Rust, no wasm toolchain, no repo source for the library.

```sh
docker build -f docker/demo-npm/Dockerfile -t bterm-demo-npm .
docker run --rm -p 8080:8080 bterm-demo-npm
# → http://localhost:8080
```

Pin a different release:

```sh
docker build -f docker/demo-npm/Dockerfile --build-arg BTERM_VERSION=0.1.0 -t bterm-demo-npm .
```

## Why it exists

Every other build here resolves the library through a workspace symlink into
`packages/browser-terminal`, so it reads whatever is on disk regardless of
what was published. "The demo works" has therefore never been evidence that
someone else's `npm install` works — and a packaging mistake (a missing
`dist/wasm`, an `exports` map pointing at a file that was never emitted, a
runtime dependency declared as a devDependency) is invisible until a stranger
hits it.

This image removes the symlink from the equation.

## What makes it a proof rather than a demo

`.dockerignore` excludes `crates/` and `packages/browser-terminal/`
outright, so no `COPY` can quietly reintroduce the source. Only the demo
*application* is copied — its own `src/` and `index.html`, which import the
library by name and reference nothing else outside that directory.

The build then asserts, and fails rather than warns:

- the lockfile entry resolves to `https://registry.npmjs.org/...`, printing
  the version and integrity hash — an install that somehow came from a local
  path stops the build instead of producing a misleading green
- `dist/wasm/bterm_wasm_bg.wasm` exists inside the installed package and is
  over 100 KB, so a truncated or placeholder binary cannot pass
- `@xterm/xterm` resolved even though the manifest never mentions it, which
  is what proves the library declares its runtime dependencies correctly;
  the repo's own demo lists xterm itself, so it could not catch this
- the bundle emitted a `.wasm` asset — an unresolved asset URL is
  success-shaped right up until a browser asks for the file
- the runtime image really has `index.html` and a `.wasm` under its served
  root

The runtime stage is nginx over static files: no `node`, `npm`, `cargo`,
`rustc`, `wasm-bindgen` or `wasm-opt` in the final image, and no `.rs` file
anywhere in it.

`nginx.conf` declares `application/wasm` explicitly. A `.wasm` served as
`application/octet-stream` makes `WebAssembly.instantiateStreaming` reject
the response, and the resulting error reads like a library bug rather than a
server misconfiguration.

## Verified

Built against `@benjamin-small/browser-terminal@0.1.0`
(`sha512-0XOmw6Rn/yHkvjHiTI/mQgo5kSj9cr2GiQc3ZhelPltjenDVakusrDmr16i19xZGyTi3l1JjX31ZOM1Go78M5A==`),
served at `http://localhost:8080`, driven in a real browser:

| check | result |
| --- | --- |
| wasm `Content-Type` | `application/wasm` |
| engine instantiated | terminal mounted, no console errors |
| `links \| filter {\|o\| $o.text != ''} \| length` | `7` |
| `echo hi \| str upcase` | `HI` |
| `links \| grep rust --on href -i \| length` | `1` |
| commands registered | 35, including the page-side `links` |

The `1` is worth noting: that is the single-match case that used to error
before the batch-model fix, confirmed here through the published artifact
rather than the working tree.
