//! The wasm-bindgen boundary crate — the only crate that touches
//! wasm-bindgen/js-sys.
//!
//! Concurrency contract: engine state lives in a `thread_local` and is only
//! reachable through `WasmAccess::with`, a synchronous closure — no borrow
//! can cross an await, and no `&mut self` exports exist. Events queue inside
//! the borrow and flush to the JS callback only after it drops, so a JS
//! handler that synchronously calls back into the engine cannot
//! double-borrow.

use bterm_core::engine::{execute_line, Engine, EngineAccess};
use serde::Serialize;
use std::cell::RefCell;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::spawn_local;

thread_local! {
    static ENGINE: RefCell<Option<Engine>> = const { RefCell::new(None) };
    static ON_EVENT: RefCell<Option<js_sys::Function>> = const { RefCell::new(None) };
}

fn engine_alive() -> bool {
    ENGINE.with(|c| c.borrow().is_some())
}

#[derive(Clone)]
struct WasmAccess;

impl EngineAccess for WasmAccess {
    fn with<R>(&self, f: impl FnOnce(&mut Engine) -> R) -> R {
        ENGINE.with(|cell| {
            let mut guard = cell.borrow_mut();
            let engine = guard
                .as_mut()
                .unwrap_or_else(|| panic!("browser-terminal: engine used after dispose()"));
            f(engine)
        })
    }

    fn events_ready(&self) {
        flush_events();
    }
}

/// Drain queued events and invoke the JS callback with no engine borrow
/// held. Reentrant-safe: a handler that calls back into the engine flushes
/// its own events recursively.
fn flush_events() {
    loop {
        let events = ENGINE.with(|c| {
            c.borrow_mut()
                .as_mut()
                .map(|e| e.drain_events())
                .unwrap_or_default()
        });
        if events.is_empty() {
            return;
        }
        let Some(cb) = ON_EVENT.with(|c| c.borrow().clone()) else {
            return;
        };
        let ser = serde_wasm_bindgen::Serializer::json_compatible();
        for ev in events {
            if let Ok(js) = ev.serialize(&ser) {
                let _ = cb.call1(&JsValue::NULL, &js);
            }
        }
    }
}

fn to_js<T: Serialize>(value: &T) -> JsValue {
    let ser = serde_wasm_bindgen::Serializer::json_compatible();
    value.serialize(&ser).unwrap_or(JsValue::NULL)
}

/// Handle to the engine held by the TypeScript wrapper.
#[wasm_bindgen]
pub struct BtermCore {}

#[wasm_bindgen]
impl BtermCore {
    /// Create the engine (singleton per page) with the event callback.
    /// Emits the banner and first prompt for pane 0.
    #[wasm_bindgen(constructor)]
    pub fn new(on_event: js_sys::Function) -> Result<BtermCore, JsValue> {
        if engine_alive() {
            return Err(JsValue::from_str(
                "browser-terminal: one instance per page in v1; call dispose() first.",
            ));
        }
        ON_EVENT.with(|c| *c.borrow_mut() = Some(on_event));
        ENGINE.with(|c| {
            let mut engine = Engine::new();
            let pane = engine.create_pane(80, 24);
            engine.emit_output(
                pane,
                "\x1b[1mbrowser-terminal\x1b[0m — structured shell. Type \x1b[36mhelp\x1b[0m to explore.\n",
            );
            let prompt = engine.prompt_line(pane);
            engine.emit_output(pane, &prompt);
            *c.borrow_mut() = Some(engine);
        });
        flush_events();
        Ok(BtermCore {})
    }

    /// Sync input hot path: raw terminal input in, echo effects out, same
    /// tick. Submitted lines each spawn their own evaluation task.
    pub fn feed(&self, pane: u32, data: &str) -> JsValue {
        if !engine_alive() {
            return JsValue::NULL;
        }
        let access = WasmAccess;
        let effects = access.with(|e| {
            let fx = e.feed(pane, data);
            if !fx.submitted.is_empty() {
                if let Some(p) = e.pane_mut(pane) {
                    p.running = true;
                }
            }
            fx
        });
        for line in &effects.submitted {
            let line = line.clone();
            spawn_local(execute_line(WasmAccess, pane, line));
        }
        flush_events();
        to_js(&effects)
    }

    pub fn resize(&self, pane: u32, cols: u16, rows: u16) {
        if !engine_alive() {
            return;
        }
        WasmAccess.with(|e| e.resize(pane, cols, rows));
    }

    /// Tear down the engine. Subsequent calls on this handle are no-ops.
    pub fn dispose(&self) {
        ON_EVENT.with(|c| *c.borrow_mut() = None);
        ENGINE.with(|c| *c.borrow_mut() = None);
    }
}
