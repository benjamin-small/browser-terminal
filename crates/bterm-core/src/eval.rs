//! Pipeline evaluation. Engine-agnostic: command lookup goes through
//! `CommandSource`, so the wasm engine can resolve inside a short
//! `with_engine` borrow while the CLI borrows a registry directly. Nothing
//! is ever borrowed across an await.

use crate::ast::{Call, Line, Pipeline};
use crate::error::ShellError;
use crate::registry::{Command, CommandRegistry, ExecContext, PipelineData};
use crate::signature::{bind, wants_help, Scope};
use std::cell::RefCell;
use std::rc::Rc;

/// Resolves command names. `lookup` is synchronous and must not hold any
/// borrow after returning (clone the Rc out).
pub trait CommandSource {
    /// Longest-prefix resolution over leading barewords → (command, words consumed).
    fn lookup(&self, words: &[String]) -> Option<(Rc<dyn Command>, usize)>;
    /// Rendered group page when `words` prefixes commands without being one
    /// (`task`, `mux window`). Checked only after `lookup` fails.
    fn group_help(&self, words: &[String]) -> Option<String>;
    fn unknown_command_error(&self, words: &[crate::ast::Spanned<String>]) -> ShellError;
}

impl CommandSource for CommandRegistry {
    fn lookup(&self, words: &[String]) -> Option<(Rc<dyn Command>, usize)> {
        CommandRegistry::lookup(self, words)
    }

    fn group_help(&self, words: &[String]) -> Option<String> {
        CommandRegistry::group_help(self, words)
    }

    fn unknown_command_error(&self, words: &[crate::ast::Spanned<String>]) -> ShellError {
        CommandRegistry::unknown_command_error(self, words)
    }
}

/// Shared-registry variant (CLI, wasm engine): borrows briefly, clones the
/// Rc out, drops the borrow before any await.
impl CommandSource for std::cell::RefCell<CommandRegistry> {
    fn lookup(&self, words: &[String]) -> Option<(Rc<dyn Command>, usize)> {
        self.borrow().lookup(words)
    }

    fn group_help(&self, words: &[String]) -> Option<String> {
        self.borrow().group_help(words)
    }

    fn unknown_command_error(&self, words: &[crate::ast::Spanned<String>]) -> ShellError {
        self.borrow().unknown_command_error(words)
    }
}

/// Evaluate one submitted line. Returns one result per completed
/// `;`-pipeline plus the error that stopped a later pipeline, if any —
/// earlier successful results are never discarded.
pub async fn eval_line(
    line: &Line,
    source: &impl CommandSource,
    ctx: &ExecContext,
    scope: &Scope,
) -> (Vec<PipelineData>, Option<ShellError>) {
    let mut results = Vec::new();
    for pipeline in &line.pipelines {
        match eval_pipeline(pipeline, source, ctx, scope).await {
            Ok(data) => results.push(data),
            Err(err) => return (results, Some(err)),
        }
    }
    (results, None)
}

/// Evaluate a pipeline by running every stage as a concurrent future joined
/// by bounded channels.
///
/// Every command still collects its whole input, so a stage cannot start
/// before its predecessor finishes and the output is identical to the
/// sequential version this replaced. What changes is the transport: the
/// machinery for streaming stages is in place and exercised, so a later
/// stage adds streaming commands rather than rebuilding how stages talk.
pub async fn eval_pipeline(
    pipeline: &Pipeline,
    source: &impl CommandSource,
    ctx: &ExecContext,
    scope: &Scope,
) -> Result<PipelineData, ShellError> {
    // A single call has no channel to build; keep the direct path so the
    // common case pays nothing for machinery it cannot use.
    if pipeline.calls.len() == 1 {
        return eval_call(&pipeline.calls[0], PipelineData::Empty, source, ctx, scope).await;
    }

    let outcome: Rc<RefCell<Option<PipelineData>>> = Rc::new(RefCell::new(None));
    let failure: Rc<RefCell<Option<ShellError>>> = Rc::new(RefCell::new(None));

    let mut stages: Vec<crate::pipeline::BoxedStage<'_>> = Vec::new();
    let mut upstream: Option<crate::chan::Receiver> = None;

    for (idx, call) in pipeline.calls.iter().enumerate() {
        let last = idx + 1 == pipeline.calls.len();
        let rx = upstream.take();
        let (tx, next_rx) = if last {
            (None, None)
        } else {
            let (tx, rx) = crate::chan::channel(STAGE_BUFFER);
            (Some(tx), Some(rx))
        };
        upstream = next_rx;

        stages.push(Box::pin(run_stage(
            call,
            rx,
            tx,
            source,
            ctx,
            scope,
            last.then(|| outcome.clone()),
            failure.clone(),
        )));
    }

    crate::pipeline::drive(stages).await;

    if let Some(err) = failure.borrow_mut().take() {
        return Err(err);
    }
    let result = outcome.borrow_mut().take().unwrap_or(PipelineData::Empty);
    Ok(result)
}

/// Items a stage may buffer before its producer is made to wait. The bound
/// is what makes memory usage independent of how fast a producer runs.
const STAGE_BUFFER: usize = 64;

/// One stage: drain the upstream channel, run the command, hand the result
/// downstream (or to `outcome`, for the last stage).
///
/// The first error wins and stops the pipeline; later stages find their
/// channel closed and return without overwriting it with a symptom.
#[allow(clippy::too_many_arguments)]
async fn run_stage(
    call: &Call,
    upstream: Option<crate::chan::Receiver>,
    downstream: Option<crate::chan::Sender>,
    source: &impl CommandSource,
    ctx: &ExecContext,
    scope: &Scope,
    outcome: Option<Rc<RefCell<Option<PipelineData>>>>,
    failure: Rc<RefCell<Option<ShellError>>>,
) {
    // Collect the whole upstream. Every command still wants its complete
    // input; streaming commands arrive in a later stage of this project.
    let input = match upstream {
        None => PipelineData::Empty,
        Some(mut rx) => {
            let mut last = PipelineData::Empty;
            while let Some(item) = rx.recv().await {
                last = item;
            }
            last
        }
    };

    // An earlier stage already failed; do not run, and do not overwrite its
    // error with a downstream symptom.
    if failure.borrow().is_some() {
        return;
    }

    match eval_call(call, input, source, ctx, scope).await {
        Ok(data) => match (downstream, outcome) {
            (Some(tx), _) => {
                // Err means the consumer went away, which is not this
                // stage's problem to report.
                let _ = tx.send(data).await;
            }
            (None, Some(slot)) => *slot.borrow_mut() = Some(data),
            (None, None) => {}
        },
        Err(err) => {
            let mut slot = failure.borrow_mut();
            if slot.is_none() {
                *slot = Some(err);
            }
        }
    }
}

async fn eval_call(
    call: &Call,
    input: PipelineData,
    source: &impl CommandSource,
    ctx: &ExecContext,
    scope: &Scope,
) -> Result<PipelineData, ShellError> {
    let words: Vec<String> = call.words.iter().map(|w| w.node.clone()).collect();
    let (cmd, consumed) = match source.lookup(&words) {
        Some(hit) => hit,
        // Not a command — but it may be a group, in which case naming it
        // (with or without `--help`) should list what lives under it.
        None => match source.group_help(&words) {
            Some(help) => return Ok(PipelineData::Rendered(help)),
            None => return Err(source.unknown_command_error(&call.words)),
        },
    };

    // `--help` intercepted before binding, so a malformed call still gets help.
    if wants_help(call) {
        return Ok(PipelineData::Rendered(cmd.signature().render_help()));
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
    use crate::value::Value;
    use crate::signature::{BoundCall, Shape, Signature};

    struct NullHost;
    impl HostHooks for NullHost {}

    fn ctx() -> ExecContext {
        ExecContext {
            host: Rc::new(NullHost),
            sink: Rc::new(crate::sink::NullSink),
            width: 80,
            pane: 0,
            run_id: 0,
        }
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
        let (results, error) = block_on(eval_line(&out.line, &registry(), &ctx(), &Scope::new()));
        match error {
            Some(e) => Err(e),
            None => Ok(results),
        }
    }

    #[test]
    fn failing_pipeline_keeps_earlier_results() {
        let out = crate::parse::parse("emit 1; nope; emit 3");
        assert!(out.errors.is_empty());
        let (results, error) = block_on(eval_line(&out.line, &registry(), &ctx(), &Scope::new()));
        assert_eq!(results, vec![PipelineData::Value(Value::Int(1))]);
        assert!(error.expect("second pipeline fails").msg.contains("unknown command"));
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
            // Rendered, not Value(Str): help is pre-formatted text and must
            // reach the terminal with its styling intact.
            PipelineData::Rendered(s) => {
                assert!(s.contains("Usage:"));
                assert!(s.contains('\x1b'), "help keeps its ANSI styling");
            }
            other => panic!("expected rendered help text, got {other:?}"),
        }
    }
}
