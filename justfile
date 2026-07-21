# just build    — wasm + TypeScript package
# just test     — native Rust tests (fast, the bulk)
# just test-wasm— wasm-bindgen boundary tests under Node
# just test-e2e — Playwright smoke suite against the demo
# just demo     — build everything and run the Vite demo
# just pack     — npm pack dry-run of the library

wasm:
    cargo build -p bterm-wasm --release --target wasm32-unknown-unknown
    wasm-bindgen --target web --out-dir packages/browser-terminal/src/wasm target/wasm32-unknown-unknown/release/bterm_wasm.wasm
    wasm-opt -Oz -o packages/browser-terminal/src/wasm/bterm_wasm_bg.wasm packages/browser-terminal/src/wasm/bterm_wasm_bg.wasm

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

# One self-contained .html — opens from the filesystem, no server needed.
demo-standalone: build
    npm --prefix packages/demo run build
    node packages/demo/scripts/build-standalone.mjs

pack: build
    npm --prefix packages/browser-terminal pack --dry-run
