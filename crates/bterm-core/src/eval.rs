//! Pipeline evaluation. Engine-agnostic: command lookup goes through
//! `CommandSource`, so the wasm engine can resolve inside a short
//! `with_engine` borrow while the CLI borrows a registry directly. Nothing
//! is ever borrowed across an await.

use crate::ast::{Call, Line, Pipeline};
use crate::error::ShellError;
use crate::registry::{Command, CommandRegistry, ExecContext, PipelineData};
use crate::signature::{bind, wants_help, Scope};
use crate::value::Value;
use std::rc::Rc;

/// Resolves command names. `lookup` is synchronous and must not hold any
/// borrow after returning (clone the Rc out).
pub trait CommandSource {
    /// Longest-prefix resolution over leading barewords → (command, words consumed).
    fn lookup(&self, words: &[String]) -> Option<(Rc<dyn Command>, usize)>;
    fn unknown_command_error(&self, word: &str, span: crate::error::Span) -> ShellError;
}

impl CommandSource for CommandRegistry {
    fn lookup(&self, words: &[String]) -> Option<(Rc<dyn Command>, usize)> {
        CommandRegistry::lookup(self, words)
    }

    fn unknown_command_error(&self, word: &str, span: crate::error::Span) -> ShellError {
        CommandRegistry::unknown_command_error(self, word, span)
    }
}

/// Shared-registry variant (CLI, wasm engine): borrows briefly, clones the
/// Rc out, drops the borrow before any await.
impl CommandSource for std::cell::RefCell<CommandRegistry> {
    fn lookup(&self, words: &[String]) -> Option<(Rc<dyn Command>, usize)> {
        self.borrow().lookup(words)
    }

    fn unknown_command_error(&self, word: &str, span: crate::error::Span) -> ShellError {
        self.borrow().unknown_command_error(word, span)
    }
}

/// Evaluate one submitted line. Returns one result per `;`-pipeline (each is
/// rendered separately by the host).
pub async fn eval_line(
    line: &Line,
    source: &impl CommandSource,
    ctx: &ExecContext,
    scope: &Scope,
) -> Result<Vec<PipelineData>, ShellError> {
    let mut results = Vec::new();
    for pipeline in &line.pipelines {
        results.push(eval_pipeline(pipeline, source, ctx, scope).await?);
    }
    Ok(results)
}

pub async fn eval_pipeline(
    pipeline: &Pipeline,
    source: &impl CommandSource,
    ctx: &ExecContext,
    scope: &Scope,
) -> Result<PipelineData, ShellError> {
    let mut data = PipelineData::Empty;
    for call in &pipeline.calls {
        data = eval_call(call, data, source, ctx, scope).await?;
    }
    Ok(data)
}

async fn eval_call(
    call: &Call,
    input: PipelineData,
    source: &impl CommandSource,
    ctx: &ExecContext,
    scope: &Scope,
) -> Result<PipelineData, ShellError> {
    let words: Vec<String> = call.words.iter().map(|w| w.node.clone()).collect();
    let (cmd, consumed) = source
        .lookup(&words)
        .ok_or_else(|| source.unknown_command_error(&words[0], call.words[0].span))?;

    // `--help` intercepted before binding, so a malformed call still gets help.
    if wants_help(call) {
        return Ok(PipelineData::Value(Value::Str(cmd.signature().render_help())));
    }

    let bound = bind(cmd.signature(), &call.words[consumed..], call, scope)?;
    cmd.run(ctx.clone(), bound, input).await
}

/// Minimal executor for native use (CLI, tests). Core builtins complete
/// without yielding, so this just polls in a loop with a no-op waker; a
/// pending future (impossible natively in v1) would spin, not deadlock.
pub fn block_on<T>(fut: impl std::future::Future<Output = T>) -> T {
    use std::pin::pin;
    use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};

    fn noop_raw_waker() -> RawWaker {
        fn clone(_: *const ()) -> RawWaker {
            noop_raw_waker()
        }
        fn noop(_: *const ()) {}
        RawWaker::new(std::ptr::null(), &RawWakerVTable::new(clone, noop, noop, noop))
    }

    // SAFETY: the vtable functions are all no-ops over a null pointer.
    let waker = unsafe { Waker::from_raw(noop_raw_waker()) };
    let mut cx = Context::from_waker(&waker);
    let mut fut = pin!(fut);
    loop {
        match fut.as_mut().poll(&mut cx) {
            Poll::Ready(v) => return v,
            Poll::Pending => std::hint::spin_loop(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::{ready, HostHooks, LocalBoxFuture};
    use crate::signature::{BoundCall, Shape, Signature};

    struct NullHost;
    impl HostHooks for NullHost {
        fn emit_line(&self, _line: &str) {}
    }

    fn ctx() -> ExecContext {
        ExecContext { host: Rc::new(NullHost), width: 80 }
    }

    /// Fake command: `emit <n>` produces Int(n); `double` doubles its input.
    struct Emit;
    impl Command for Emit {
        fn signature(&self) -> &Signature {
            static SIG: std::sync::OnceLock<Signature> = std::sync::OnceLock::new();
            SIG.get_or_init(|| {
                Signature::build("emit", "emit an int").required_arg("n", Shape::Int, "the int")
            })
        }
        fn run(
            &self,
            _ctx: ExecContext,
            call: BoundCall,
            _input: PipelineData,
        ) -> LocalBoxFuture<Result<PipelineData, ShellError>> {
            let n = call.positionals[0].as_int().unwrap_or(0);
            ready(Ok(PipelineData::Value(Value::Int(n))))
        }
    }

    struct Double;
    impl Command for Double {
        fn signature(&self) -> &Signature {
            static SIG: std::sync::OnceLock<Signature> = std::sync::OnceLock::new();
            SIG.get_or_init(|| Signature::build("double", "double the input"))
        }
        fn run(
            &self,
            _ctx: ExecContext,
            _call: BoundCall,
            input: PipelineData,
        ) -> LocalBoxFuture<Result<PipelineData, ShellError>> {
            let out = match input.into_value() {
                Value::Int(n) => Value::Int(n * 2),
                other => other,
            };
            ready(Ok(PipelineData::Value(out)))
        }
    }

    fn registry() -> CommandRegistry {
        let mut r = CommandRegistry::new();
        r.register_builtin(Rc::new(Emit));
        r.register_builtin(Rc::new(Double));
        r
    }

    fn eval(src: &str) -> Result<Vec<PipelineData>, ShellError> {
        let out = crate::parse::parse(src);
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        block_on(eval_line(&out.line, &registry(), &ctx(), &Scope::new()))
    }

    #[test]
    fn pipeline_threads_values() {
        let results = eval("emit 21 | double").expect("eval");
        assert_eq!(results, vec![PipelineData::Value(Value::Int(42))]);
    }

    #[test]
    fn semicolon_pipelines_all_evaluate() {
        let results = eval("emit 1; emit 2 | double").expect("eval");
        assert_eq!(
            results,
            vec![
                PipelineData::Value(Value::Int(1)),
                PipelineData::Value(Value::Int(4)),
            ]
        );
    }

    #[test]
    fn unknown_command_suggests() {
        let err = eval("emti 1").expect_err("should fail");
        assert!(err.msg.contains("unknown command `emti`"));
        assert_eq!(err.help.as_deref(), Some("did you mean `emit`?"));
    }

    #[test]
    fn help_intercepted_before_binding() {
        // Missing required arg, but --help still works.
        let results = eval("emit --help").expect("help");
        match &results[0] {
            PipelineData::Value(Value::Str(s)) => assert!(s.contains("Usage:")),
            other => panic!("expected help text, got {other:?}"),
        }
    }
}
