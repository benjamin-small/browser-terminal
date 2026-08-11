//! The wasm-bindgen boundary crate — the only crate that touches
//! wasm-bindgen/js-sys.
//!
//! Concurrency contract: engine state lives in a `thread_local` and is only
//! reachable through `WasmAccess::with`, a synchronous closure — no borrow
//! can cross an await, and no `&mut self` exports exist. Events queue inside
//! the borrow and flush to the JS callback only after it drops, so a JS
//! handler that synchronously calls back into the engine cannot
//! double-borrow. For the same reason, conversion to and from JS happens
//! outside the borrow and never inside it: `js_to_value` walks property
//! getters that can run host code, and that code calling back into the engine
//! would find the `ENGINE` cell already borrowed — which `&self` exports do
//! nothing to prevent. Every submitted pipeline runs inside an `Abortable` whose
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

/// Diagnostics come back as a plain JS string array, so a caller can print,
/// ignore, or surface them without touching the terminal.
fn string_array(lines: &[String]) -> js_sys::Array {
    lines.iter().map(|s| JsValue::from_str(s)).collect()
}

/// Build `run()`'s rejection: a real `Error` carrying whatever the pipeline
/// wrote before it failed.
///
/// A failure is exactly when the log leading up to it matters most, so
/// rejecting with only a message throws away the useful part. It stays an
/// `Error` rather than becoming a resolved `{ error }` object because a
/// promise that resolves on failure invites callers to miss it — `await`
/// with no check would silently continue.
fn run_rejection(msg: &str, sink: &bterm_core::sink::CollectingSink) -> JsValue {
    let e = js_sys::Error::new(msg);
    let _ = js_sys::Reflect::set(&e, &JsValue::from_str("log"), &string_array(&sink.log_lines()));
    let _ = js_sys::Reflect::set(&e, &JsValue::from_str("err"), &string_array(&sink.err_lines()));
    e.into()
}

/// First-paint deadline for a pane's progressive table: if a slow source
/// hasn't produced `PROBE_ROWS` (bterm-core's row-count bound) by this many
/// milliseconds after the run started, we commit whatever it has buffered
/// so far rather than leaving the pane blank. bterm-core has no clock, so
/// this timing decision -- and the `setTimeout` that carries it out --
/// lives here.
const PROBE_DEADLINE_MS: i32 = 150;

/// Arm the probe deadline for a run: after `PROBE_DEADLINE_MS`, force
/// whatever that pane's progressive renderer has buffered to paint.
fn arm_probe_deadline(run_id: u64, pane: u32) {
    schedule_probe_check(run_id, pane);
}

/// Schedule one probe-deadline check `PROBE_DEADLINE_MS` out. Recorded in
/// the task registry so `finish`/abort clears it before it can fire late
/// against a pane that has moved on to a different run.
///
/// A source's first row can itself arrive after the deadline (an `await`
/// before the first `yield`, say) — at that moment there is nothing
/// buffered yet, so `commit_pending_render` finds nothing to paint and a
/// single one-shot check would silently give up, leaving the pane blank
/// until the stream ends on its own. So a check that finds nothing pending
/// reschedules itself, rather than assuming "nothing yet" means "nothing
/// ever" — it keeps trying every `PROBE_DEADLINE_MS` until either something
/// paints, the renderer settles some other way (reaching `PROBE_ROWS`), or
/// the run itself ends (which cancels the pending timer via `tasks`).
fn schedule_probe_check(run_id: u64, pane: u32) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let cb = Closure::once_into_js(move || {
        if !engine_alive() || !tasks::is_active(run_id) {
            return;
        }
        let text = WasmAccess.with(|e| e.commit_pending_render(pane, run_id));
        match text {
            Some(text) => {
                WasmAccess.with(|e| e.emit_output(pane, &text));
                flush_events();
            }
            None => {
                if !WasmAccess.with(|e| e.probe_settled(pane, run_id)) {
                    schedule_probe_check(run_id, pane);
                }
            }
        }
    });
    let armed = window.set_timeout_with_callback_and_timeout_and_arguments_0(
        cb.as_ref().unchecked_ref(),
        PROBE_DEADLINE_MS,
    );
    // `once_into_js` hands the closure to JS rather than leaking it, so a
    // long session submitting many lines doesn't accumulate one per run.
    // Clearing the timeout on finish/abort cancels the *timer*; it is JS
    // that reclaims the callback.
    if let Ok(id) = armed {
        tasks::set_probe_timeout(run_id, id);
    }
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
    arm_probe_deadline(run_id, pane);
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

/// Which layer a variable call targets.
enum VarTarget {
    Host,
    Session(u32),
}

/// Read the optional `{ scope, session }` argument.
///
/// Absent means host, so every call written before session scope existed
/// keeps working. An unrecognised `scope` is an error rather than a
/// fallback to host: a typo that silently wrote to the wrong layer would
/// be found later, by someone reading a value that was never set where
/// they looked.
///
/// Pure JS work — call it before opening an engine borrow.
fn var_target(opts: &JsValue) -> Result<VarTarget, JsValue> {
    if opts.is_undefined() || opts.is_null() {
        return Ok(VarTarget::Host);
    }
    let scope = js_sys::Reflect::get(opts, &JsValue::from_str("scope"))
        .ok()
        .and_then(|v| v.as_string());
    match scope.as_deref() {
        None | Some("host") => Ok(VarTarget::Host),
        Some("session") => {
            let id = js_sys::Reflect::get(opts, &JsValue::from_str("session"))
                .ok()
                .and_then(|v| v.as_f64())
                .ok_or_else(|| {
                    JsValue::from_str(
                        "{ scope: 'session' } needs a `session` id from snapshot.sessions",
                    )
                })?;
            Ok(VarTarget::Session(id as u32))
        }
        Some(other) => Err(JsValue::from_str(&format!(
            "unknown scope `{other}`: expected 'host' or 'session'"
        ))),
    }
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

    /// Inject a value the shell resolves as `$name`, for every session and
    /// every pane, including ones created later.
    ///
    /// The optional `opts` names the layer: omitted (or `{ scope: 'host' }`)
    /// writes the engine-wide one, `{ scope: 'session', session: id }` writes
    /// a single session's, which shadows it there.
    ///
    /// Throws on a name the shell could not reference. The value crosses as
    /// a typed `Value` and is never parsed as shell source, so a string
    /// containing `; rm -rf /` is that string and cannot become syntax.
    ///
    /// Takes effect from the next command line: a pipeline already running
    /// keeps the values it started with.
    pub fn set_variable(&self, name: &str, value: JsValue, opts: JsValue) -> Result<(), JsValue> {
        // Guard first, like every other engine-touching export: reaching
        // `WasmAccess::with` on a disposed engine panics, and with
        // `panic = "abort"` that kills the module rather than throwing
        // something a caller could catch.
        if !engine_alive() {
            return Err(JsValue::from_str("browser-terminal: engine is disposed"));
        }
        // Both conversions are JS work and happen before the borrow opens.
        let target = var_target(&opts)?;
        let converted = convert::js_to_value(&value).map_err(|e| JsValue::from_str(&e))?;
        WasmAccess
            .with(|e| match target {
                VarTarget::Host => e.set_host_var(name, converted),
                VarTarget::Session(id) => e.set_session_var(id, name, converted),
            })
            .map_err(|err| JsValue::from_str(&err.msg))
    }

    /// Set several variables at once. Names not mentioned are left alone —
    /// this merges into what is already injected rather than replacing it;
    /// use `unsetVariable` to remove one.
    ///
    /// Every name is validated and every value converted before anything is
    /// applied, so one bad entry leaves the previous state untouched. A
    /// partial apply is the worst outcome for a host synchronising editor
    /// state: some values land, one stays stale, and the next command runs
    /// against a mix that looks plausible.
    ///
    /// `opts` names the layer, exactly as it does for `setVariable`; the
    /// whole batch lands in one layer.
    pub fn set_variables(&self, values: JsValue, opts: JsValue) -> Result<(), JsValue> {
        if !engine_alive() {
            return Err(JsValue::from_str("browser-terminal: engine is disposed"));
        }
        // Parsed before the borrow opens, like every other JS read here.
        let target = var_target(&opts)?;
        // An array passes `dyn_into::<Object>()` and its indices stringify
        // into legal variable names, so without this a host that meant to
        // pass a record would silently get `$0`, `$1`, … and no error.
        if js_sys::Array::is_array(&values) {
            return Err(JsValue::from_str(
                "setVariables expects an object of name → value, not an array",
            ));
        }
        let obj: js_sys::Object = values
            .dyn_into()
            .map_err(|_| JsValue::from_str("setVariables expects an object"))?;

        // Convert and validate everything first — still outside any borrow.
        // Both halves have to happen up front: validating names but
        // converting as you apply leaves an unconvertible value half-way
        // through the batch, which is the same broken state by a different
        // route (`set_variables_applies_all_or_nothing` covers both).
        let mut pending: Vec<(String, bterm_core::value::Value)> = Vec::new();
        for entry in js_sys::Object::entries(&obj).iter() {
            let pair: js_sys::Array = entry.into();
            let name = pair.get(0).as_string().unwrap_or_default();
            if !bterm_core::lex::is_valid_var_name(&name) {
                return Err(JsValue::from_str(&format!(
                    "`{name}` is not a valid variable name: use letters, digits and `_`"
                )));
            }
            // Name the key: a batch can be large, and "cannot convert a JS
            // function" on its own leaves the host hunting for which one.
            let value = convert::js_to_value(&pair.get(1))
                .map_err(|e| JsValue::from_str(&format!("`{name}`: {e}")))?;
            pending.push((name, value));
        }

        let applied: Result<(), bterm_core::error::ShellError> = WasmAccess.with(|e| {
            // An unknown session id has to fail the batch whole, like a bad
            // name does — so the target is checked before anything lands,
            // not discovered part-way through the loop.
            if let VarTarget::Session(id) = target {
                e.session_vars(id)?;
            }
            for (name, value) in pending {
                // Names are already validated above and the session id just
                // above that; the engine re-checks and both errors are
                // unreachable here, so drop them rather than unwrap.
                match target {
                    VarTarget::Host => {
                        let _ = e.set_host_var(&name, value);
                    }
                    VarTarget::Session(id) => {
                        let _ = e.set_session_var(id, &name, value);
                    }
                }
            }
            Ok(())
        });
        applied.map_err(|err| JsValue::from_str(&err.msg))
    }

    /// Remove an injected variable from the layer `opts` names. Returns
    /// whether it was set — and false on a disposed engine, where nothing is
    /// set by definition.
    pub fn unset_variable(&self, name: &str, opts: JsValue) -> bool {
        if !engine_alive() {
            return false;
        }
        // A malformed `opts` removes nothing: `-> bool` has nowhere to put an
        // error, and reporting `true` for a target that was never touched
        // would be worse than reporting `false`.
        let Ok(target) = var_target(&opts) else {
            return false;
        };
        WasmAccess.with(|e| match target {
            VarTarget::Host => e.unset_host_var(name),
            // The one place a bad session id is not an error: `-> bool` has
            // nowhere to put one, and a name that is not set in a session
            // that does not exist is, truthfully, not set. The inconsistency
            // with the other four is deliberate — do not "fix" it by
            // widening the return type.
            VarTarget::Session(id) => e.unset_session_var(id, name).unwrap_or(false),
        })
    }

    /// The value of an injected variable in the layer `opts` names, or
    /// `undefined` if it is not set there.
    ///
    /// `undefined` means exactly that one thing. It used to also mean "the
    /// session id names no session" and "the engine is disposed", and a host
    /// could not tell the three apart; both of those now throw, like the
    /// setters already did.
    ///
    /// One layer, never a merged view: a read that silently fell through to
    /// the host value would answer a question the caller did not ask.
    ///
    /// `undefined` rather than null: a host can legitimately inject null,
    /// and the two must stay distinguishable.
    pub fn get_variable(&self, name: &str, opts: JsValue) -> Result<JsValue, JsValue> {
        // Same guard as the setters: reaching `WasmAccess::with` on a
        // disposed engine panics, and `panic = "abort"` makes that fatal
        // rather than catchable.
        if !engine_alive() {
            return Err(JsValue::from_str("browser-terminal: engine is disposed"));
        }
        // JS work, so before the borrow opens.
        let target = var_target(&opts)?;
        let found = WasmAccess
            .with(|e| match target {
                VarTarget::Host => Ok(e.host_var(name).cloned()),
                VarTarget::Session(id) => e.session_var(id, name).map(|v| v.cloned()),
            })
            .map_err(|err| JsValue::from_str(&err.msg))?;
        // The conversion is a JS call, so it happens after the borrow closes.
        Ok(match found {
            Some(v) => convert::value_to_js(&v),
            None => JsValue::UNDEFINED,
        })
    }

    /// Everything injected into the layer `opts` names, as a plain object.
    ///
    /// Throws if the session id names no session, or the engine is disposed.
    /// It used to answer `undefined` in both cases, which made the wrapper's
    /// declared `Record<string, Value>` a lie in exactly the case a caller
    /// was most likely to reach by accident — a stale id.
    ///
    /// One layer, never a merged view: with `opts` omitted this is the host
    /// scope alone, and with `{ scope: 'session', session: id }` it is that
    /// session's own, never the two folded together. The shell's `vars`
    /// command shows the merged view instead, because it answers a different
    /// question — what `$name` resolves to here.
    ///
    /// Every name passed to `setVariables` appears here, alongside anything
    /// injected earlier and not unset. Values are what the shell holds, not
    /// the objects handed in: they have been through the same conversion
    /// command arguments take, so `undefined` reads back as `null`.
    ///
    /// Keys come back sorted. The underlying `Scope` is a `HashMap`, whose
    /// iteration order varies run to run, so without this a host doing
    /// `JSON.stringify(bt.variables())` would see the same state serialize
    /// differently each time — enough to churn a snapshot test or a diff
    /// for no reason. The `vars` builtin sorts for the same reason; this
    /// keeps the programmatic view as predictable as the shell one.
    pub fn variables(&self, opts: JsValue) -> Result<JsValue, JsValue> {
        if !engine_alive() {
            return Err(JsValue::from_str("browser-terminal: engine is disposed"));
        }
        let target = var_target(&opts)?;
        // Pairs are collected out of the borrow first: building the object
        // needs `Reflect::set` and `value_to_js`, both JS calls, and a JS
        // call inside an engine borrow risks a `RefCell` double-borrow that
        // `panic = "abort"` turns into a dead module.
        let mut pairs = WasmAccess
            .with(|e| match target {
                VarTarget::Host => Ok(e
                    .host_vars()
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect::<Vec<_>>()),
                // An id naming no session is an error, not an empty object:
                // an empty object would claim the session exists and simply
                // holds nothing.
                VarTarget::Session(id) => e.session_vars(id).map(|s| {
                    s.iter()
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect::<Vec<_>>()
                }),
            })
            .map_err(|err| JsValue::from_str(&err.msg))?;
        pairs.sort_by(|a, b| a.0.cmp(&b.0));
        let obj = js_sys::Object::new();
        for (name, value) in pairs {
            let _ = js_sys::Reflect::set(
                &obj,
                &JsValue::from_str(&name),
                &convert::value_to_js(&value),
            );
        }
        Ok(obj.into())
    }

    /// Programmatic execution: evaluate a line in a pane's context and
    /// resolve with `{ value, log, err }` — the final structured value
    /// plus whatever the pipeline wrote to its two diagnostic channels (no
    /// prompt echo, no pane render, nothing written to the terminal).
    ///
    /// Rejects with an `Error` whose message is the shell error and which
    /// also carries `log` and `err` — a failed run keeps the diagnostics it
    /// wrote on the way down.
    ///
    /// Cancellable from the moment it returns: the run is in the task
    /// registry before the promise reaches the caller, so a Ctrl-C in the
    /// same tick rejects it with `aborted`.
    pub fn run(&self, pane: u32, line: String) -> js_sys::Promise {
        if !engine_alive() {
            return js_sys::Promise::reject(&JsValue::from_str(
                "browser-terminal: engine is disposed",
            ));
        }
        let run_id = tasks::next_id();
        let Ok(controller) = web_sys::AbortController::new() else {
            let e = js_sys::Error::new("AbortController unavailable");
            return js_sys::Promise::reject(&e.into());
        };
        // Registered under the pane so Ctrl-C also cancels programmatic runs
        // targeting it — and registered *here*, synchronously, so that holds
        // from the moment the promise exists rather than from whenever
        // `future_to_promise` first polls the task (a microtask later). A
        // page that wires a cancel button, or a caller that feeds `\x03` on
        // the next statement, interrupts in this same tick: registering
        // inside the async block would leave that Ctrl-C nothing to find,
        // and the promise would never settle.
        //
        // An abort that lands in the gap is still honoured, and cheaply:
        // `Abortable::poll` checks the flag before it polls the inner
        // future, so the task's first poll settles it `Err(Aborted)` without
        // `eval_to_value` running at all.
        let sink = Rc::new(bterm_core::sink::CollectingSink::new());
        let (fut, handle) =
            Abortable::wrap(eval_to_value(WasmAccess, pane, line, run_id, sink.clone()));
        tasks::register(run_id, pane, handle, controller);
        future_to_promise(async move {
            let result = fut.await;
            tasks::finish(run_id);
            match result {
                Ok(Ok(value)) => {
                    let out = js_sys::Object::new();
                    let _ = js_sys::Reflect::set(
                        &out,
                        &JsValue::from_str("value"),
                        &convert::value_to_js(&value),
                    );
                    let _ = js_sys::Reflect::set(
                        &out,
                        &JsValue::from_str("log"),
                        &string_array(&sink.log_lines()),
                    );
                    let _ = js_sys::Reflect::set(
                        &out,
                        &JsValue::from_str("err"),
                        &string_array(&sink.err_lines()),
                    );
                    Ok(out.into())
                }
                Ok(Err(err)) => {
                    let mut msg = err.msg.clone();
                    if let Some(help) = &err.help {
                        msg.push_str(&format!(" ({help})"));
                    }
                    Err(run_rejection(&msg, &sink))
                }
                // Ctrl-C keeps what the command managed to print — the
                // partial log is usually why you interrupted it. A run
                // aborted before its first poll printed nothing, so this is
                // the same rejection with both channels empty.
                Err(_aborted) => Err(run_rejection("aborted", &sink)),
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
