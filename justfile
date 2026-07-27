# just build    — wasm + TypeScript package
# just test     — native Rust tests (fast, the bulk)
# just typecheck— tsc over the demo (its `vite build` never checks types)
# just test-wasm— wasm-bindgen boundary tests under Node
# just test-e2e — Playwright smoke suite against the demo
# just demo     — build everything and run the Vite demo
# just pack     — npm pack dry-run of the library

# `-Oz` measurably beats `-Os` here (opt-level="s" came out 42 KB larger
# after wasm-opt); `--converge` re-runs passes until they stop helping.
wasm:
    cargo build -p bterm-wasm --release --target wasm32-unknown-unknown
    wasm-bindgen --target web --out-dir packages/browser-terminal/src/wasm target/wasm32-unknown-unknown/release/bterm_wasm.wasm
    wasm-opt -Oz --converge --strip-debug --strip-producers --vacuum -o packages/browser-terminal/src/wasm/bterm_wasm_bg.wasm packages/browser-terminal/src/wasm/bterm_wasm_bg.wasm

# Where the bytes actually are (needs an unstripped build for symbol names).
size:
    cargo build -p bterm-wasm --release --target wasm32-unknown-unknown
    wasm-bindgen --target web --out-dir target/size-analysis target/wasm32-unknown-unknown/release/bterm_wasm.wasm
    twiggy top -n 25 target/size-analysis/bterm_wasm_bg.wasm
    @echo "shipped: $(stat -f%z packages/browser-terminal/src/wasm/bterm_wasm_bg.wasm) raw / $(gzip -c packages/browser-terminal/src/wasm/bterm_wasm_bg.wasm | wc -c | tr -d ' ') gz"

build: wasm
    npm --prefix packages/browser-terminal run build

test:
    cargo test --workspace

test-wasm:
    cargo test -p bterm-wasm --target wasm32-unknown-unknown

# The demo's `vite build` transpiles without checking, so this is the only
# thing that type-checks packages/demo — including the Playwright spec, which
# types `window.bt` from the library's real exports. Depends on `build`: the
# demo resolves `browser-terminal` to its emitted dist/index.d.ts.
typecheck: build
    npm --prefix packages/demo run typecheck

# Type-check first: no point running the smoke suite against a spec that
# describes an API the library no longer has.
test-e2e: typecheck
    cd packages/demo && npx playwright test

demo: build
    npm --prefix packages/demo run dev

# Framework integration demos: the same task list driven by shell commands.
demo-react: build
    npm --prefix packages/demo-react run dev

demo-svelte: build
    npm --prefix packages/demo-svelte run dev

# One self-contained .html — opens from the filesystem, no server needed.
demo-standalone: build
    npm --prefix packages/demo run build
    node packages/demo/scripts/build-standalone.mjs

pack: build
    npm --prefix packages/browser-terminal pack --dry-run

# Build and serve the demo against the *published* npm package — no Rust, no
# repo source for the library. The one check that the workspace symlink can
# never make. See docker/demo-npm/README.md.
demo-npm version="0.1.0":
    docker build -f docker/demo-npm/Dockerfile --build-arg BTERM_VERSION={{version}} -t bterm-demo-npm .
    @echo "→ http://localhost:8080"
    docker run --rm -p 8080:8080 bterm-demo-npm
