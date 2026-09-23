//! Value ↔ JsValue conversion.
//!
//! Rust → JS preserves bytes as Uint8Array, including inside lists and records
//! (plain objects, never Map). JS → Rust is a hand-written walk so that
//! integral JS numbers become `Int` (serde's untagged path would make every
//! number a `Float`).

use bterm_core::Value;
use serde::Serialize;
use wasm_bindgen::{JsCast, JsValue};

/// Largest integer exactly representable in an f64 (2^53), mirroring the
/// lexer's guard.
const MAX_SAFE: f64 = 9_007_199_254_740_992.0;

pub fn value_to_js(value: &Value) -> JsValue {
    match value {
        // Copy into JS-owned memory; callers must not alias the WASM heap.
        Value::Bytes(bytes) => js_sys::Uint8Array::from(bytes.as_slice()).into(),
        Value::List(items) => items
            .iter()
            .map(value_to_js)
            .collect::<js_sys::Array>()
            .into(),
        Value::Record(fields) => {
            let entries = js_sys::Array::new();
            for (key, value) in fields {
                let pair = js_sys::Array::new();
                pair.push(&key.into());
                pair.push(&value_to_js(value));
                entries.push(&pair);
            }
            // fromEntries preserves keys such as __proto__ as own data properties.
            js_sys::Object::from_entries(&entries)
                .map(JsValue::from)
                .unwrap_or(JsValue::NULL)
        }
        _ => value
            .serialize(&serde_wasm_bindgen::Serializer::json_compatible())
            .unwrap_or(JsValue::NULL),
    }
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
    if let Some(bytes) = v.dyn_ref::<js_sys::Uint8Array>() {
        // to_vec respects subarray offsets and copies the bytes into Rust.
        return Ok(Value::Bytes(bytes.to_vec()));
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
