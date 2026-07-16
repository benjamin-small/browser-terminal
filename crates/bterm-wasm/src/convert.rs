//! Value ↔ JsValue conversion.
//!
//! Rust → JS goes through serde's json-compatible serializer (records become
//! plain objects, never `Map`). JS → Rust is a hand-written walk so that
//! integral JS numbers become `Int` (serde's untagged path would make every
//! number a `Float`).

use bterm_core::Value;
use serde::Serialize;
use wasm_bindgen::{JsCast, JsValue};

/// Largest integer exactly representable in an f64 (2^53), mirroring the
/// lexer's guard.
const MAX_SAFE: f64 = 9_007_199_254_740_992.0;

pub fn value_to_js(value: &Value) -> JsValue {
    let ser = serde_wasm_bindgen::Serializer::json_compatible();
    value.serialize(&ser).unwrap_or(JsValue::NULL)
}

pub fn js_to_value(v: &JsValue) -> Result<Value, String> {
    if v.is_null() || v.is_undefined() {
        return Ok(Value::Null);
    }
    if let Some(b) = v.as_bool() {
        return Ok(Value::Bool(b));
    }
    if let Some(n) = v.as_f64() {
        if n.fract() == 0.0 && n.abs() <= MAX_SAFE {
            return Ok(Value::Int(n as i64));
        }
        return Ok(Value::Float(n));
    }
    if let Some(s) = v.as_string() {
        return Ok(Value::Str(s));
    }
    if js_sys::Array::is_array(v) {
        let arr: &js_sys::Array = v.unchecked_ref();
        let mut items = Vec::with_capacity(arr.length() as usize);
        for item in arr.iter() {
            items.push(js_to_value(&item)?);
        }
        return Ok(Value::List(items));
    }
    if v.is_object() {
        let entries = js_sys::Object::entries(v.unchecked_ref());
        let mut pairs = Vec::with_capacity(entries.length() as usize);
        for entry in entries.iter() {
            let pair: js_sys::Array = entry.into();
            let key = pair.get(0).as_string().unwrap_or_default();
            pairs.push((key, js_to_value(&pair.get(1))?));
        }
        return Ok(Value::record(pairs));
    }
    Err(format!(
        "cannot convert a JS {} into a shell value",
        v.js_typeof().as_string().unwrap_or_else(|| "value".into())
    ))
}
