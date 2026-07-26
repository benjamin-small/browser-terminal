//! TS-registered commands: a `Signature` plus a JS function, invoked as
//! `fn(args, input, ctx)` where `args = { positionals, flags }`, `input` is
//! the piped value, and `ctx = { signal: AbortSignal, log, err, emit }`.
//! `log` and `err` are callable writer objects, but the call and the writer
//! methods are different APIs: `ctx.log('line')` is a cooked, unbuffered
//! message -- the shell strip-everything sanitizes it, same as before this
//! module grew buffering -- while `ctx.log.write(s)`/`.flush()`/`.mode(...)`
//! pass terminal bytes through the allowlist sanitizer and an `OutputBuffer`,
//! for partial output and progress bars. `emit` is an alias for `log`, kept
//! because it predates the channel split.
//! Sync returns are tolerated via `Promise.resolve`; rejections map to
//! `ShellError` (rich `{ message, help? }` objects keep their help text).

use crate::convert::{js_to_value, value_to_js};
use bterm_core::error::Span;
use bterm_core::outbuf::{Mode, OutputBuffer};
use bterm_core::registry::{Command, ExecContext, LocalBoxFuture, PipelineData};
use bterm_core::signature::{BoundCall, Signature};
use bterm_core::sink::Record;
use bterm_core::ShellError;
use std::cell::RefCell;
use std::rc::Rc;
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
            // These stay alive across the await: an async command may write
            // from a continuation. A command that stashes one and calls it
            // after completing gets a JS error, which is the intended signal.
            //
            // The line call and the raw write are different APIs, not two
            // spellings of one: `ctx.log('msg')` passes a *message* -- the
            // shell owns the framing and strip-everything sanitizing, and
            // for *that* path `run()`'s log/err arrays are a public contract
            // of clean, single-line entries (a SECURITY test asserts an
            // embedded newline collapses so page-controlled text cannot fake
            // extra lines in a caller's log viewer). The contract is no wider
            // than the cooked path: a raw entry lands in the same array with
            // a different shape -- `ctx.log.write('a\nb')` puts one
            // multi-line entry in `run().log`, and line/block coalescing
            // means raw entry count does not track call count either.
            // `ctx.log.write('\r50%')` passes *terminal bytes* -- the command
            // owns the framing, and the allowlist sanitizer plus
            // `OutputBuffer` is what makes that safe.
            // Only `.write`/`.flush`/`.mode` touch the buffer; the
            // plain call bypasses it entirely, so mixing the two on one
            // channel reorders output -- `ctx.log.write('a')` may still be
            // buffered when a later `ctx.log('b')` goes straight out, so `b`
            // lands first and `a` follows when the buffer drains. Acceptable,
            // since a command that wants ordering guarantees should pick one
            // API and stick to it (or `flush()` before switching).
            let log_buf = Rc::new(RefCell::new(OutputBuffer::new()));
            // The run owns the buffer's tail, not this future: the drains at
            // the bottom of this function only happen if the body reaches
            // them, and every `?` below (and every abort) skips them. See
            // `tasks::flush_buffers`.
            crate::tasks::register_buffer(
                ctx.run_id,
                log_buf.clone(),
                ctx.sink.clone(),
                bterm_core::sink::Channel::Log,
            );

            // ctx.log(line) -- the sugar every existing command uses: cooked,
            // unbuffered, exactly as before the channel split.
            let sink_a = ctx.sink.clone();
            let log_call = Closure::<dyn Fn(String)>::new(move |line: String| {
                sink_a.write(Record::log(line));
            });

            // ctx.log.write(s) -- partial: no delimiter appended.
            let sink_b = ctx.sink.clone();
            let buf_b = log_buf.clone();
            let log_write = Closure::<dyn Fn(String)>::new(move |s: String| {
                let out = buf_b.borrow_mut().write(&s);
                if let Some(text) = out {
                    sink_b.write(Record::raw_log(text));
                }
            });

            // ctx.log.flush()
            let sink_c = ctx.sink.clone();
            let buf_c = log_buf.clone();
            let log_flush = Closure::<dyn Fn()>::new(move || {
                let out = buf_c.borrow_mut().flush();
                if let Some(text) = out {
                    sink_c.write(Record::raw_log(text));
                }
            });

            // ctx.log.mode(m, opts?)
            let buf_d = log_buf.clone();
            let log_mode = Closure::<dyn Fn(String, JsValue)>::new(move |m: String, opts: JsValue| {
                let mode = match m.as_str() {
                    "byte" => Mode::Byte,
                    "block" => Mode::Block,
                    _ => Mode::Line,
                };
                // A single-argument call passes `undefined`; Reflect::get on
                // it fails, and `.ok()` turns that into "no delimiter given".
                let delim = js_sys::Reflect::get(&opts, &JsValue::from_str("delimiter"))
                    .ok()
                    .and_then(|v| v.as_string());
                buf_d.borrow_mut().set_mode(mode, delim);
            });

            let log_fn = js_sys::Function::from(log_call.as_ref().clone());
            let _ = js_sys::Reflect::set(&log_fn, &JsValue::from_str("write"), log_write.as_ref());
            let _ = js_sys::Reflect::set(&log_fn, &JsValue::from_str("flush"), log_flush.as_ref());
            let _ = js_sys::Reflect::set(&log_fn, &JsValue::from_str("mode"), log_mode.as_ref());
            let _ = js_sys::Reflect::set(&ctx_obj, &JsValue::from_str("log"), &log_fn);
            // `emit` predates the channel split; it stays an alias for the
            // same function object (cooked call plus `.write`/`.flush`/`.mode`).
            let _ = js_sys::Reflect::set(&ctx_obj, &JsValue::from_str("emit"), &log_fn);

            let err_buf = Rc::new(RefCell::new(OutputBuffer::new()));
            crate::tasks::register_buffer(
                ctx.run_id,
                err_buf.clone(),
                ctx.sink.clone(),
                bterm_core::sink::Channel::Err,
            );

            // ctx.err(line) -- cooked and unbuffered, same reasoning as log_call.
            let sink_e = ctx.sink.clone();
            let err_call = Closure::<dyn Fn(String)>::new(move |line: String| {
                sink_e.write(Record::err(line));
            });

            let sink_f = ctx.sink.clone();
            let buf_f = err_buf.clone();
            let err_write = Closure::<dyn Fn(String)>::new(move |s: String| {
                let out = buf_f.borrow_mut().write(&s);
                if let Some(text) = out {
                    sink_f.write(Record::raw_err(text));
                }
            });

            let sink_g = ctx.sink.clone();
            let buf_g = err_buf.clone();
            let err_flush = Closure::<dyn Fn()>::new(move || {
                let out = buf_g.borrow_mut().flush();
                if let Some(text) = out {
                    sink_g.write(Record::raw_err(text));
                }
            });

            let buf_h = err_buf.clone();
            let err_mode = Closure::<dyn Fn(String, JsValue)>::new(move |m: String, opts: JsValue| {
                let mode = match m.as_str() {
                    "byte" => Mode::Byte,
                    "block" => Mode::Block,
                    _ => Mode::Line,
                };
                let delim = js_sys::Reflect::get(&opts, &JsValue::from_str("delimiter"))
                    .ok()
                    .and_then(|v| v.as_string());
                buf_h.borrow_mut().set_mode(mode, delim);
            });

            let err_fn = js_sys::Function::from(err_call.as_ref().clone());
            let _ = js_sys::Reflect::set(&err_fn, &JsValue::from_str("write"), err_write.as_ref());
            let _ = js_sys::Reflect::set(&err_fn, &JsValue::from_str("flush"), err_flush.as_ref());
            let _ = js_sys::Reflect::set(&err_fn, &JsValue::from_str("mode"), err_mode.as_ref());
            let _ = js_sys::Reflect::set(&ctx_obj, &JsValue::from_str("err"), &err_fn);

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
                // Drain at the *stage* boundary, so a stage's tail lands
                // before whatever the rest of the pipeline goes on to print
                // rather than at the end of the whole run.
                //
                // It is not what makes "nothing buffered is lost" true:
                // this line is only reached when the body returns normally,
                // and neither an early `?` above nor an abort (which never
                // resumes a suspended body at all) gets here.
                // `tasks::flush_buffers`, driven from the run's end, is the
                // guarantee; this is the nicety on top of it, and running
                // both is safe because a second `finish()` on a drained
                // buffer yields nothing.
                let tail_log = log_buf.borrow_mut().finish();
                if let Some(text) = tail_log {
                    ctx.sink.write(Record::raw_log(text));
                }
                let tail_err = err_buf.borrow_mut().finish();
                if let Some(text) = tail_err {
                    ctx.sink.write(Record::raw_err(text));
                }
                drop(log_call);
                drop(log_write);
                drop(log_flush);
                drop(log_mode);
                drop(err_call);
                drop(err_write);
                drop(err_flush);
                drop(err_mode);
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
            let tail_log = log_buf.borrow_mut().finish();
            if let Some(text) = tail_log {
                ctx.sink.write(Record::raw_log(text));
            }
            let tail_err = err_buf.borrow_mut().finish();
            if let Some(text) = tail_err {
                ctx.sink.write(Record::raw_err(text));
            }
            drop(log_call);
            drop(log_write);
            drop(log_flush);
            drop(log_mode);
            drop(err_call);
            drop(err_write);
            drop(err_flush);
            drop(err_mode);
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
