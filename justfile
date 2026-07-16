# just wasm  — build the WASM artifact + JS glue into the npm package
# just build — wasm + compile the TypeScript package
# just test  — native Rust tests
# just demo  — build everything and run the Vite demo
# just pack  — npm pack dry-run of the library

wasm:
    cargo build -p bterm-wasm --release --target wasm32-unknown-unknown
    wasm-bindgen --target web --out-dir packages/browser-terminal/src/wasm target/wasm32-unknown-unknown/release/bterm_wasm.wasm
    wasm-opt -Oz -o packages/browser-terminal/src/wasm/bterm_wasm_bg.wasm packages/browser-terminal/src/wasm/bterm_wasm_bg.wasm

build: wasm
    npm --prefix packages/browser-terminal run build

test:
    cargo test --workspace

demo: build
    npm --prefix packages/demo run dev

pack: build
    npm --prefix packages/browser-terminal pack --dry-run
