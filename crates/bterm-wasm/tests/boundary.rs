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

/// Run a line and hand back the whole `RunResult` — `{ value, log, err }` —
/// for the tests that care about the diagnostic channels. Rejections pass
/// through as the `RunError` itself.
async fn run_line(core: &BtermCore, line: &str) -> Result<JsValue, JsValue> {
    JsFuture::from(core.run(0, line.to_string())).await
}

/// Run a line and hand back channel 1 alone: the pipeline's final value.
///
/// Most tests here are about what a pipeline *computes*, so they want the
/// value and not the envelope it arrives in. Going through `run_line` and
/// reaching past the wrapper — rather than asserting on the resolved object
/// directly — is what keeps `assert_eq!(v.as_f64(), Some(5.0))` honest: on
/// the wrapper it reads `None` and fails for a reason that has nothing to do
/// with the pipeline. The `has` check makes a future change to `RunResult`'s
/// shape say so outright instead of quietly handing back `undefined`.
async fn run_value(core: &BtermCore, line: &str) -> Result<JsValue, JsValue> {
    let out = run_line(core, line).await?;
    assert!(
        Reflect::has(&out, &"value".into()).unwrap_or(false),
        "run() must resolve with a RunResult ({{ value, log, err }})"
    );
    Ok(Reflect::get(&out, &"value".into()).expect("RunResult.value"))
}

/// The `log`/`err` entries carried by a `RunResult` or a `RunError`.
fn entries(v: &JsValue, key: &str) -> Vec<String> {
    Reflect::get(v, &key.into())
        .ok()
        .and_then(|a| a.dyn_into::<Array>().ok())
        .map(|a| a.iter().filter_map(|e| e.as_string()).collect())
        .unwrap_or_default()
}

fn contains(entries: &[String], needle: &str) -> bool {
    entries.iter().any(|e| e.contains(needle))
}

/// A resolved list of strings, for assertions that name *which* rows survived
/// a stage rather than only counting them.
fn strings(v: &JsValue) -> Vec<String> {
    v.clone()
        .dyn_into::<Array>()
        .map(|a| a.iter().filter_map(|e| e.as_string()).collect())
        .unwrap_or_default()
}

/// Hand control back to the event loop for one macrotask.
async fn tick() {
    let p = js_sys::Promise::new(&mut |resolve, _| {
        let f = Function::new_with_args("r", "setTimeout(r, 0);");
        let _ = f.call1(&JsValue::NULL, &resolve);
    });
    let _ = JsFuture::from(p).await;
}

/// Sentinel resolution value for `within_a_second` — a run never produces it.
const NEVER_SETTLED: &str = "__bterm_never_settled";

/// Await `p`, yielding `None` if it is still pending a second from now.
///
/// Awaiting a promise that never settles does not fail a `wasm_bindgen_test`;
/// it empties Node's event loop, and the runner then exits *silently*,
/// reporting nothing and skipping every test after this one. A cancellation
/// bug whose symptom is "the promise never settles" would therefore hide
/// itself. Racing a timer turns that into an ordinary assertion failure.
///
/// Costs nothing when the promise does settle: `race` resolves as soon as it
/// does, and the timer is left to expire unobserved.
async fn within_a_second(p: js_sys::Promise) -> Option<Result<JsValue, JsValue>> {
    let timeout = js_sys::Promise::new(&mut |resolve, _| {
        let f = Function::new_with_args("r, v", "setTimeout(() => r(v), 1000);");
        let _ = f.call2(&JsValue::NULL, &resolve, &JsValue::from_str(NEVER_SETTLED));
    });
    match JsFuture::from(js_sys::Promise::race(&Array::of2(&p, &timeout))).await {
        Ok(v) if v.as_string().as_deref() == Some(NEVER_SETTLED) => None,
        settled => Some(settled),
    }
}

/// Register a TS command whose body is `body`.
fn command(core: &BtermCore, name: &str, body: &str) {
    let sig = js_sys::JSON::parse(&format!(r#"{{"name":"{name}"}}"#)).expect("sig");
    core.register_command(sig, Function::new_with_args("args, input, ctx", body))
        .expect("registered");
}

#[wasm_bindgen_test]
async fn a_partial_write_survives_a_throw_in_the_default_line_mode() {
    // The headline case. `line` is the DEFAULT mode, so a write with no
    // delimiter in it is still sitting in the buffer when the command
    // throws -- and the throw returns out of `JsCommand::run` well before
    // the drain at the bottom of its body. `RunError` promises to carry
    // whatever the pipeline wrote before it failed; that has to include the
    // text that explains what it was doing when it died, which in practice
    // is exactly the partial line.
    let core = make_core();
    command(
        &core,
        "halfway",
        "ctx.log.write('starting fetch'); ctx.err.write('warning: slow'); throw new Error('boom');",
    );
    let err = run_line(&core, "halfway").await.expect_err("rejects");
    let log = entries(&err, "log");
    let errs = entries(&err, "err");
    assert!(contains(&log, "starting fetch"), "partial log lost on throw: {log:?}");
    assert!(contains(&errs, "warning: slow"), "partial err lost on throw: {errs:?}");
    core.dispose();
}

#[wasm_bindgen_test]
async fn a_block_mode_write_survives_a_throw_without_flush() {
    // Block mode holds everything until `flush()`, so even a complete line
    // is still buffered when the command throws.
    let core = make_core();
    command(
        &core,
        "held",
        r#"ctx.log.mode('block'); ctx.log.write('one\ntwo\n'); throw new Error('boom');"#,
    );
    let err = run_line(&core, "held").await.expect_err("rejects");
    let log = entries(&err, "log");
    assert!(contains(&log, "one"), "block-mode buffer lost on throw: {log:?}");
    assert!(contains(&log, "two"), "block-mode buffer lost on throw: {log:?}");
    core.dispose();
}

#[wasm_bindgen_test]
async fn a_block_mode_write_that_never_flushes_appears_once_on_success() {
    // Two drains now exist for a command that returns normally -- the one
    // at the bottom of `JsCommand::run` and the run-scoped one -- so this
    // pins that the second finds an empty buffer rather than emitting the
    // text twice.
    let core = make_core();
    command(&core, "held", "ctx.log.mode('block'); ctx.log.write('once'); return 1;");
    let out = run_line(&core, "held").await.expect("resolves");
    let log = entries(&out, "log");
    let hits = log.iter().filter(|e| e.contains("once")).count();
    assert_eq!(hits, 1, "buffered text emitted {hits} times: {log:?}");
    core.dispose();
}

#[wasm_bindgen_test]
async fn a_partial_write_survives_ctrl_c() {
    // Ctrl-C is the case no amount of care inside the command body can
    // cover: `Abortable::poll` returns `Ready(Err(Aborted))` *before*
    // polling the inner future, so a suspended body never runs again. The
    // flush has to come from the abort path itself.
    //
    // The `tick()` is load-bearing, and it is about this test's subject
    // rather than about cancellation: there is only a partial write to
    // rescue once the command body has actually run and written one. An
    // interrupt delivered before the task's first poll settles it without
    // running the body at all -- correct, but a different case, and the one
    // `a_same_tick_ctrl_c_settles_the_run` covers.
    let core = make_core();
    command(
        &core,
        "hangs",
        "ctx.log.write('partial before ctrl-c'); return new Promise((res, rej) => { ctx.signal.addEventListener('abort', () => rej(new Error('interrupted'))); });",
    );
    let pending = core.run(0, "hangs".to_string());
    tick().await;
    core.feed(0, "\x03");
    let err = JsFuture::from(pending).await.expect_err("aborted run rejects");
    let log = entries(&err, "log");
    assert!(contains(&log, "partial before ctrl-c"), "partial log lost on Ctrl-C: {log:?}");
    core.dispose();
}

#[wasm_bindgen_test]
async fn a_same_tick_ctrl_c_settles_the_run() {
    // A `run()` has to be cancellable from the moment its promise exists,
    // not from the moment the microtask queue next drains. A page that wires
    // a cancel button and clicks it before yielding -- or any caller that
    // feeds Ctrl-C on the next statement -- delivers the interrupt in the
    // same tick as the call, and there is no tick to wait for.
    //
    // The deliberate absence of a `tick()` between the two lines below is
    // the whole test. With the run registered only inside the spawned
    // future, that Ctrl-C finds an empty task registry, aborts nothing, and
    // leaves a promise that never settles.
    //
    // Nothing is asserted about the sink here: an abort landing before the
    // first poll means `Abortable` returns `Err(Aborted)` without ever
    // polling the inner future, so no command body ran and there is nothing
    // for it to have written. The rejection still has to carry the two
    // channels, empty -- that is `RunError`'s shape, not a special case.
    let core = make_core();
    command(&core, "hangs", "return new Promise(() => {});");

    let pending = core.run(0, "hangs".to_string());
    core.feed(0, "\x03");

    let settled = within_a_second(pending).await.expect("a same-tick Ctrl-C left run() hanging");
    let err = settled.expect_err("an aborted run rejects");
    let msg = Reflect::get(&err, &"message".into())
        .ok()
        .and_then(|m| m.as_string())
        .unwrap_or_default();
    assert!(msg.contains("aborted"), "{msg}");
    assert!(Reflect::has(&err, &"log".into()).unwrap_or(false), "rejection dropped `log`");
    assert!(Reflect::has(&err, &"err".into()).unwrap_or(false), "rejection dropped `err`");
    core.dispose();
}

#[wasm_bindgen_test]
async fn run_resolves_scalar_and_plain_objects() {
    let core = make_core();
    let v = run_value(&core, "echo 5").await.expect("resolves");
    assert_eq!(v.as_f64(), Some(5.0));

    // Records must arrive as plain objects (never Map).
    let v = run_value(&core, "echo '{\"a\":1}' | from json").await.expect("resolves");
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
    let v = run_value(&core, "answer").await.expect("resolves");
    assert_eq!(v.as_f64(), Some(42.0));

    // Integral JS numbers become Int — usable by first/last.
    let sig = js_sys::JSON::parse(r#"{"name":"nums"}"#).expect("sig");
    let f = Function::new_with_args("args", "return [10, 20, 30];");
    core.register_command(sig, f).expect("registered");
    let v = run_value(&core, "nums | head 2 | length").await.expect("resolves");
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
    let v = run_value(&core, "shape a b --limit 7").await.expect("resolves");
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
        // "Learning Rust" holds "Rust" without starting with it, so the `^`
        // assertion below still separates a real anchor from a plain
        // substring search — a substring dialect would count three, not two.
        //
        // "Go lang" matches nothing any pattern here looks for. It is the
        // control: without a row that stays out, the alternation case below
        // matches every row, and would pass just as happily against a `grep`
        // that filtered nothing at all.
        r#"return [{t:"Rust lang"},{t:"WebAssembly"},{t:"rust book"},
                  {t:"Rust by example"},{t:"WebAssembly spec"},{t:"Learning Rust"},
                  {t:"Go lang"}];"#,
    );
    core.register_command(sig, f).expect("registered");

    // Every count below is deliberately ≥ 2. A stage that leaves exactly one
    // row collapses to a bare record on the inter-stage channel (the
    // batch-model note in `bterm-core`'s `builtins`), and `length` then
    // rejects it as "expects a list or string" — which would fail this test
    // for a reason having nothing to do with regex. Non-singleton data keeps
    // each assertion about the thing it names.

    // Anchors: two rows *begin* with capital "Rust". "rust book" is excluded
    // by case and "Learning Rust" by position — the latter is what proves `^`
    // anchors rather than being matched literally or ignored.
    let v = run_value(&core, "rows | grep '^Rust' | length").await.expect("resolves");
    assert_eq!(v.as_f64(), Some(2.0), "^ anchor is regex, not a literal");

    // …and *which* two, which is the stronger claim: a count of 2 would also
    // be satisfied by matching "Learning Rust" and dropping one of these.
    let v = run_value(&core, "rows | grep '^Rust' | map t").await.expect("resolves");
    assert_eq!(strings(&v), ["Rust lang", "Rust by example"], "^ matched the wrong rows");

    // Alternation + case-insensitive: everything but the "Go lang" control.
    let v = run_value(&core, "rows | grep 'rust|assembly' -i | length").await.expect("resolves");
    assert_eq!(v.as_f64(), Some(6.0), "| is alternation");

    // Character class + quantifier — both "WebAssembly…" rows, no others.
    let v = run_value(&core, r#"rows | grep '[A-Z][a-z]+As' | length"#).await.expect("resolves");
    assert_eq!(v.as_f64(), Some(2.0));
    let v = run_value(&core, r#"rows | grep '[A-Z][a-z]+As' | map t"#).await.expect("resolves");
    assert_eq!(strings(&v), ["WebAssembly", "WebAssembly spec"]);

    // Invert still composes with regex: the five rows `^Rust` did not match.
    let v = run_value(&core, "rows | grep '^Rust' -v | length").await.expect("resolves");
    assert_eq!(v.as_f64(), Some(5.0));
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

    let v = run_value(&core, "echo 5").await.expect("engine still alive");
    assert_eq!(v.as_f64(), Some(5.0));
    core.dispose();
}

#[wasm_bindgen_test]
async fn inline_functions_project_and_filter() {
    let core = make_core();
    let sig = js_sys::JSON::parse(r#"{"name":"rows"}"#).expect("sig");
    let f = Function::new_with_args(
        "args",
        // `bb` sits mid-list so that the `^b` grep below leaves two rows
        // rather than one: a lone survivor collapses to a bare record (the
        // batch-model note in `bterm-core`'s `builtins`) and there would be
        // no list left to assert the "keeps whole rows" part on. Mid-list
        // rather than appended so `tail` still lands on `c`.
        r#"return [{id:1,name:"a"},{id:7,name:"b"},{id:3,name:"bb"},{id:9,name:"c"}];"#,
    );
    core.register_command(sig, f).expect("registered");

    // The shape from the original sketch: project one field, filter by
    // another, then compose with the rest of the pipeline.
    let v = run_value(&core, "rows | map '(o) => o.name' | length").await.expect("resolves");
    assert_eq!(v.as_f64(), Some(4.0));

    let v = run_value(&core, "rows | filter '(o) => o.id > 5' | length").await.expect("resolves");
    assert_eq!(v.as_f64(), Some(2.0));

    // Computed projection — not expressible as a field path at all.
    // Bare `tail` yields the scalar; `tail 1` would yield a one-item list.
    let v = run_value(&core, "rows | map '(o) => o.name + o.id' | tail").await.expect("resolves");
    assert_eq!(v.as_string().as_deref(), Some("c9"));

    // `--on` with a function: filter on a computed key while keeping rows.
    // "b" and "bb" match; their ids survive, so the rows came through whole
    // rather than having been projected down to the computed key.
    let v = run_value(&core, "rows | grep '^b' --on '(o) => o.name' | map id")
        .await
        .expect("resolves");
    let arr: Array = v.dyn_into().expect("list");
    assert_eq!(arr.length(), 2);
    assert_eq!(arr.get(0).as_f64(), Some(7.0));
    assert_eq!(arr.get(1).as_f64(), Some(3.0));

    // Computed sort key: descending by negating, so the largest id leads.
    let v = run_value(&core, "rows | sort-by --on '(o) => -o.id' | map id | head")
        .await
        .expect("resolves");
    assert_eq!(v.as_f64(), Some(9.0));
    core.dispose();
}

#[wasm_bindgen_test]
async fn registered_functions_work_without_eval() {
    let core = make_core();
    let sig = js_sys::JSON::parse(r#"{"name":"rows"}"#).expect("sig");
    core.register_command(
        sig,
        // Three rows so two survive `@big`: one survivor would collapse to a
        // bare record and `length` would reject it. Same shape as the native
        // `closure_filters_without_any_host_engine`.
        Function::new_with_args("args", r#"return [{id:1},{id:7},{id:9}];"#),
    )
    .expect("registered");

    core.register_fn("big", Function::new_with_args("o", "return o.id > 5;"))
        .expect("register_fn");
    let v = run_value(&core, "rows | filter @big | length").await.expect("resolves");
    assert_eq!(v.as_f64(), Some(2.0));

    // Unknown name gets a did-you-mean rather than a bare failure.
    let err = run_line(&core, "rows | filter @bigg").await.expect_err("unknown");
    let msg = Reflect::get(&err, &"message".into())
        .ok()
        .and_then(|m| m.as_string())
        .unwrap_or_default();
    assert!(msg.contains("did you mean `@big`"), "message: {msg}");

    core.unregister_fn("big");
    assert!(run_line(&core, "rows | filter @big").await.is_err(), "unregistered");
    core.dispose();
}

#[wasm_bindgen_test]
async fn callable_errors_are_clean_and_survivable() {
    let core = make_core();
    let sig = js_sys::JSON::parse(r#"{"name":"rows"}"#).expect("sig");
    core.register_command(
        sig,
        Function::new_with_args("args", r#"return [{id:1}];"#),
    )
    .expect("registered");

    // Syntax error in inline source.
    let err = run_line(&core, "rows | map '(o) =>'").await.expect_err("syntax");
    let msg = Reflect::get(&err, &"message".into())
        .ok()
        .and_then(|m| m.as_string())
        .unwrap_or_default();
    assert!(!msg.is_empty(), "syntax error surfaces");

    // A function that throws at call time.
    let err = run_line(&core, "rows | map '(o) => { throw new Error(\"boom\") }'")
        .await
        .expect_err("throws");
    let msg = Reflect::get(&err, &"message".into())
        .ok()
        .and_then(|m| m.as_string())
        .unwrap_or_default();
    assert!(msg.contains("boom"), "message: {msg}");

    // Engine survives both.
    let v = run_value(&core, "echo 7").await.expect("still alive");
    assert_eq!(v.as_f64(), Some(7.0));
    core.dispose();
}

#[wasm_bindgen_test]
async fn native_closures_match_the_cli_exactly() {
    // These are the same lines the native CLI runs in bterm-core's tests —
    // no JS engine is involved in evaluating them, so browser and CLI agree.
    let core = make_core();
    let sig = js_sys::JSON::parse(r#"{"name":"rows"}"#).expect("sig");
    core.register_command(
        sig,
        // Three rows so `a > 5` leaves two: one survivor would collapse to a
        // bare record and `length` would reject it, exactly as the native
        // `closure_filters_without_any_host_engine` had to account for.
        // `a:7` last keeps the two `head` assertions below unchanged.
        Function::new_with_args("args", r#"return [{a:2,b:3},{a:10,b:1},{a:7,b:4}];"#),
    )
    .expect("registered");

    let v = run_value(&core, "rows | filter {|o| $o.a > 5} | length").await.expect("resolves");
    assert_eq!(v.as_f64(), Some(2.0));

    let v = run_value(&core, "rows | map {|o| $o.a * $o.b} | head").await.expect("resolves");
    assert_eq!(v.as_f64(), Some(6.0));

    // Computed descending sort key, expressible in no column.
    let v = run_value(&core, "rows | sort-by --on {|o| -$o.a} | map a | head")
        .await
        .expect("resolves");
    assert_eq!(v.as_f64(), Some(10.0));

    // Closures and JS lambdas coexist in one pipeline. Every row clears
    // `a > 1`, so all three reach the JS lambda.
    let v = run_value(&core, "rows | filter {|o| $o.a > 1} | map '(o) => o.b' | length")
        .await
        .expect("resolves");
    assert_eq!(v.as_f64(), Some(3.0));
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
    let v = run_value(&core, "mine").await.expect("resolves");
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
    // The subject here is `ctx.signal` reaching a *running* command, so the
    // body has to have run and attached its listener before the interrupt --
    // hence the tick. Interrupting earlier than that still rejects the run
    // (`a_same_tick_ctrl_c_settles_the_run`), but the body never runs, so
    // there is no listener to fire and nothing for this test to observe.
    tick().await;
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
