//! Boundary tests: Value ↔ JS conversion, JsCommand invocation, collision
//! policy, cancellation. Run with:
//!   cargo test -p bterm-wasm --target wasm32-unknown-unknown
//! (wasm-bindgen-test-runner under Node; no DOM needed.)

#![cfg(target_arch = "wasm32")]

use bterm_wasm::BtermCore;
use js_sys::{Array, Function, Reflect};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::*;

fn event_collector() -> Function {
    Function::new_with_args(
        "ev",
        "globalThis.__events = globalThis.__events || []; globalThis.__events.push(ev);",
    )
}

fn events() -> Array {
    Reflect::get(&js_sys::global(), &"__events".into())
        .ok()
        .and_then(|v| v.dyn_into::<Array>().ok())
        .unwrap_or_default()
}

fn make_core() -> BtermCore {
    // Tests share one wasm instance; reset the singleton and event sink so
    // one test's failure can't cascade into the others.
    bterm_wasm::dispose_engine();
    let _ = Reflect::set(&js_sys::global(), &"__events".into(), &Array::new());
    BtermCore::new(event_collector()).expect("engine created")
}

async fn run_line(core: &BtermCore, line: &str) -> Result<JsValue, JsValue> {
    JsFuture::from(core.run(0, line.to_string())).await
}

#[wasm_bindgen_test]
async fn run_resolves_scalar_and_plain_objects() {
    let core = make_core();
    let v = run_line(&core, "echo 5").await.expect("resolves");
    assert_eq!(v.as_f64(), Some(5.0));

    // Records must arrive as plain objects (never Map).
    let v = run_line(&core, "echo '{\"a\":1}' | from json").await.expect("resolves");
    assert!(v.is_object());
    assert!(!v.is_instance_of::<js_sys::Map>());
    let a = Reflect::get(&v, &"a".into()).expect("field a");
    assert_eq!(a.as_f64(), Some(1.0));
    core.dispose();
}

#[wasm_bindgen_test]
async fn ts_command_sync_return_and_int_conversion() {
    let core = make_core();
    let sig = js_sys::JSON::parse(r#"{"name":"answer","summary":"the answer"}"#).expect("sig");
    let f = Function::new_with_args("args, input, ctx", "return 42;");
    core.register_command(sig, f).expect("registered");
    let v = run_line(&core, "answer").await.expect("resolves");
    assert_eq!(v.as_f64(), Some(42.0));

    // Integral JS numbers become Int — usable by first/last.
    let sig = js_sys::JSON::parse(r#"{"name":"nums"}"#).expect("sig");
    let f = Function::new_with_args("args", "return [10, 20, 30];");
    core.register_command(sig, f).expect("registered");
    let v = run_line(&core, "nums | head 2 | length").await.expect("resolves");
    assert_eq!(v.as_f64(), Some(2.0));
    core.dispose();
}

#[wasm_bindgen_test]
async fn ts_command_async_and_args_shape() {
    let core = make_core();
    let sig = js_sys::JSON::parse(
        r#"{"name":"shape","flags":[{"long":"limit","shape":"int"}],"rest":{"name":"rest"}}"#,
    )
    .expect("sig");
    let f = Function::new_with_args(
        "args, input, ctx",
        "return Promise.resolve({ nPos: args.positionals.length, limit: args.flags.limit ?? null, hasEmit: typeof ctx.emit === 'function', hasSignal: ctx.signal instanceof AbortSignal });",
    );
    core.register_command(sig, f).expect("registered");
    let v = run_line(&core, "shape a b --limit 7").await.expect("resolves");
    assert_eq!(Reflect::get(&v, &"nPos".into()).expect("nPos").as_f64(), Some(2.0));
    assert_eq!(Reflect::get(&v, &"limit".into()).expect("limit").as_f64(), Some(7.0));
    assert_eq!(Reflect::get(&v, &"hasEmit".into()).expect("hasEmit").as_bool(), Some(true));
    assert_eq!(Reflect::get(&v, &"hasSignal".into()).expect("hasSignal").as_bool(), Some(true));
    core.dispose();
}

#[wasm_bindgen_test]
async fn grep_uses_real_regex_in_the_browser() {
    let core = make_core();
    let sig = js_sys::JSON::parse(r#"{"name":"rows"}"#).expect("sig");
    let f = Function::new_with_args(
        "args",
        r#"return [{t:"Rust lang"},{t:"WebAssembly"},{t:"rust book"}];"#,
    );
    core.register_command(sig, f).expect("registered");

    // Anchors: only "Rust lang" starts with capital R-u-s-t.
    let v = run_line(&core, "rows | grep '^Rust' | length").await.expect("resolves");
    assert_eq!(v.as_f64(), Some(1.0), "^ anchor is regex, not a literal");

    // Alternation + case-insensitive.
    let v = run_line(&core, "rows | grep 'rust|assembly' -i | length").await.expect("resolves");
    assert_eq!(v.as_f64(), Some(3.0), "| is alternation");

    // Character class + quantifier.
    let v = run_line(&core, r#"rows | grep '[A-Z][a-z]+As' | length"#).await.expect("resolves");
    assert_eq!(v.as_f64(), Some(1.0));

    // Invert still composes with regex.
    let v = run_line(&core, "rows | grep '^Rust' -v | length").await.expect("resolves");
    assert_eq!(v.as_f64(), Some(2.0));
    core.dispose();
}

#[wasm_bindgen_test]
async fn grep_invalid_regex_is_a_clean_error_not_a_crash() {
    let core = make_core();
    // Unterminated group: RegExp throws SyntaxError; must surface as a shell
    // error, and the engine must stay alive afterwards.
    let err = run_line(&core, "echo abc | grep '('").await.expect_err("rejects");
    let msg = Reflect::get(&err, &"message".into())
        .ok()
        .and_then(|m| m.as_string())
        .unwrap_or_default();
    assert!(msg.contains("invalid regex pattern"), "message: {msg}");

    let v = run_line(&core, "echo 5").await.expect("engine still alive");
    assert_eq!(v.as_f64(), Some(5.0));
    core.dispose();
}

#[wasm_bindgen_test]
async fn ts_rejection_and_rich_error() {
    let core = make_core();
    let sig = js_sys::JSON::parse(r#"{"name":"boom"}"#).expect("sig");
    let f = Function::new_with_args(
        "args, input, ctx",
        "throw { message: 'kaboom', help: 'try not exploding' };",
    );
    core.register_command(sig, f).expect("registered");
    let err = run_line(&core, "boom").await.expect_err("rejects");
    let msg = Reflect::get(&err, &"message".into())
        .ok()
        .and_then(|m| m.as_string())
        .unwrap_or_default();
    assert!(msg.contains("kaboom"), "message: {msg}");
    assert!(msg.contains("try not exploding"), "help folded in: {msg}");
    core.dispose();
}

#[wasm_bindgen_test]
async fn builtin_collision_rejected_and_replace_allowed() {
    let core = make_core();
    let f = Function::new_with_args("args", "return 1;");
    let sig = js_sys::JSON::parse(r#"{"name":"echo"}"#).expect("sig");
    assert!(core.register_command(sig, f.clone()).is_err(), "builtin name must be rejected");

    let sig1 = js_sys::JSON::parse(r#"{"name":"mine"}"#).expect("sig");
    core.register_command(sig1, Function::new_with_args("a", "return 1;")).expect("first ok");
    let sig2 = js_sys::JSON::parse(r#"{"name":"mine"}"#).expect("sig");
    core.register_command(sig2, Function::new_with_args("a", "return 2;")).expect("replace ok");
    let v = run_line(&core, "mine").await.expect("resolves");
    assert_eq!(v.as_f64(), Some(2.0), "replacement wins");

    core.unregister_command("mine");
    assert!(run_line(&core, "mine").await.is_err(), "unregistered");
    core.dispose();
}

#[wasm_bindgen_test]
async fn signature_typo_errors_loudly() {
    let core = make_core();
    // "flag" (typo for "flags") must be rejected, not silently ignored.
    let sig = js_sys::JSON::parse(r#"{"name":"typo","flag":[{"long":"x"}]}"#).expect("sig");
    let err = core
        .register_command(sig, Function::new_with_args("a", "return 1;"))
        .expect_err("unknown field must error");
    let msg = err.as_string().unwrap_or_default();
    assert!(msg.contains("invalid command signature"), "{msg}");
    core.dispose();
}

#[wasm_bindgen_test]
async fn feed_emits_pane_output_events() {
    let core = make_core();
    let before = events().length();
    core.feed(0, "echo hi");
    core.feed(0, "\r");
    // Wait a macrotask so the spawned pipeline completes and flushes.
    let p = js_sys::Promise::new(&mut |resolve, _| {
        let f = Function::new_with_args("r", "setTimeout(r, 30);");
        let _ = f.call1(&JsValue::NULL, &resolve);
    });
    JsFuture::from(p).await.expect("timer");
    assert!(events().length() > before, "paneOutput events flushed");
    core.dispose();
}

#[wasm_bindgen_test]
async fn abort_signal_fires_on_ctrl_c() {
    let core = make_core();
    let sig = js_sys::JSON::parse(r#"{"name":"hang"}"#).expect("sig");
    let f = Function::new_with_args(
        "args, input, ctx",
        "return new Promise((res, rej) => { ctx.signal.addEventListener('abort', () => { globalThis.__aborted = true; rej(new Error('aborted')); }); });",
    );
    core.register_command(sig, f).expect("registered");
    let _ = Reflect::set(&js_sys::global(), &"__aborted".into(), &JsValue::FALSE);

    let pending = core.run(0, "hang".to_string());
    core.feed(0, "\x03"); // Ctrl-C aborts pane 0's runs
    let err = JsFuture::from(pending).await.expect_err("aborted run rejects");
    let msg = Reflect::get(&err, &"message".into())
        .ok()
        .and_then(|m| m.as_string())
        .unwrap_or_default();
    assert!(msg.contains("aborted"), "{msg}");
    let aborted = Reflect::get(&js_sys::global(), &"__aborted".into())
        .expect("flag")
        .as_bool()
        .unwrap_or(false);
    assert!(aborted, "TS command observed the AbortSignal");
    core.dispose();
}
