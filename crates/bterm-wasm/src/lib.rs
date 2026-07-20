//! The wasm-bindgen boundary crate — the only crate that touches
//! wasm-bindgen/js-sys.
//!
//! Concurrency contract: engine state lives in a `thread_local` and is only
//! reachable through `WasmAccess::with`, a synchronous closure — no borrow
//! can cross an await, and no `&mut self` exports exist. Events queue inside
//! the borrow and flush to the JS callback only after it drops, so a JS
//! handler that synchronously calls back into the engine cannot
//! double-borrow. Every submitted pipeline runs inside an `Abortable` whose
//! abort flag is checked before the body resumes — Ctrl-C and `dispose()`
//! can always settle in-flight work without it touching the engine again.

mod convert;
mod js_command;
mod js_fn;
mod js_regex;
mod tasks;

use bterm_core::abort::Abortable;
use bterm_core::engine::{eval_to_value, execute_line, Engine, EngineAccess};
use bterm_core::signature::Signature;
use js_command::JsCommand;
use serde::Serialize;
use std::cell::RefCell;
use std::rc::Rc;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::{future_to_promise, spawn_local};

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

    fn panes_closed(&self, panes: &[u32]) {
        // Runs with no engine borrow held: AbortController listeners may
        // synchronously call back into the engine.
        tasks::abort_panes(panes);
    }

    fn lookup_fn(&self, name: &str) -> Result<Rc<dyn bterm_core::callable::HostFn>, String> {
        // The registry lives outside the engine, so no borrow is involved.
        js_fn::lookup(name)
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

/// Spawn one submitted line as an abortable task, tracked in the task
/// registry so Ctrl-C / dispose can cancel it.
fn spawn_pipeline(pane: u32, line: String) {
    let run_id = tasks::next_id();
    let Ok(controller) = web_sys::AbortController::new() else {
        return;
    };
    let (fut, handle) = Abortable::wrap(execute_line(WasmAccess, pane, line, run_id));
    tasks::register(run_id, pane, handle, controller);
    spawn_local(async move {
        let result = fut.await;
        tasks::finish(run_id);
        if result.is_err() && engine_alive() {
            // Aborted: execute_line never reached finish_pane.
            WasmAccess.with(|e| {
                if let Some(p) = e.pane_mut(pane) {
                    p.running = false;
                    p.editor.set_last_status(false);
                }
            });
        }
    });
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
            // Upgrade `grep` from substring to real regex, and enable inline
            // callables — both free, since the browser's JS engine is
            // already loaded.
            engine.set_matcher(Rc::new(js_regex::JsRegexMatcher));
            engine.set_fn_compiler(Rc::new(js_fn::JsFnCompiler));
            let pane = engine.mux.active_pane();
            engine.emit_output(
                pane,
                "\x1b[1mbrowser-terminal\x1b[0m — structured shell. Type \x1b[36mhelp\x1b[0m to explore, \x1b[36mCtrl-B %\x1b[0m to split.\n",
            );
            let prompt = engine.prompt_line(pane);
            engine.emit_output(pane, &prompt);
            let snapshot = engine.snapshot();
            engine.emit(bterm_core::protocol::EngineEvent::LayoutChanged { snapshot });
            *c.borrow_mut() = Some(engine);
        });
        flush_events();
        Ok(BtermCore {})
    }

    /// Host → engine control messages: prefix chord, post-prefix keys, pane
    /// clicks, divider drags. Message shape is `HostMsg` as tagged JSON.
    pub fn dispatch(&self, msg: JsValue) -> Result<(), JsValue> {
        if !engine_alive() {
            return Ok(());
        }
        let json = js_sys::JSON::stringify(&msg)
            .map_err(|_| JsValue::from_str("invalid HostMsg: not JSON-serializable"))?;
        let msg: bterm_core::protocol::HostMsg = serde_json::from_str(
            &json.as_string().ok_or_else(|| JsValue::from_str("invalid HostMsg"))?,
        )
        .map_err(|e| JsValue::from_str(&format!("invalid HostMsg: {e}")))?;

        let result = WasmAccess.with(|e| e.handle_msg(msg));
        if !result.closed_panes.is_empty() {
            tasks::abort_panes(&result.closed_panes);
        }
        if let Some((pane, cmd)) = result.run {
            spawn_pipeline(pane, cmd);
        }
        flush_events();
        Ok(())
    }

    /// Current layout snapshot (sessions, windows, pane rects).
    pub fn snapshot(&self) -> JsValue {
        if !engine_alive() {
            return JsValue::NULL;
        }
        let snapshot = WasmAccess.with(|e| e.snapshot());
        to_js(&snapshot)
    }

    /// Sync input hot path: raw terminal input in, echo effects out, same
    /// tick. Submitted lines each spawn their own abortable task; Ctrl-C
    /// aborts everything running in the pane.
    pub fn feed(&self, pane: u32, data: &str) -> JsValue {
        if !engine_alive() {
            return JsValue::NULL;
        }
        let effects = WasmAccess.with(|e| e.feed(pane, data));
        if effects.ctrl_c {
            // Runs after the borrow is released: controller.abort() can
            // synchronously invoke JS abort listeners.
            tasks::abort_pane(pane);
        }
        for line in &effects.submitted {
            WasmAccess.with(|e| {
                if let Some(p) = e.pane_mut(pane) {
                    p.running = true;
                }
            });
            spawn_pipeline(pane, line.clone());
        }
        if !effects.submitted.is_empty() || effects.ctrl_c {
            WasmAccess.with(|e| {
                if let Some(p) = e.pane_mut(pane) {
                    p.running = tasks::pane_busy(pane);
                }
            });
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

    /// Register a TS command. Errors if the name collides with a builtin;
    /// re-registering a TS command replaces it (the HMR behavior) with a
    /// console warning.
    pub fn register_command(&self, sig: JsValue, f: js_sys::Function) -> Result<(), JsValue> {
        if !engine_alive() {
            return Err(JsValue::from_str("browser-terminal: engine is disposed"));
        }
        // Through JSON text, not serde_wasm_bindgen::from_value:
        // serde-wasm-bindgen reads struct fields by direct property lookup,
        // which silently ignores unknown fields — a TS author's typo
        // (`flag` vs `flags`) must error loudly instead.
        let sig_json = js_sys::JSON::stringify(&sig)
            .map_err(|_| JsValue::from_str("invalid command signature: not JSON-serializable"))?;
        let sig: Signature = serde_json::from_str(
            &sig_json
                .as_string()
                .ok_or_else(|| JsValue::from_str("invalid command signature"))?,
        )
        .map_err(|e| JsValue::from_str(&format!("invalid command signature: {e}")))?;
        if sig.name.trim().is_empty() {
            return Err(JsValue::from_str("command name must not be empty"));
        }
        let name = sig.name.clone();
        let outcome = WasmAccess.with(|e| {
            e.registry
                .register_external(Rc::new(JsCommand { sig, func: f }))
        });
        match outcome {
            Ok(bterm_core::registry::RegisterOutcome::Replaced) => {
                web_sys::console::warn_1(&JsValue::from_str(&format!(
                    "browser-terminal: command `{name}` re-registered (replacing the previous registration)"
                )));
                Ok(())
            }
            Ok(_) => Ok(()),
            Err(e) => Err(JsValue::from_str(&e.msg)),
        }
    }

    /// Remove a TS-registered command (builtins are not removable).
    pub fn unregister_command(&self, name: &str) {
        if !engine_alive() {
            return;
        }
        WasmAccess.with(|e| e.registry.unregister_external(name));
    }

    /// Register a named function usable as `@name` in any selector
    /// (`--on`, `map`, `filter`). Unlike inline source this needs no `eval`,
    /// so it works under a strict Content-Security-Policy.
    pub fn register_fn(&self, name: &str, func: js_sys::Function) -> Result<(), JsValue> {
        if name.trim().is_empty() {
            return Err(JsValue::from_str("function name must not be empty"));
        }
        if name.starts_with('@') {
            return Err(JsValue::from_str(
                "register the bare name; `@` is only used at the call site",
            ));
        }
        js_fn::register(name, func);
        Ok(())
    }

    pub fn unregister_fn(&self, name: &str) {
        js_fn::unregister(name);
    }

    /// Programmatic execution: evaluate a line in a pane's context and
    /// resolve with the final structured value (no prompt echo, no pane
    /// render). Rejects with an Error whose message is the shell error.
    pub fn run(&self, pane: u32, line: String) -> js_sys::Promise {
        if !engine_alive() {
            return js_sys::Promise::reject(&JsValue::from_str(
                "browser-terminal: engine is disposed",
            ));
        }
        future_to_promise(async move {
            let run_id = tasks::next_id();
            if let Ok(controller) = web_sys::AbortController::new() {
                // Registered under the pane so Ctrl-C also cancels
                // programmatic runs targeting it.
                let (fut, handle) = Abortable::wrap(eval_to_value(WasmAccess, pane, line, run_id));
                tasks::register(run_id, pane, handle, controller);
                let result = fut.await;
                tasks::finish(run_id);
                match result {
                    Ok(Ok(value)) => Ok(convert::value_to_js(&value)),
                    Ok(Err(err)) => {
                        let mut msg = err.msg.clone();
                        if let Some(help) = &err.help {
                            msg.push_str(&format!(" ({help})"));
                        }
                        Err(js_sys::Error::new(&msg).into())
                    }
                    Err(_aborted) => Err(js_sys::Error::new("aborted").into()),
                }
            } else {
                Err(js_sys::Error::new("AbortController unavailable").into())
            }
        })
    }

    /// Tear down the engine: abort all in-flight work, drop state, detach
    /// the event callback. Subsequent calls on this handle are no-ops.
    pub fn dispose(&self) {
        dispose_engine();
    }
}

/// Idempotent global teardown — same effect as `BtermCore::dispose()`.
/// Useful when a hot-reload cycle lost the handle but the singleton engine
/// is still alive.
#[wasm_bindgen]
pub fn dispose_engine() {
    tasks::abort_all();
    js_fn::clear();
    ON_EVENT.with(|c| *c.borrow_mut() = None);
    ENGINE.with(|c| *c.borrow_mut() = None);
}
