//! The wasm-bindgen boundary crate — the only crate that touches
//! wasm-bindgen/js-sys. Milestone 1: proves the toolchain round-trip by
//! echoing `feed` input through bterm-core.

use wasm_bindgen::prelude::*;

/// Handle to the engine held by the TypeScript wrapper.
#[wasm_bindgen]
pub struct BtermCore {}

#[wasm_bindgen]
impl BtermCore {
    #[wasm_bindgen(constructor)]
    #[allow(clippy::new_without_default)]
    pub fn new() -> BtermCore {
        BtermCore {}
    }

    /// Sync input hot path: raw terminal input in, echo bytes out, same tick.
    pub fn feed(&self, _pane: u32, data: &str) -> String {
        bterm_core::echo_transform(data)
    }
}
