//! Pipeline evaluation. Engine-agnostic: command lookup goes through
//! `CommandSource`, so the wasm engine can resolve inside a short
//! `with_engine` borrow while the CLI borrows a registry directly. Nothing
//! is ever borrowed across an await.

use crate::ast::{Call, Line, Pipeline};
use crate::error::ShellError;
use crate::registry::{Command, CommandRegistry, ExecContext, PipelineData};
use crate::signature::{bind, wants_help, Scope};
use crate::value::Value;
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
    let outcome: Rc<RefCell<PipelineData>> = Rc::new(RefCell::new(PipelineData::Empty));
    let failure: Rc<RefCell<Option<ShellError>>> = Rc::new(RefCell::new(None));

    // The first stage's input is an already-closed empty stream.
    let (empty_tx, empty_rx) = crate::chan::channel(1);
    drop(empty_tx);

    let mut stages: Vec<crate::pipeline::BoxedStage<'_>> = Vec::new();
    let mut upstream = empty_rx;

    for call in pipeline.calls.iter() {
        let (tx, rx) = crate::chan::channel(STAGE_BUFFER);
        stages.push(Box::pin(run_stage(
            call,
            upstream,
            tx,
            source,
            ctx,
            scope,
            failure.clone(),
        )));
        upstream = rx;
    }

    // Terminal collector: drain the last stage's output into `outcome`.
    //
    // Deliberately not `crate::stream::collect` — that helper is for a
    // *collecting command's own input*, where degrading `Rendered` to `Str`
    // is the intended "trust drops once consumed" rule. This is the outer
    // boundary instead: the renderer needs to know a single surviving item
    // was `Rendered` (pre-styled help/table text) rather than a `Value` to
    // print it verbatim instead of running it through `render()`. A single
    // command's own `flatten` call produces either all-`Value` items (a
    // `List` unrolled, or one scalar) or exactly one `Rendered` item — never
    // a mix — so only the N == 1 case needs the tag preserved; N > 1 is
    // always `Value` items and collects the same way `stream::collect` would.
    let outcome_for_collector = outcome.clone();
    stages.push(Box::pin(async move {
        let mut items: Vec<PipelineData> = Vec::new();
        while let Some(item) = upstream.recv().await {
            items.push(item);
        }
        let collected = match items.len() {
            0 => PipelineData::Empty,
            1 => items.into_iter().next().expect("len checked above"),
            _ => PipelineData::Value(Value::List(
                items.into_iter().map(PipelineData::into_value).collect(),
            )),
        };
        *outcome_for_collector.borrow_mut() = collected;
    }));

    crate::pipeline::drive(stages).await;

    if let Some(err) = failure.borrow_mut().take() {
        return Err(err);
    }
    let result = std::mem::replace(&mut *outcome.borrow_mut(), PipelineData::Empty);
    Ok(result)
}

/// Items a stage may buffer before its producer is made to wait. The bound
/// is what makes memory usage independent of how fast a producer runs.
const STAGE_BUFFER: usize = 64;

/// One stage: run the command from `input` to `output`.
///
/// The first error wins and stops the pipeline; later stages find their
/// channel closed and return without overwriting it with a symptom.
#[allow(clippy::too_many_arguments)]
async fn run_stage(
    call: &Call,
    input: crate::chan::Receiver,
    output: crate::chan::Sender,
    source: &impl CommandSource,
    ctx: &ExecContext,
    scope: &Scope,
    failure: Rc<RefCell<Option<ShellError>>>,
) {
    if failure.borrow().is_some() {
        return;
    }
    if let Err(err) = eval_call(call, input, output, source, ctx, scope).await {
        let mut slot = failure.borrow_mut();
        if slot.is_none() {
            *slot = Some(err);
        }
    }
}

async fn eval_call(
    call: &Call,
    input: crate::chan::Receiver,
    output: crate::chan::Sender,
    source: &impl CommandSource,
    ctx: &ExecContext,
    scope: &Scope,
) -> Result<(), ShellError> {
    let words: Vec<String> = call.words.iter().map(|w| w.node.clone()).collect();
    let (cmd, consumed) = match source.lookup(&words) {
        Some(hit) => hit,
        // Not a command — but it may be a group, in which case naming it
        // (with or without `--help`) should list what lives under it.
        None => match source.group_help(&words) {
            Some(help) => {
                let _ = output.send(PipelineData::Rendered(help)).await;
                return Ok(());
            }
            None => return Err(source.unknown_command_error(&call.words)),
        },
    };

    // `--help` intercepted before binding, so a malformed call still gets help.
    if wants_help(call) {
        let _ = output.send(PipelineData::Rendered(cmd.signature().render_help())).await;
        return Ok(());
    }

    let bound = bind(cmd.signature(), &call.words[consumed..], call, scope)?;
    cmd.run(ctx.clone(), bound, input, output).await
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
            mut input: crate::chan::Receiver,
            output: crate::chan::Sender,
        ) -> LocalBoxFuture<Result<(), ShellError>> {
            let n = call.positionals[0].as_int().unwrap_or(0);
            Box::pin(async move {
                let _ = crate::stream::collect(&mut input).await;
                let _ = crate::stream::flatten(PipelineData::Value(Value::Int(n)), &output).await;
                Ok(())
            })
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
            mut input: crate::chan::Receiver,
            output: crate::chan::Sender,
        ) -> LocalBoxFuture<Result<(), ShellError>> {
            Box::pin(async move {
                let collected = crate::stream::collect(&mut input).await;
                let out = match collected.into_value() {
                    Value::Int(n) => Value::Int(n * 2),
                    other => other,
                };
                let _ = crate::stream::flatten(PipelineData::Value(out), &output).await;
                Ok(())
            })
        }
    }

    /// Always fails, so error propagation through the channel wiring can be
    /// asserted rather than assumed.
    struct Boom;
    impl Command for Boom {
        fn signature(&self) -> &Signature {
            static SIG: std::sync::OnceLock<Signature> = std::sync::OnceLock::new();
            SIG.get_or_init(|| Signature::build("boom", "always fails"))
        }
        fn run(
            &self,
            _ctx: ExecContext,
            _call: BoundCall,
            _input: crate::chan::Receiver,
            _output: crate::chan::Sender,
        ) -> LocalBoxFuture<Result<(), ShellError>> {
            ready(Err(ShellError::runtime("boom")))
        }
    }

    fn registry() -> CommandRegistry {
        let mut r = CommandRegistry::new();
        r.register_builtin(Rc::new(Emit));
        r.register_builtin(Rc::new(Double));
        r.register_builtin(Rc::new(Boom));
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
    fn a_failing_stage_stops_the_pipeline_and_keeps_its_own_error() {
        // The first error wins: a later stage must not overwrite it with a
        // downstream symptom of the same failure.
        let err = eval("emit 1 | boom | double").expect_err("boom should fail");
        assert!(err.msg.contains("boom"), "wrong error survived: {}", err.msg);
    }

    #[test]
    fn a_three_stage_pipeline_threads_values_end_to_end() {
        // Two channels, three stages: proves the wiring is not accidentally
        // correct only for the single-channel case.
        let out = eval("emit 5 | double | double").expect("eval");
        assert_eq!(
            out.into_iter().last().map(PipelineData::into_value),
            Some(Value::Int(20))
        );
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
