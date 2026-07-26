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

/// Receives the last stage's items.
///
/// `eval_pipeline` is engine-agnostic — it cannot paint, because painting
/// needs an `EngineAccess` only the pane path has. So the terminal consumer
/// is pluggable: programmatic `run()`, the CLI, and tests collect into one
/// value; the interactive pane paints progressively.
///
/// `item` is synchronous because painting is (`access.with` is a sync
/// closure) and because a future borrowing `&mut self` cannot be boxed into
/// the `'static` `LocalBoxFuture`. Backpressure is therefore a separate
/// `ready()`, which borrows nothing of `self`.
pub trait FinalConsumer {
    /// One item from the last stage: paint or buffer it now.
    fn item(&mut self, item: PipelineData);

    /// Resolves when the consumer can accept more — the pane throttle.
    /// Default immediate; awaited between items with no borrow held.
    fn ready(&self) -> crate::registry::LocalBoxFuture<()> {
        crate::registry::ready(())
    }

    /// Whether `ready()` is worth awaiting. Default `false`, so a consumer
    /// that cannot fall behind pays no per-item future allocation; the pane's
    /// throttling consumer overrides it.
    fn needs_backpressure(&self) -> bool {
        false
    }

    /// End of stream: the value this pipeline reports.
    fn finish(&mut self) -> PipelineData;
}

/// Collects items into one value, exactly as the previous hardcoded terminal
/// collector did — the behaviour `run()`, the CLI, and every test depend on.
#[derive(Default)]
pub struct CollectingConsumer {
    items: Vec<PipelineData>,
}

impl CollectingConsumer {
    pub fn new() -> Self {
        Self::default()
    }
}

impl FinalConsumer for CollectingConsumer {
    fn item(&mut self, item: PipelineData) {
        self.items.push(item);
    }

    fn finish(&mut self) -> PipelineData {
        let items = std::mem::take(&mut self.items);
        let all_rendered = !items.is_empty()
            && items.iter().all(|i| matches!(i, PipelineData::Rendered(_)));
        if all_rendered {
            let joined = items
                .into_iter()
                .map(|i| match i {
                    PipelineData::Rendered(s) => s,
                    _ => String::new(),
                })
                .collect::<Vec<_>>()
                .join("\n");
            PipelineData::Rendered(joined)
        } else {
            match items.len() {
                0 => PipelineData::Empty,
                1 => items.into_iter().next().expect("len checked above"),
                _ => PipelineData::Value(Value::List(
                    items.into_iter().map(PipelineData::into_value).collect(),
                )),
            }
        }
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
    let mut make = || -> Box<dyn FinalConsumer> { Box::new(CollectingConsumer::new()) };
    eval_line_with(line, source, ctx, scope, &mut make).await
}

/// Like `eval_line`, but the caller supplies the terminal consumer — the
/// pane path passes one that paints progressively. Returns the per-pipeline
/// results the consumer reported, plus the error that stopped a later
/// pipeline, if any.
pub async fn eval_line_with(
    line: &Line,
    source: &impl CommandSource,
    ctx: &ExecContext,
    scope: &Scope,
    make_consumer: &mut dyn FnMut() -> Box<dyn FinalConsumer>,
) -> (Vec<PipelineData>, Option<ShellError>) {
    let mut results = Vec::new();
    for pipeline in &line.pipelines {
        let mut consumer = make_consumer();
        match eval_pipeline(pipeline, source, ctx, scope, consumer.as_mut()).await {
            Ok(()) => results.push(consumer.finish()),
            Err(err) => return (results, Some(err)),
        }
    }
    (results, None)
}

/// Evaluate a pipeline by running every stage as a concurrent future joined
/// by bounded channels: each stage's `Receiver` is the previous stage's
/// `Sender`, so a stage can start consuming as soon as its predecessor sends
/// anything, rather than waiting for it to finish.
///
/// Every builtin today is still a *collecting* command — the `Builtin`
/// adapter drains its whole input before running and flattens its whole
/// result back out — so a fully-collected pipeline's output is unchanged
/// from the sequential version this replaced. A later stage adds streaming
/// commands that read and write item-by-item instead, without needing any
/// change here: they just don't drain before producing.
pub async fn eval_pipeline(
    pipeline: &Pipeline,
    source: &impl CommandSource,
    ctx: &ExecContext,
    scope: &Scope,
    consumer: &mut dyn FinalConsumer,
) -> Result<(), ShellError> {
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

    // Terminal consumer: drain the last stage's output into `consumer`.
    //
    // `eval_pipeline` is engine-agnostic and cannot paint, so what happens to
    // each item is pluggable: `CollectingConsumer` reproduces the old
    // hardcoded collector (below) exactly; the pane's progressive consumer
    // paints each item as it arrives instead. Either way this stays a stage
    // so consumption interleaves with upstream production rather than
    // waiting for it to finish.
    stages.push(Box::pin(async move {
        while let Some(item) = upstream.recv().await {
            consumer.item(item);
            if consumer.needs_backpressure() {
                consumer.ready().await;
            }
        }
    }));

    crate::pipeline::drive(stages).await;

    if let Some(err) = failure.borrow_mut().take() {
        return Err(err);
    }
    Ok(())
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
                send_lines(&help, &output).await;
                return Ok(());
            }
            None => return Err(source.unknown_command_error(&call.words)),
        },
    };

    // `--help` intercepted before binding, so a malformed call still gets help.
    if wants_help(call) {
        send_lines(&cmd.signature().render_help(), &output).await;
        return Ok(());
    }

    let bound = bind(cmd.signature(), &call.words[consumed..], call, scope)?;
    cmd.run(ctx.clone(), bound, input, output).await
}

/// Send trusted, pre-styled text downstream as one `Rendered` item per line,
/// so `cmd --help | grep flag` filters lines. A downstream stage consuming
/// these gets plain `Str` (via `into_value`), which is where the trust drops.
async fn send_lines(text: &str, output: &crate::chan::Sender) {
    for line in text.lines() {
        if output.send(PipelineData::Rendered(line.to_string())).await.is_err() {
            return;
        }
    }
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

    #[test]
    fn help_streams_lines_so_grep_can_filter_them() {
        let mut registry = CommandRegistry::new();
        crate::builtins::register_all(&mut registry);
        let ctx = ExecContext {
            host: Rc::new(NullHost),
            sink: Rc::new(crate::sink::NullSink),
            width: 80,
            pane: 0,
            run_id: 0,
        };
        // `sort-by --help` has a `--reverse` line; grep keeps only it.
        let out = crate::parse::parse("sort-by --help | grep reverse");
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        let (mut results, error) =
            block_on(eval_line(&out.line, &registry, &ctx, &Scope::new()));
        assert!(error.is_none(), "{:?}", error);
        let value = results.pop().map(PipelineData::into_value).unwrap_or(Value::Null);
        let text = match value {
            Value::Str(s) => s,
            Value::List(items) => items.iter().map(|v| v.as_str().unwrap_or("")).collect::<Vec<_>>().join("\n"),
            other => panic!("unexpected: {other:?}"),
        };
        assert!(text.contains("reverse"), "no reverse line: {text:?}");
        assert!(!text.contains("Usage"), "did not filter to one line: {text:?}");
    }

    #[test]
    fn the_consumer_seam_preserves_the_final_value() {
        // Guards the refactor: routing the last stage through a pluggable
        // consumer must not change what a pipeline reports.
        let out = eval("emit 5 | double").expect("eval");
        assert_eq!(
            out.into_iter().last().map(PipelineData::into_value),
            Some(Value::Int(10))
        );
    }

    #[test]
    fn bare_help_still_renders_as_one_block() {
        let mut registry = CommandRegistry::new();
        crate::builtins::register_all(&mut registry);
        let ctx = ExecContext {
            host: Rc::new(NullHost),
            sink: Rc::new(crate::sink::NullSink),
            width: 80,
            pane: 0,
            run_id: 0,
        };
        let out = crate::parse::parse("sort-by --help");
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        let (mut results, error) =
            block_on(eval_line(&out.line, &registry, &ctx, &Scope::new()));
        assert!(error.is_none(), "{:?}", error);
        // A bare --help stays one Rendered block (verbatim, styled), NOT a
        // table of lines.
        match results.pop() {
            Some(PipelineData::Rendered(s)) => {
                assert!(s.contains("Usage"), "help text missing: {s:?}");
                assert!(s.contains("reverse"), "flags missing: {s:?}");
            }
            other => panic!("expected one Rendered block, got {other:?}"),
        }
    }
}
