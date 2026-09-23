//! Async host callbacks for structured input/output redirection.

use bterm_core::redirect::RedirectHandler;
use bterm_core::registry::LocalBoxFuture;
use bterm_core::{ExecContext, ShellError, Span, Value};
use js_sys::{Function, Object, Promise, Reflect};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;

use crate::convert::{js_to_value, value_to_js};
use crate::js_command::js_error_to_shell;

pub struct JsRedirectHandler {
    receiver: JsValue,
    read: Function,
    write: Function,
}

impl JsRedirectHandler {
    pub fn new(receiver: JsValue) -> Result<Self, JsValue> {
        let method = |name: &str| -> Result<Function, JsValue> {
            Reflect::get(&receiver, &name.into())
                .map_err(|error| {
                    crate::js_error(
                        js_error_to_shell(&error, Span::new(0, 0), "redirect handler").msg,
                    )
                })?
                .dyn_into::<Function>()
                .map_err(|_| crate::js_error(format!("redirect handler needs a `{name}` function")))
        };
        let read = method("read")?;
        let write = method("write")?;
        Ok(Self {
            receiver,
            read,
            write,
        })
    }
}

fn context(ctx: &ExecContext, append: Option<bool>) -> Result<Object, ShellError> {
    let signal = crate::tasks::signal_for(ctx.run_id)
        .ok_or_else(|| ShellError::runtime("redirect run is no longer active"))?;
    let options = Object::new();
    let _ = Reflect::set(&options, &"signal".into(), &signal);
    let _ = Reflect::set(&options, &"session".into(), &ctx.session.into());
    let _ = Reflect::set(&options, &"pane".into(), &ctx.pane.into());
    if let Some(append) = append {
        let _ = Reflect::set(&options, &"append".into(), &append.into());
    }
    Ok(Object::freeze(&options))
}

impl RedirectHandler for JsRedirectHandler {
    fn read(&self, target: String, ctx: ExecContext) -> LocalBoxFuture<Result<Value, ShellError>> {
        let function = self.read.clone();
        let receiver = self.receiver.clone();
        Box::pin(async move {
            let options = context(&ctx, None)?;
            let error = |e| js_error_to_shell(&e, Span::new(0, 0), "redirect read");
            let result = function
                .call2(&receiver, &target.into(), &options)
                .map_err(error)?;
            let value = JsFuture::from(Promise::resolve(&result))
                .await
                .map_err(error)?;
            js_to_value(&value).map_err(ShellError::runtime)
        })
    }

    fn write(
        &self,
        target: String,
        value: Value,
        append: bool,
        ctx: ExecContext,
    ) -> LocalBoxFuture<Result<(), ShellError>> {
        let function = self.write.clone();
        let receiver = self.receiver.clone();
        Box::pin(async move {
            let options = context(&ctx, Some(append))?;
            let error = |e| js_error_to_shell(&e, Span::new(0, 0), "redirect write");
            let result = function
                .call3(&receiver, &target.into(), &value_to_js(&value), &options)
                .map_err(error)?;
            JsFuture::from(Promise::resolve(&result))
                .await
                .map_err(error)?;
            Ok(())
        })
    }
}
