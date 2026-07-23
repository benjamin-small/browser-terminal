//! TS-registered commands: a `Signature` plus a JS function, invoked as
//! `fn(args, input, ctx)` where `args = { positionals, flags }`, `input` is
//! the piped value, and `ctx = { signal: AbortSignal, log(line), err(line),
//! emit(line) }` (`emit` is an alias for `log`).
//! Sync returns are tolerated via `Promise.resolve`; rejections map to
//! `ShellError` (rich `{ message, help? }` objects keep their help text).

use crate::convert::{js_to_value, value_to_js};
use bterm_core::error::Span;
use bterm_core::registry::{Command, ExecContext, LocalBoxFuture, PipelineData};
use bterm_core::signature::{BoundCall, Signature};
use bterm_core::sink::Record;
use bterm_core::ShellError;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::JsValue;

pub struct JsCommand {
    pub sig: Signature,
    pub func: js_sys::Function,
}

impl Command for JsCommand {
    fn signature(&self) -> &Signature {
        &self.sig
    }

    fn run(
        &self,
        ctx: ExecContext,
        call: BoundCall,
        input: PipelineData,
    ) -> LocalBoxFuture<Result<PipelineData, ShellError>> {
        let func = self.func.clone();
        let name = self.sig.name.clone();
        Box::pin(async move {
            let span = call.head_span;

            let positionals = js_sys::Array::new();
            for v in &call.positionals {
                positionals.push(&value_to_js(v));
            }
            let flags = js_sys::Object::new();
            for (k, v) in &call.flags {
                let _ = js_sys::Reflect::set(&flags, &JsValue::from_str(k), &value_to_js(v));
            }
            let args = js_sys::Object::new();
            let _ = js_sys::Reflect::set(&args, &JsValue::from_str("positionals"), &positionals);
            let _ = js_sys::Reflect::set(&args, &JsValue::from_str("flags"), &flags);

            let input_js = value_to_js(&input.into_value());

            let ctx_obj = js_sys::Object::new();
            if let Some(signal) = crate::tasks::signal_for(ctx.run_id) {
                let _ = js_sys::Reflect::set(&ctx_obj, &JsValue::from_str("signal"), &signal);
            }
            let log_sink = ctx.sink.clone();
            // These stay alive across the await: an async command may write
            // from a continuation. A command that stashes one and calls it
            // after completing gets a JS error, which is the intended signal.
            let log = Closure::<dyn Fn(String)>::new(move |line: String| {
                log_sink.write(Record::Log(line));
            });
            let err_sink = ctx.sink.clone();
            let err = Closure::<dyn Fn(String)>::new(move |line: String| {
                err_sink.write(Record::Err(line));
            });
            let emit_sink = ctx.sink.clone();
            // `emit` predates the channel split and is what every existing
            // command calls; it is retained as an alias for `log`.
            let emit = Closure::<dyn Fn(String)>::new(move |line: String| {
                emit_sink.write(Record::Log(line));
            });
            let _ = js_sys::Reflect::set(&ctx_obj, &JsValue::from_str("log"), log.as_ref());
            let _ = js_sys::Reflect::set(&ctx_obj, &JsValue::from_str("err"), err.as_ref());
            let _ = js_sys::Reflect::set(&ctx_obj, &JsValue::from_str("emit"), emit.as_ref());

            let returned = func
                .call3(&JsValue::NULL, &args, &input_js, &ctx_obj)
                .map_err(|e| js_error_to_shell(&e, span, &name))?;
            let resolved = wasm_bindgen_futures::JsFuture::from(js_sys::Promise::resolve(&returned))
                .await
                .map_err(|e| js_error_to_shell(&e, span, &name))?;
            drop(log);
            drop(err);
            drop(emit);

            if resolved.is_undefined() {
                return Ok(PipelineData::Empty);
            }
            let value = js_to_value(&resolved)
                .map_err(|msg| ShellError::runtime(format!("`{name}`: {msg}")).with_span(span))?;
            Ok(PipelineData::Value(value))
        })
    }
}

/// Map a thrown/rejected JS value to a ShellError. `Error` instances and
/// plain `{ message, help? }` objects keep their message and help; stacks go
/// to the browser console.
fn js_error_to_shell(e: &JsValue, span: Span, cmd: &str) -> ShellError {
    if e.is_object() {
        let get_str = |key: &str| {
            js_sys::Reflect::get(e, &JsValue::from_str(key))
                .ok()
                .and_then(|v| v.as_string())
        };
        if let Some(msg) = get_str("message") {
            if let Some(stack) = get_str("stack") {
                web_sys::console::error_1(&JsValue::from_str(&stack));
            }
            let mut err = ShellError::runtime(format!("`{cmd}`: {msg}")).with_span(span);
            if let Some(help) = get_str("help") {
                err = err.with_help(help);
            }
            return err;
        }
    }
    let text = e
        .as_string()
        .unwrap_or_else(|| js_sys::JSON::stringify(e).ok().and_then(|s| s.as_string()).unwrap_or_else(|| "unknown error".into()));
    ShellError::runtime(format!("`{cmd}`: {text}")).with_span(span)
}
