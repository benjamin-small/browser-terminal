//! `grep`'s pattern engine in the browser: JavaScript's native `RegExp`.
//!
//! The engine is already in every JS runtime, so this buys full regex —
//! including lookaround and backreferences, which Rust's `regex` crate
//! deliberately omits — for zero added binary size. `RegExp` is ECMAScript
//! (ES3, 1999); anything new enough to instantiate our wasm has it.

use bterm_core::matcher::{Pattern, PatternMatcher};
use js_sys::RegExp;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;

// `new RegExp(...)` throws a SyntaxError on a bad pattern. Letting a JS
// exception unwind through wasm would abort the engine (panic = abort), so
// compilation is funnelled through this try/catch shim: it returns a RegExp
// on success and an error *string* on failure — the two are never confusable.
#[wasm_bindgen(inline_js = r#"
export function bterm_compile_regex(pattern, flags) {
  try { return new RegExp(pattern, flags); }
  catch (e) { return String((e && e.message) || e); }
}
"#)]
extern "C" {
    #[wasm_bindgen(js_name = bterm_compile_regex)]
    fn compile_regex(pattern: &str, flags: &str) -> JsValue;
}

pub struct JsRegexMatcher;

struct JsRegexPattern {
    re: RegExp,
}

impl Pattern for JsRegexPattern {
    fn is_match(&self, text: &str) -> bool {
        // No `g` flag is ever set, so `test` keeps no `lastIndex` state and
        // this stays a pure predicate across calls.
        self.re.test(text)
    }
}

impl PatternMatcher for JsRegexMatcher {
    fn compile(&self, pattern: &str, case_insensitive: bool) -> Result<Box<dyn Pattern>, String> {
        let flags = if case_insensitive { "i" } else { "" };
        let result = compile_regex(pattern, flags);
        match result.as_string() {
            // A string return means the shim caught a SyntaxError.
            Some(err) => Err(err),
            None => Ok(Box::new(JsRegexPattern {
                re: result.unchecked_into::<RegExp>(),
            })),
        }
    }

    fn dialect(&self) -> &'static str {
        "regex"
    }
}
