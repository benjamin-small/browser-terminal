//! Inline callables, compiled by the browser's own JavaScript engine, plus
//! the registry of functions the host page registered by name.
//!
//! Two paths, because one of them can be switched off by the page:
//!
//! * **Inline source** — `map '(o) => o.name'` — compiled with `new
//!   Function`. That is `eval`, so a page whose Content-Security-Policy
//!   omits `unsafe-eval` will refuse it. We detect that once and fail with
//!   a message that points at the alternative rather than a raw JS error.
//! * **Registered functions** — `map @slug`, registered from TypeScript via
//!   `registerFn`. No `eval`, so this works under any CSP, and the function
//!   stays type-checked and debuggable in devtools.

use crate::convert::{js_to_value, value_to_js};
use bterm_core::callable::{FnCompiler, HostFn};
use bterm_core::Value;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use wasm_bindgen::prelude::*;

thread_local! {
    /// Functions registered by name from TypeScript (`@name` selectors).
    static REGISTRY: RefCell<HashMap<String, js_sys::Function>> = RefCell::new(HashMap::new());
    /// `None` until probed: whether `new Function` is permitted by CSP.
    static EVAL_ALLOWED: RefCell<Option<bool>> = const { RefCell::new(None) };
}

// Both shims catch, because both can throw for reasons the user controls:
// a syntax error in the source, or a CSP that forbids eval entirely. An
// uncaught JS exception unwinding through wasm would abort the engine.
#[wasm_bindgen(inline_js = r#"
export function bterm_compile_fn(source) {
  try {
    // Wrapped in parens so a bare arrow function is an expression.
    const fn = (0, eval)("(" + source + ")");
    if (typeof fn !== "function") return "expression is not a function";
    return fn;
  } catch (e) {
    return String((e && e.message) || e);
  }
}
export function bterm_eval_allowed() {
  try { new Function("return 1"); return true; }
  catch (e) { return false; }
}
"#)]
extern "C" {
    #[wasm_bindgen(js_name = bterm_compile_fn)]
    fn compile_fn_js(source: &str) -> JsValue;
    #[wasm_bindgen(js_name = bterm_eval_allowed)]
    fn eval_allowed_js() -> bool;
}

fn eval_allowed() -> bool {
    EVAL_ALLOWED.with(|cell| {
        let mut slot = cell.borrow_mut();
        // CSP can't change for the page's lifetime, so probe once.
        *slot.get_or_insert_with(eval_allowed_js)
    })
}

/// Wraps a JS function so the engine can call it per pipeline item.
pub struct JsHostFn {
    func: js_sys::Function,
    /// Shown in errors: `(o) => …` or `@name`.
    label: String,
}

impl HostFn for JsHostFn {
    fn call(&self, value: &Value) -> Result<Value, String> {
        let arg = value_to_js(value);
        let returned = self
            .func
            .call1(&JsValue::NULL, &arg)
            .map_err(|e| format!("{} threw: {}", self.label, js_error_text(&e)))?;
        js_to_value(&returned).map_err(|msg| format!("{}: {msg}", self.label))
    }
}

fn js_error_text(e: &JsValue) -> String {
    if let Some(msg) = js_sys::Reflect::get(e, &JsValue::from_str("message"))
        .ok()
        .and_then(|m| m.as_string())
    {
        return msg;
    }
    e.as_string().unwrap_or_else(|| "unknown error".into())
}

pub struct JsFnCompiler;

impl FnCompiler for JsFnCompiler {
    fn compile(&self, source: &str) -> Result<Rc<dyn HostFn>, String> {
        if !eval_allowed() {
            return Err(format!(
                "this page's Content-Security-Policy forbids `eval`, so inline \
                 functions cannot be compiled. Register the function from \
                 TypeScript instead — bt.registerFn('name', {source}) — and use `@name`"
            ));
        }
        let result = compile_fn_js(source);
        // A string return means the shim caught something.
        match result.as_string() {
            Some(err) => Err(err),
            None => Ok(Rc::new(JsHostFn {
                func: result.unchecked_into::<js_sys::Function>(),
                label: format!("`{source}`"),
            })),
        }
    }

    fn dialect(&self) -> &'static str {
        "javascript"
    }
}

/// Register a named function from TypeScript. Replacing an existing name is
/// allowed — the same hot-reload behavior as `register_command`.
pub fn register(name: &str, func: js_sys::Function) {
    REGISTRY.with(|r| {
        r.borrow_mut().insert(name.to_string(), func);
    });
}

pub fn unregister(name: &str) {
    REGISTRY.with(|r| {
        r.borrow_mut().remove(name);
    });
}

pub fn clear() {
    REGISTRY.with(|r| r.borrow_mut().clear());
}

/// Resolve `@name`, with a did-you-mean over the registered names.
pub fn lookup(name: &str) -> Result<Rc<dyn HostFn>, String> {
    REGISTRY.with(|r| {
        let map = r.borrow();
        match map.get(name) {
            Some(func) => Ok(Rc::new(JsHostFn {
                func: func.clone(),
                label: format!("`@{name}`"),
            }) as Rc<dyn HostFn>),
            None => {
                let known: Vec<&str> = map.keys().map(|s| s.as_str()).collect();
                let suggestion = bterm_core::error::did_you_mean(name, known.iter().copied());
                Err(match suggestion {
                    Some(s) => format!("no registered function `@{name}` — did you mean `@{s}`?"),
                    None if known.is_empty() => format!(
                        "no registered function `@{name}`; register one with bt.registerFn(name, fn)"
                    ),
                    None => format!(
                        "no registered function `@{name}`; registered: {}",
                        known.join(", ")
                    ),
                })
            }
        }
    })
}
