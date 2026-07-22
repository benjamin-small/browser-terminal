# just build    — wasm + TypeScript package
# just test     — native Rust tests (fast, the bulk)
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

test-e2e: build
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
