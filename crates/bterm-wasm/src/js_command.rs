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
        mut input: bterm_core::chan::Receiver,
        output: bterm_core::chan::Sender,
    ) -> LocalBoxFuture<Result<(), ShellError>> {
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

            let collected = bterm_core::stream::collect(&mut input).await;
            let input_js = value_to_js(&collected.into_value());

            let ctx_obj = js_sys::Object::new();
            if let Some(signal) = crate::tasks::signal_for(ctx.run_id) {
                let _ = js_sys::Reflect::set(&ctx_obj, &JsValue::from_str("signal"), &signal);
            }
            let log_sink = ctx.sink.clone();
            // These stay alive across the await: an async command may write
            // from a continuation. A command that stashes one and calls it
            // after completing gets a JS error, which is the intended signal.
            let log = Closure::<dyn Fn(String)>::new(move |line: String| {
                log_sink.write(Record::log(line));
            });
            let err_sink = ctx.sink.clone();
            let err = Closure::<dyn Fn(String)>::new(move |line: String| {
                err_sink.write(Record::err(line));
            });
            let emit_sink = ctx.sink.clone();
            // `emit` predates the channel split and is what every existing
            // command calls; it is retained as an alias for `log`.
            let emit = Closure::<dyn Fn(String)>::new(move |line: String| {
                emit_sink.write(Record::log(line));
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

            // Streaming: an async iterable (async generator) yields items over
            // time. `head` closing the channel makes us call the iterator's
            // return(), so the generator's finally runs -- that is where a
            // watch-style command removes its DOM listener.
            let async_iter_sym = js_sys::Symbol::async_iterator();
            let iter_getter = js_sys::Reflect::get(&resolved, async_iter_sym.as_ref()).ok();
            let is_async_iterable = iter_getter.as_ref().is_some_and(|f| f.is_function());

            if is_async_iterable {
                let getter = js_sys::Function::from(
                    js_sys::Reflect::get(&resolved, async_iter_sym.as_ref())
                        .map_err(|e| js_error_to_shell(&e, span, &name))?,
                );
                let iterator = getter
                    .call0(&resolved)
                    .map_err(|e| js_error_to_shell(&e, span, &name))?;
                let next_fn = js_sys::Function::from(
                    js_sys::Reflect::get(&iterator, &JsValue::from_str("next"))
                        .map_err(|e| js_error_to_shell(&e, span, &name))?,
                );
                loop {
                    let next_call = next_fn
                        .call0(&iterator)
                        .map_err(|e| js_error_to_shell(&e, span, &name))?;
                    let step = wasm_bindgen_futures::JsFuture::from(js_sys::Promise::resolve(&next_call))
                        .await
                        .map_err(|e| js_error_to_shell(&e, span, &name))?;
                    let done = js_sys::Reflect::get(&step, &JsValue::from_str("done"))
                        .map(|v| v.is_truthy())
                        .unwrap_or(true);
                    if done {
                        break;
                    }
                    let value_js = js_sys::Reflect::get(&step, &JsValue::from_str("value"))
                        .map_err(|e| js_error_to_shell(&e, span, &name))?;
                    let value = js_to_value(&value_js)
                        .map_err(|msg| ShellError::runtime(format!("`{name}`: {msg}")).with_span(span))?;
                    // Downstream closed (e.g. `head`): stop the generator so its
                    // finally runs (listeners/resources released).
                    //
                    // Note: this does NOT fire `ctx.signal` -- that only fires on
                    // Ctrl-C/dispose via `abort_pane`. An in-flight
                    // `fetch(..., { signal: ctx.signal })` between yields is
                    // therefore not cancelled by a downstream `head` closing the
                    // channel, only by Ctrl-C. Known, documented limitation; the
                    // generator's `finally` (via `return()`) is the cleanup path
                    // for downstream close.
                    if bterm_core::stream::flatten(PipelineData::Value(value), &output).await.is_err() {
                        stop_iterator(&iterator).await;
                        break;
                    }
                    // The send above only proves the channel had buffer space,
                    // not that downstream is still reading: a collecting
                    // consumer like `head N` can reach its limit and drop its
                    // receiver only *after* actually processing what we just
                    // sent, which happens on its own turn of the driver's poll
                    // pass, not before. `yield_once` hands control back so that
                    // turn happens before we ask the generator for the next
                    // item. Without it, a live source (`watch`) would block on
                    // a pull downstream no longer wants, waiting on a real-world
                    // event (e.g. a second click) that may never come, and
                    // `head N` could never terminate an infinite/live upstream
                    // as the streaming design promises.
                    yield_once().await;
                    if output.is_closed() {
                        stop_iterator(&iterator).await;
                        break;
                    }
                }
                drop(log);
                drop(err);
                drop(emit);
                return Ok(());
            }

            // Not an async iterable: the existing collecting path.
            let pd = if resolved.is_undefined() {
                PipelineData::Empty
            } else {
                let value = js_to_value(&resolved)
                    .map_err(|msg| ShellError::runtime(format!("`{name}`: {msg}")).with_span(span))?;
                PipelineData::Value(value)
            };
            let _ = bterm_core::stream::flatten(pd, &output).await;
            drop(log);
            drop(err);
            drop(emit);
            Ok(())
        })
    }
}

/// Call an async iterator's `return()`, if it has one, and await the result
/// so its `finally` block (listener/resource cleanup) completes before the
/// caller proceeds. Shared by both places the streaming loop decides to stop
/// early: a failed send, and a downstream close discovered after the fact.
async fn stop_iterator(iterator: &JsValue) {
    if let Ok(ret) = js_sys::Reflect::get(iterator, &JsValue::from_str("return")) {
        if ret.is_function() {
            let ret_fn = js_sys::Function::from(ret);
            if let Ok(p) = ret_fn.call0(iterator) {
                let _ = wasm_bindgen_futures::JsFuture::from(js_sys::Promise::resolve(&p)).await;
            }
        }
    }
}

/// Hand control back to `pipeline::drive`'s poll pass exactly once.
///
/// `drive` polls every stage once per pass regardless of whether earlier
/// stages returned `Ready` or `Pending`, so a self-wake-then-`Pending` here
/// lets a downstream stage that can finish synchronously (e.g. `head N`
/// hitting its limit) actually run and drop its receiver within the same
/// pass, rather than leaving `output.is_closed()` stale until some unrelated
/// future event happens to re-poll us.
async fn yield_once() {
    let mut yielded = false;
    std::future::poll_fn(move |cx| {
        if yielded {
            std::task::Poll::Ready(())
        } else {
            yielded = true;
            cx.waker().wake_by_ref();
            std::task::Poll::Pending
        }
    })
    .await
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
