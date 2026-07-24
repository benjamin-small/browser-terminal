# Streaming Commands (Stage 3) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `map`/`filter`/`grep`/`head` process pipeline items incrementally, so a live source can be consumed and `head 5` terminates it.

**Architecture:** `Command::run` gains `Receiver`/`Sender` handles (Approach B from the spec). The ~30 collecting builtins keep their `fn(ctx, call, PipelineData) -> PipelineData` signature behind an adapter that collects the input stream and flattens the result; only the five transforms get a streaming adapter. A returned `List` is a batch of items, so collected pipelines round-trip to identical output.

**Tech Stack:** Rust (`bterm-core`, no wasm deps, no `futures` crate), `bterm-wasm` (JsCommand), TypeScript demo, Playwright.

**Spec:** [docs/superpowers/specs/2026-07-24-streaming-commands-design.md](../specs/2026-07-24-streaming-commands-design.md)

---

## Context an engineer needs

**The transport already exists.** Stage 2 (merged) built `crates/bterm-core/src/chan.rs` — a bounded async channel — and `crates/bterm-core/src/pipeline.rs::drive` — a concurrent stage driver. `eval_pipeline` in `crates/bterm-core/src/eval.rs` already runs each stage as a future joined by channels; today each stage *collects* its whole input first. This stage stops the collecting where it shouldn't happen.

**The channel API** (`crate::chan`):
- `channel(capacity) -> (Sender, Receiver)`
- `Sender::send(PipelineData) -> impl Future<Output = Result<(), Closed>>`
- `Receiver::recv() -> impl Future<Output = Option<PipelineData>>`
- Dropping the `Receiver` makes the next `send` return `Err(Closed)` — this is how `head` terminates a producer.

**The borrow invariant is unchanged.** Engine state is reached only through synchronous `EngineAccess::with` closures; no borrow crosses `.await`. Nothing in this stage awaits inside such a closure.

**`PipelineData`** (`crate::registry`): `Empty` | `Value(Value)` | `Rendered(String)`. `into_value()` maps `Rendered(s) -> Value::Str(s)` — this is the "trust drops on consumption" rule.

**Clippy** denies `unwrap_used` on `bterm-core`. Comments explain WHY.

**Stage 3 is NOT behaviour-preserving at the edges** (unlike stage 2). Collected/table pipelines must stay byte-identical, but `echo 5 | length`-class edges may change — those test updates are expected.

---

## File Structure

| File | Responsibility |
|---|---|
| `crates/bterm-core/src/stream.rs` | **Create.** `collect(&mut Receiver) -> PipelineData` and `flatten(PipelineData, &Sender)`, the two reconciliation rules. Pure, unit-tested. |
| `crates/bterm-core/src/lib.rs` | **Modify.** Register `pub mod stream;`. |
| `crates/bterm-core/src/registry.rs` | **Modify.** `Command::run` signature. |
| `crates/bterm-core/src/builtins/mod.rs` | **Modify.** `Builtin` collecting adapter; `StreamingBuiltin`; convert 5 transforms. |
| `crates/bterm-core/src/eval.rs` | **Modify.** `eval_call`/`run_stage`/`eval_pipeline` onto the new signature; terminal collector; help interception. |
| `crates/bterm-core/src/engine.rs` | **Modify.** Test fixtures to the new signature. |
| `crates/bterm-wasm/src/js_command.rs` | **Modify.** Collecting bridge first, then generator streaming. |
| `packages/demo/src/main.ts`, `index.html` | **Modify.** `watch` command + Try block. |
| `packages/demo/tests/smoke.spec.ts` | **Modify.** Browser streaming test. |

---

## Task 1: The `stream` module — collect and flatten

**Files:**
- Create: `crates/bterm-core/src/stream.rs`
- Modify: `crates/bterm-core/src/lib.rs`

- [ ] **Step 1: Register the module first**

Add `pub mod stream;` to `crates/bterm-core/src/lib.rs`, alphabetically — it goes after `pub mod sink;` (line 25) and before `pub mod value;`. Do this before the test so a failure is a compile error, not "0 tests".

- [ ] **Step 2: Write the failing test**

Create `crates/bterm-core/src/stream.rs` with only the test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::chan::channel;
    use crate::eval::block_on;
    use crate::registry::PipelineData;
    use crate::value::Value;

    fn int(n: i64) -> PipelineData {
        PipelineData::Value(Value::Int(n))
    }

    #[test]
    fn flatten_sends_list_elements_as_separate_items() {
        // A returned List is a batch: one level is flattened into items.
        let (tx, mut rx) = channel(8);
        block_on(async {
            let list = Value::List(vec![Value::Int(1), Value::Int(2), Value::Int(3)]);
            flatten(PipelineData::Value(list), &tx).await.expect("flatten");
            drop(tx);
            let mut seen = Vec::new();
            while let Some(item) = rx.recv().await {
                seen.push(item.into_value());
            }
            assert_eq!(seen, vec![Value::Int(1), Value::Int(2), Value::Int(3)]);
        });
    }

    #[test]
    fn flatten_is_one_level_only() {
        let (tx, mut rx) = channel(8);
        block_on(async {
            let nested = Value::List(vec![
                Value::List(vec![Value::Int(1), Value::Int(2)]),
                Value::List(vec![Value::Int(3)]),
            ]);
            flatten(PipelineData::Value(nested), &tx).await.expect("flatten");
            drop(tx);
            let mut count = 0;
            while rx.recv().await.is_some() {
                count += 1;
            }
            assert_eq!(count, 2, "outer list flattens; inner lists stay whole");
        });
    }

    #[test]
    fn flatten_scalar_and_empty() {
        let (tx, mut rx) = channel(8);
        block_on(async {
            flatten(int(5), &tx).await.expect("scalar");
            flatten(PipelineData::Empty, &tx).await.expect("empty");
            drop(tx);
            let mut seen = Vec::new();
            while let Some(item) = rx.recv().await {
                seen.push(item.into_value());
            }
            // The scalar is one item; Empty sends nothing.
            assert_eq!(seen, vec![Value::Int(5)]);
        });
    }

    #[test]
    fn collect_gathers_items_back() {
        let (tx, mut rx) = channel(8);
        block_on(async {
            tx.send(int(1)).await.expect("send");
            tx.send(int(2)).await.expect("send");
            drop(tx);
            let collected = collect(&mut rx).await;
            assert_eq!(
                collected.into_value(),
                Value::List(vec![Value::Int(1), Value::Int(2)])
            );
        });
    }

    #[test]
    fn collect_single_item_is_not_wrapped() {
        let (tx, mut rx) = channel(8);
        block_on(async {
            tx.send(int(7)).await.expect("send");
            drop(tx);
            // Exactly one item collects to that value, not a 1-element list —
            // so `echo 5` stays a scalar, not `[5]`.
            assert_eq!(collect(&mut rx).await.into_value(), Value::Int(7));
        });
    }

    #[test]
    fn collect_empty_stream_is_empty() {
        let (tx, mut rx) = channel::<>(8);
        block_on(async {
            drop(tx);
            assert_eq!(collect(&mut rx).await, PipelineData::Empty);
        });
    }
}
```

Note: `channel::<>(8)` in the last test is a typo guard — write `channel(8)`; remove the turbofish. (Fix it as you type; it is here to make you read the line.)

- [ ] **Step 3: Run, verify it fails**

Run: `cargo test -p bterm-core stream::`
Expected: compile error, `cannot find function flatten`.

- [ ] **Step 4: Implement**

Above the test module in `crates/bterm-core/src/stream.rs`:

```rust
//! The two rules that reconcile the streaming transport with commands that
//! still return one whole value.
//!
//! `flatten` runs on the way *out* of a collecting producer; `collect` runs
//! on the way *in* to a collecting consumer. Because both live here, the
//! streaming commands only ever see individual items and never flatten.

use crate::chan::{Closed, Receiver, Sender};
use crate::registry::PipelineData;
use crate::value::Value;

/// Send a producer's result downstream as items. A `List` is a batch: its
/// elements go one at a time (one level only). A scalar is one item; `Empty`
/// sends nothing; `Rendered` text is one item, kept whole.
pub async fn flatten(data: PipelineData, tx: &Sender) -> Result<(), Closed> {
    match data {
        PipelineData::Empty => Ok(()),
        PipelineData::Value(Value::List(items)) => {
            for item in items {
                tx.send(PipelineData::Value(item)).await?;
            }
            Ok(())
        }
        PipelineData::Value(v) => tx.send(PipelineData::Value(v)).await,
        PipelineData::Rendered(s) => tx.send(PipelineData::Rendered(s)).await,
    }
}

/// Gather a whole stream back into one value for a collecting command. N
/// items become a `List`; exactly one item stays that value (unwrapped, so
/// `echo 5` is a scalar, not `[5]`); zero items are `Empty`. `Rendered`
/// items degrade to `Str` via `into_value`, which is where a trusted stream
/// loses its trust on consumption.
pub async fn collect(rx: &mut Receiver) -> PipelineData {
    let mut items: Vec<Value> = Vec::new();
    while let Some(item) = rx.recv().await {
        items.push(item.into_value());
    }
    match items.len() {
        0 => PipelineData::Empty,
        1 => PipelineData::Value(items.into_iter().next().unwrap_or(Value::Null)),
        _ => PipelineData::Value(Value::List(items)),
    }
}
```

Note the `unwrap_or(Value::Null)` guards the `len()==1` branch without an `unwrap` — clippy-safe.

- [ ] **Step 5: Run, verify pass**

Run: `cargo test -p bterm-core stream::`
Expected: 6 passed.

Run: `cargo test --workspace`
Expected: 191 passed (185 existing + 6).

Run: `cargo clippy -p bterm-core --all-targets`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add crates/bterm-core/src/stream.rs crates/bterm-core/src/lib.rs
git commit -m "Add stream::collect and stream::flatten

The two rules that reconcile the streaming transport with collecting
commands: flatten a List into items on the way out, collect items back on
the way in. Living here means streaming commands never flatten."
```

---

## Task 2: Change the `Command` contract (the gate)

**This is the largest task. It is compile-breaking by design and must leave every existing test green** — collected pipelines round-trip to identical output.

**Files:**
- Modify: `crates/bterm-core/src/registry.rs` (trait), `crates/bterm-core/src/builtins/mod.rs` (Builtin adapter), `crates/bterm-core/src/eval.rs` (eval_call/run_stage/eval_pipeline + fixtures), `crates/bterm-core/src/engine.rs` (fixtures), `crates/bterm-wasm/src/js_command.rs` (JsCommand).

- [ ] **Step 1: Change the trait**

In `crates/bterm-core/src/registry.rs`, replace `Command::run`:

```rust
pub trait Command {
    fn signature(&self) -> &Signature;
    /// Read items from `input`, write items to `output`. A collecting
    /// command reads its whole input and writes one result (via the
    /// `Builtin` adapter); a streaming command transforms item by item.
    fn run(
        &self,
        ctx: ExecContext,
        call: BoundCall,
        input: crate::chan::Receiver,
        output: crate::chan::Sender,
    ) -> LocalBoxFuture<Result<(), ShellError>>;
}
```

- [ ] **Step 2: The `Builtin` collecting adapter**

In `crates/bterm-core/src/builtins/mod.rs`, the `RunFn` type is unchanged (`fn(ExecContext, BoundCall, PipelineData) -> Result<PipelineData, ShellError>`). Rewrite `Builtin::run` to bridge:

```rust
impl Command for Builtin {
    fn signature(&self) -> &Signature {
        &self.sig
    }

    fn run(
        &self,
        ctx: ExecContext,
        call: BoundCall,
        mut input: crate::chan::Receiver,
        output: crate::chan::Sender,
    ) -> LocalBoxFuture<Result<(), ShellError>> {
        let run_fn = self.run_fn;
        Box::pin(async move {
            let collected = crate::stream::collect(&mut input).await;
            let result = run_fn(ctx, call, collected)?;
            // Err(Closed) means a downstream `head` stopped reading — not
            // this command's error to report.
            let _ = crate::stream::flatten(result, &output).await;
            Ok(())
        })
    }
}
```

`ready(...)` is no longer used here; leave the `ready` import if other code uses it, else remove to satisfy clippy.

- [ ] **Step 3: `JsCommand` collecting bridge**

In `crates/bterm-wasm/src/js_command.rs`, `JsCommand::run` takes the new signature. For THIS task it stays collecting (generator streaming is Task 6). Change the signature and wrap the existing body:

```rust
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
            let collected = bterm_core::stream::collect(&mut input).await;
            // ... existing body, but `input.into_value()` becomes
            // `collected.into_value()`, and instead of returning
            // `Ok(PipelineData::Value(value))` / `Ok(PipelineData::Empty)`,
            // flatten into `output`:
            //   let result = <PipelineData built from the JS return>;
            //   let _ = bterm_core::stream::flatten(result, &output).await;
            //   Ok(())
        })
    }
```

Concretely: keep the whole existing async body that builds `args`/`ctx_obj`, calls `func`, awaits the promise, and maps errors. Where it currently does `let input_js = value_to_js(&input.into_value());`, use `collected.into_value()`. Where it currently returns `Ok(PipelineData::Empty)` / `Ok(PipelineData::Value(value))`, instead build that `PipelineData`, call `bterm_core::stream::flatten(pd, &output).await` (ignore the `Closed` result), and return `Ok(())`. Error paths still `return Err(...)`.

Add `use bterm_core::chan;` if needed (or fully-qualify as above).

- [ ] **Step 4: Rework `eval_call`**

In `crates/bterm-core/src/eval.rs`, `eval_call` no longer returns `PipelineData`; it drives one command's rx→tx. Replace it:

```rust
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
        None => match source.group_help(&words) {
            // A group page is trusted rendered text; send it as one item.
            Some(help) => {
                let _ = output.send(PipelineData::Rendered(help)).await;
                return Ok(());
            }
            None => return Err(source.unknown_command_error(&call.words)),
        },
    };

    if wants_help(call) {
        let _ = output.send(PipelineData::Rendered(cmd.signature().render_help())).await;
        return Ok(());
    }

    let bound = bind(cmd.signature(), &call.words[consumed..], call, scope)?;
    cmd.run(ctx.clone(), bound, input, output).await
}
```

(Task 5 changes the two `Rendered` sends to per-line; leave them as single blobs here so behaviour is preserved.)

- [ ] **Step 5: Rework `run_stage` and `eval_pipeline`**

Replace `run_stage` (it no longer drains — it just wires rx/tx and records failure):

```rust
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
    // An earlier stage already failed; dropping `input`/`output` here closes
    // this stage's channels so neighbours unblock.
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
```

Replace `eval_pipeline`. It builds a channel per stage, a terminal collector, and drives them all. Remove the single-call fast path — a lone streaming command (`watch`) needs a real channel, and the general path is correct for N=1:

```rust
pub async fn eval_pipeline(
    pipeline: &Pipeline,
    source: &impl CommandSource,
    ctx: &ExecContext,
    scope: &Scope,
) -> Result<PipelineData, ShellError> {
    let n = pipeline.calls.len();
    let outcome: Rc<RefCell<PipelineData>> = Rc::new(RefCell::new(PipelineData::Empty));
    let failure: Rc<RefCell<Option<ShellError>>> = Rc::new(RefCell::new(None));

    // One channel per stage output, plus the first stage's empty input.
    let (_empty_tx, empty_rx) = crate::chan::channel(1);
    drop(_empty_tx); // first stage's input is an already-closed stream

    let mut stages: Vec<crate::pipeline::BoxedStage<'_>> = Vec::new();
    let mut upstream = empty_rx;

    for (idx, call) in pipeline.calls.iter().enumerate() {
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
        let _ = idx;
    }

    // Terminal collector: drain the last stage's output into `outcome`.
    let outcome_for_collector = outcome.clone();
    stages.push(Box::pin(async move {
        let collected = crate::stream::collect(&mut upstream).await;
        *outcome_for_collector.borrow_mut() = collected;
    }));

    crate::pipeline::drive(stages).await;

    if let Some(err) = failure.borrow_mut().take() {
        return Err(err);
    }
    let result = std::mem::replace(&mut *outcome.borrow_mut(), PipelineData::Empty);
    Ok(result)
}
```

`STAGE_BUFFER` (the `const … = 64;` from stage 2) stays. Delete the old `outcome: Option<Rc<...>>` plumbing and the `debug_assert!(count <= 1)` drain (it lived in the old `run_stage`).

- [ ] **Step 6: Update the 6 test fixtures**

Each hand-written `impl Command` fixture takes the new signature. They are collecting, so each uses `collect`/`flatten`. Full replacements:

`crates/bterm-core/src/eval.rs` — `Emit`, `Double`, `Boom`:

```rust
    impl Command for Emit {
        fn signature(&self) -> &Signature { /* unchanged */ }
        fn run(
            &self,
            _ctx: ExecContext,
            call: BoundCall,
            mut input: crate::chan::Receiver,
            output: crate::chan::Sender,
        ) -> LocalBoxFuture<Result<(), ShellError>> {
            Box::pin(async move {
                let _ = crate::stream::collect(&mut input).await; // emit ignores input
                let n = call.positionals[0].as_int().unwrap_or(0);
                let _ = crate::stream::flatten(PipelineData::Value(Value::Int(n)), &output).await;
                Ok(())
            })
        }
    }

    impl Command for Double {
        fn signature(&self) -> &Signature { /* unchanged */ }
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

    impl Command for Boom {
        fn signature(&self) -> &Signature { /* unchanged */ }
        fn run(
            &self,
            _ctx: ExecContext,
            _call: BoundCall,
            _input: crate::chan::Receiver,
            _output: crate::chan::Sender,
        ) -> LocalBoxFuture<Result<(), ShellError>> {
            Box::pin(async move { Err(ShellError::runtime("boom")) })
        }
    }
```

Keep each `fn signature(&self)` body exactly as it is now — only the `run` bodies and signatures change. The test module needs `use crate::value::Value;` (likely already imported) and the async blocks; remove the now-unused `ready` import from the test module if clippy flags it.

`crates/bterm-core/src/engine.rs` — `Noisy` and `Borrower`: same transformation. `Noisy` writes to `ctx.sink` then flattens `PipelineData::Empty` (or its `Value::Int(1)`) to `output`. `Borrower` keeps its poll-loop body, then flattens `Value::Int(1)` to `output`. `crates/bterm-core/src/builtins/mod.rs` — its `Noisy` fixture: same.

For each fixture, the pattern is identical: `Box::pin(async move { <collect input if used>; <existing logic>; flatten(result, &output).await; Ok(()) })`.

- [ ] **Step 7: The gate — full suite green**

Run: `cargo test --workspace`
Expected: **191 passed, with the SAME test names as before** (Task 1 added 6; no test removed). If a collected/table pipeline test fails, the round-trip is broken — fix the adapter, not the test.

Note: one or two edge tests may now behave differently (`echo 5 | length`, etc.). If such a test fails, verify by hand whether the new behaviour is *correct per the batch model* (e.g. `length` of a one-item stream). If correct, update the test WITH A COMMENT explaining the batch-model change. If you cannot justify it, the adapter is wrong. Report every test you change and why.

Run: `cargo clippy --workspace --all-targets`
Expected: clean.

Run the CLI parity check:
```bash
printf "echo a b c | str upcase\necho '[{\"n\":3},{\"n\":1}]' | from json | sort-by n | to json\nlinks --help\nmux\n" | cargo run -q -p bterm-cli 2>&1 | tail -20
```
Expected: identical to before this task (upcased list, sorted JSON, `links` help, `mux` group page). The CLI has no `links`; use `echo`/`sort-by`/`str`/`mux` which exist. Report the output.

- [ ] **Step 8: Commit**

```bash
git add crates/
git commit -m "Command::run gains Receiver/Sender; collecting commands bridge

Approach B: the ~30 collecting builtins keep their fn signature behind an
adapter that collects the input stream and flattens the result, so
collected pipelines round-trip to identical output -- the existing suite
passing is the gate. eval_pipeline builds a channel per stage plus a
terminal collector; the single-call fast path is gone because a lone
streaming command needs a real channel.

Streaming commands and generator-based JsCommands arrive in later tasks;
here JsCommand also bridges via collect/flatten."
```

---

## Task 3: `StreamingBuiltin` + `head` streams

> Note: there is no `take` command — only `head` and `tail` exist. `head`
> becomes streaming here; `tail` stays collecting (it needs the whole input
> to know the last N).

**Files:**
- Modify: `crates/bterm-core/src/builtins/mod.rs`

- [ ] **Step 1: Write the failing test**

In the `#[cfg(test)] mod tests` block in `crates/bterm-core/src/builtins/mod.rs`, add a streaming producer fixture and a termination test. The producer counts how many items it sent, so we can assert `head` stopped it:

```rust
    /// Emits ints 1.. forever, recording how many it managed to send. A
    /// downstream `head` closing the channel is what stops it.
    struct Counter(std::rc::Rc<std::cell::Cell<i64>>);
    impl Command for Counter {
        fn signature(&self) -> &Signature {
            static SIG: std::sync::OnceLock<Signature> = std::sync::OnceLock::new();
            SIG.get_or_init(|| Signature::build("counter", "emits forever"))
        }
        fn run(
            &self,
            _ctx: ExecContext,
            _call: BoundCall,
            _input: crate::chan::Receiver,
            output: crate::chan::Sender,
        ) -> crate::registry::LocalBoxFuture<Result<(), ShellError>> {
            let sent = self.0.clone();
            Box::pin(async move {
                let mut n = 0i64;
                loop {
                    n += 1;
                    if output.send(PipelineData::Value(Value::Int(n))).await.is_err() {
                        sent.set(n - 1); // the last send failed: head closed us
                        return Ok(());
                    }
                }
            })
        }
    }

    #[test]
    fn head_terminates_an_infinite_producer() {
        let sent = std::rc::Rc::new(std::cell::Cell::new(0));
        let mut registry = CommandRegistry::new();
        register_all(&mut registry);
        registry.register_builtin(Rc::new(Counter(sent.clone())));

        let ctx = ExecContext {
            host: Rc::new(TestHost),
            sink: Rc::new(crate::sink::NullSink),
            width: 80,
            pane: 0,
            run_id: 0,
        };
        let out = parse("counter | head 3");
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        let (mut results, error) =
            block_on(eval_line(&out.line, &registry, &ctx, &Scope::new()));
        assert!(error.is_none(), "{:?}", error);
        assert_eq!(
            results.pop().map(PipelineData::into_value),
            Some(Value::List(vec![Value::Int(1), Value::Int(2), Value::Int(3)]))
        );
        // The producer must have stopped, not run away. With STAGE_BUFFER=64
        // it may get up to a bufferful ahead, but it must be bounded and
        // must have observed the close.
        assert!(sent.get() >= 3, "produced too few: {}", sent.get());
        assert!(sent.get() < 1000, "producer did not stop: {}", sent.get());
    }
```

- [ ] **Step 2: Run, verify it fails**

Run: `cargo test -p bterm-core builtins::tests::head_terminates`
Expected: FAIL — `head` is still collecting, so it drains `counter` forever (the test will hang). **Run it with a timeout** so you observe the hang without wedging your session:

```bash
timeout 15 cargo test -p bterm-core builtins::tests::head_terminates ; echo "exit: $?"
```
Expected: `exit: 124` (timed out) — confirming `head` collects today.

- [ ] **Step 3: Add the streaming adapter and convert `head`**

Add near `Builtin`:

```rust
/// A command that transforms its input stream item by item.
type StreamFn = fn(
    ExecContext,
    BoundCall,
    crate::chan::Receiver,
    crate::chan::Sender,
) -> crate::registry::LocalBoxFuture<Result<(), ShellError>>;

struct StreamingBuiltin {
    sig: Signature,
    run_fn: StreamFn,
}

impl Command for StreamingBuiltin {
    fn signature(&self) -> &Signature {
        &self.sig
    }
    fn run(
        &self,
        ctx: ExecContext,
        call: BoundCall,
        input: crate::chan::Receiver,
        output: crate::chan::Sender,
    ) -> crate::registry::LocalBoxFuture<Result<(), ShellError>> {
        (self.run_fn)(ctx, call, input, output)
    }
}

fn streaming(sig: Signature, run_fn: StreamFn) -> Rc<dyn Command> {
    Rc::new(StreamingBuiltin { sig, run_fn })
}
```

Replace the collecting `head` fn with a streaming one, and register it via `streaming(...)`:

```rust
fn head(
    _ctx: ExecContext,
    call: BoundCall,
    mut input: crate::chan::Receiver,
    output: crate::chan::Sender,
) -> crate::registry::LocalBoxFuture<Result<(), ShellError>> {
    Box::pin(async move {
        let n = take_n(&call)?;
        let limit = n.unwrap_or(1);
        let mut taken = 0usize;
        while taken < limit {
            match input.recv().await {
                Some(item) => {
                    // Err(Closed) downstream ends us early too.
                    if output.send(item).await.is_err() {
                        return Ok(());
                    }
                    taken += 1;
                }
                None => break, // upstream ended before we hit the limit
            }
        }
        // Dropping `input` here closes the upstream, stopping the producer.
        Ok(())
    })
}
```

Find `head`'s `register_builtin(cmd(...))` line (the `Signature::build("head", …)` registration) and change `cmd(` to `streaming(`.

**`tail` stays collecting** — it needs the whole input to know the last N. Leave `tail` as a `cmd(...)` collecting fn, unchanged.

Note the behaviour change: `head` with no arg previously returned the first item *unwrapped* (a scalar); now it emits one item, and the terminal collector unwraps a single item to that scalar — same result. Verify `head` with no arg still yields the first element, not a 1-list.

- [ ] **Step 4: Run, verify pass**

Run: `cargo test -p bterm-core builtins::tests::head_terminates`
Expected: PASS, promptly (no timeout).

Run: `cargo test --workspace`
Expected: 192 passed (191 + 1). Any pre-existing `head`/`tail` test must still pass.

Run: `cargo clippy --workspace --all-targets` — clean.

- [ ] **Step 5: Commit**

```bash
git add crates/bterm-core/src/builtins/mod.rs
git commit -m "Stream head; add a StreamingBuiltin adapter

head now reads N items and stops, dropping its receiver -- which closes
the upstream and terminates an infinite producer, proven by a counter
fixture that records where it was stopped. tail stays collecting: it
needs the whole input to know the last N."
```

---

## Task 4: Stream `map`/`filter`/`grep`

**Files:**
- Modify: `crates/bterm-core/src/builtins/mod.rs`

- [ ] **Step 1: Write the failing test**

A `filter` between the infinite `counter` (from Task 3) and `head` must not collect. Add:

```rust
    #[test]
    fn filter_between_source_and_head_does_not_collect() {
        let sent = std::rc::Rc::new(std::cell::Cell::new(0));
        let mut registry = CommandRegistry::new();
        register_all(&mut registry);
        registry.register_builtin(Rc::new(Counter(sent.clone())));

        let ctx = ExecContext {
            host: Rc::new(TestHost),
            sink: Rc::new(crate::sink::NullSink),
            width: 80,
            pane: 0,
            run_id: 0,
        };
        // Keep evens, take 3 -> 2,4,6. If filter collected, this hangs.
        let out = parse("counter | filter {|n| $n % 2 == 0} | head 3");
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        let (mut results, error) =
            block_on(eval_line(&out.line, &registry, &ctx, &Scope::new()));
        assert!(error.is_none(), "{:?}", error);
        assert_eq!(
            results.pop().map(PipelineData::into_value),
            Some(Value::List(vec![Value::Int(2), Value::Int(4), Value::Int(6)]))
        );
        assert!(sent.get() < 1000, "producer did not stop: {}", sent.get());
    }
```

- [ ] **Step 2: Run, verify it hangs (times out)**

Run: `timeout 15 cargo test -p bterm-core builtins::tests::filter_between ; echo "exit: $?"`
Expected: `exit: 124` — `filter` still collects.

- [ ] **Step 3: Convert `map`, `filter`, `grep` to streaming**

Each becomes a `StreamFn` looping over `input.recv()`. The per-item logic is the same computation the current code applies to each list element. `grep` compiles its pattern once before the loop.

`map`:

```rust
fn map(
    ctx: ExecContext,
    call: BoundCall,
    mut input: crate::chan::Receiver,
    output: crate::chan::Sender,
) -> crate::registry::LocalBoxFuture<Result<(), ShellError>> {
    Box::pin(async move {
        let selector = positional_selector(&ctx, &call, "selector")?;
        while let Some(item) = input.recv().await {
            let projected = project(&selector, &item.into_value(), call.head_span)?;
            if output.send(PipelineData::Value(projected)).await.is_err() {
                return Ok(());
            }
        }
        Ok(())
    })
}
```

`filter`:

```rust
fn filter(
    ctx: ExecContext,
    call: BoundCall,
    mut input: crate::chan::Receiver,
    output: crate::chan::Sender,
) -> crate::registry::LocalBoxFuture<Result<(), ShellError>> {
    Box::pin(async move {
        let selector = positional_selector(&ctx, &call, "predicate")?;
        let invert = call.has_flag("invert");
        while let Some(item) = input.recv().await {
            let value = item.into_value();
            let verdict = is_truthy(&project(&selector, &value, call.head_span)?);
            if verdict != invert && output.send(PipelineData::Value(value)).await.is_err() {
                return Ok(());
            }
        }
        Ok(())
    })
}
```

`grep`:

```rust
fn grep(
    ctx: ExecContext,
    call: BoundCall,
    mut input: crate::chan::Receiver,
    output: crate::chan::Sender,
) -> crate::registry::LocalBoxFuture<Result<(), ShellError>> {
    Box::pin(async move {
        let pattern_src = call.positionals[0].as_str().unwrap_or_default().to_string();
        let case_insensitive = call.has_flag("ignore-case");
        let invert = call.has_flag("invert");
        let selector = on_selector(&ctx, &call)?;
        let pattern = ctx
            .host
            .compile_pattern(&pattern_src, case_insensitive)
            .map_err(|msg| {
                ShellError::new(
                    ErrorKind::Binding,
                    format!("invalid {} pattern `{pattern_src}`: {msg}", ctx.host.pattern_dialect()),
                )
                .with_span(call.head_span)
            })?;
        let keep = |text: &str| pattern.is_match(text) != invert;

        while let Some(item) = input.recv().await {
            let value = item.into_value();
            let hit = match (&selector, &value) {
                (Some(sel), _) => keep(&plain(&project(sel, &value, call.head_span)?)),
                (None, Value::Record(map)) => {
                    map.values().any(|v| pattern.is_match(&plain(v))) != invert
                }
                (None, scalar) => keep(&plain(scalar)),
            };
            if hit && output.send(PipelineData::Value(value)).await.is_err() {
                return Ok(());
            }
        }
        Ok(())
    })
}
```

Register all three via `streaming(...)` instead of `cmd(...)`.

**Behaviour notes to preserve:**
- The old `map`/`filter`/`grep` called `check_field_exists` on the whole list up front to turn an unknown column into an error rather than an empty result. In the streaming model there is no whole list. **Drop the up-front check**; an unknown field surfaces per-item as `project` returns `Null` or errors, consistent with how `map` already treats a missing field (it errors in `project`). Confirm the existing `map_unknown_field_errors_rather_than_yielding_nulls` test still passes; if it depended on the up-front check, update it with a comment that streaming checks per-item.
- The old `grep` on a `Str` input split it into lines. In the batch model a multi-line `Str` arriving as one item is *not* auto-split (that was list-specific behaviour on a whole-string input). `cmd --help | grep` gets its per-line behaviour from Task 5 (help emits line items), not from grep splitting. If a test asserted `grep` splitting a bare string's lines, update it with a comment pointing at Task 5.

- [ ] **Step 4: Run, verify pass**

Run: `cargo test -p bterm-core builtins::tests::filter_between`
Expected: PASS promptly.

Run: `cargo test --workspace`
Expected: green. Report any test you had to change and why (the two behaviour notes above are the likely ones).

Run: `cargo clippy --workspace --all-targets` — clean.

- [ ] **Step 5: Commit**

```bash
git add crates/bterm-core/src/builtins/mod.rs
git commit -m "Stream map/filter/grep

They now transform item by item, so a filter between a live source and
head passes rows through instead of collecting -- proven by a filter over
the infinite counter terminating at head 3. The old up-front
unknown-field check goes away (there's no whole list to check); a missing
field still errors per item in project."
```

---

## Task 5: `--help` emits a line stream

**Files:**
- Modify: `crates/bterm-core/src/eval.rs`

- [ ] **Step 1: Write the failing test**

`cmd --help | grep <flag>` should filter help lines. Using the CLI-registered builtins in a native test — `sort-by --help` has a `--reverse` flag line. Add to the `#[cfg(test)] mod tests` block in `crates/bterm-core/src/eval.rs` (which has the `Emit`/`Double` registry; you need the real builtins here, so build a registry with `crate::builtins::register_all`):

```rust
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
        // Grep the help for a word that appears on exactly one line.
        let out = crate::parse::parse("sort-by --help | grep reverse");
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        let (mut results, error) =
            block_on(eval_line(&out.line, &registry, &ctx, &Scope::new()));
        assert!(error.is_none(), "{:?}", error);
        let value = results.pop().map(PipelineData::into_value).unwrap_or(Value::Null);
        // grep over the help line-stream keeps only matching lines, as
        // plain Str (trust dropped on consumption).
        let text = match value {
            Value::Str(s) => s,
            Value::List(items) => items.iter().map(|v| v.as_str().unwrap_or("")).collect::<Vec<_>>().join("\n"),
            other => panic!("unexpected: {other:?}"),
        };
        assert!(text.contains("reverse"), "no reverse line: {text:?}");
        assert!(!text.contains("Usage"), "did not filter to one line: {text:?}");
    }
```

`NullHost` in that test module returns substring matching for `compile_pattern` by default (the `HostHooks` default). Confirm `NullHost` exists in the `eval.rs` test module (it does).

- [ ] **Step 2: Run, verify it fails**

Run: `cargo test -p bterm-core eval::tests::help_streams_lines`
Expected: FAIL — help is one `Rendered` blob, so `grep` sees a single Str item containing all lines and matches the whole thing (the `!contains("Usage")` assertion fails).

- [ ] **Step 3: Emit help as per-line `Rendered` items**

In `eval_call` (from Task 2), replace the two single-blob `Rendered` sends with a per-line helper. Add a small async helper in `eval.rs`:

```rust
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
```

Change the group-help and `wants_help` branches in `eval_call` to `send_lines(&help, &output).await;` and `send_lines(&cmd.signature().render_help(), &output).await;` respectively.

- [ ] **Step 4: The terminal render must still show help unchanged**

A bare `cmd --help` (no pipe) now produces a stream of `Rendered` line items. The terminal collector (`stream::collect`) turns N `Rendered` items into a `List` of `Str` via `into_value` — which would render as a *table*, not as help text. That is wrong for the no-pipe case.

Fix the terminal render path, not `collect`. In `crates/bterm-core/src/engine.rs`, the render loop turns the final `PipelineData` into pane output. Find where a pipeline result is rendered (search `render(` and `PipelineData::Rendered`). The collector now yields `Value::List` of `Str` for multi-line help. To preserve the look, the **terminal collector should special-case an all-`Rendered` stream**: if every item collected was `Rendered`, join them with `\n` and keep the result as `PipelineData::Rendered` (printed verbatim) rather than a `List`.

Update `stream::collect` to preserve the tag when the whole stream is `Rendered`:

```rust
pub async fn collect(rx: &mut Receiver) -> PipelineData {
    let mut values: Vec<Value> = Vec::new();
    let mut rendered: Vec<String> = Vec::new();
    let mut all_rendered = true;
    while let Some(item) = rx.recv().await {
        match &item {
            PipelineData::Rendered(s) => rendered.push(s.clone()),
            _ => all_rendered = false,
        }
        values.push(item.into_value());
    }
    if values.is_empty() {
        return PipelineData::Empty;
    }
    // A pure help/text stream reaching the end stays trusted text, printed
    // verbatim -- so `cmd --help` looks the same as before. The moment a
    // non-text stage consumes it, `into_value` has already dropped the tag.
    if all_rendered {
        return PipelineData::Rendered(rendered.join("\n"));
    }
    match values.len() {
        1 => PipelineData::Value(values.into_iter().next().unwrap_or(Value::Null)),
        _ => PipelineData::Value(Value::List(values)),
    }
}
```

This means Task 1's `collect` tests that fed only `Value` items still pass; add one test in `stream.rs` that an all-`Rendered` stream collects to a single joined `Rendered`.

- [ ] **Step 5: Run, verify pass**

Run: `cargo test -p bterm-core eval::tests::help_streams_lines stream::`
Expected: all pass.

Run: `cargo test --workspace`
Expected: green — including the existing `--help` rendering test (`sort-by --help` still shows Usage). Report any changed test.

Run the CLI:
```bash
printf "sort-by --help\nsort-by --help | grep reverse\n" | cargo run -q -p bterm-cli 2>&1 | tail -20
```
Expected: first shows the full styled help; second shows only the `--reverse` line.

- [ ] **Step 6: Commit**

```bash
git add crates/bterm-core/src/eval.rs crates/bterm-core/src/stream.rs
git commit -m "Help output streams one line per item

cmd --help now emits a Rendered item per line, so `cmd --help | grep flag`
filters lines and grep's output is plain Str (trust dropped on
consumption). A bare cmd --help still prints verbatim: collect keeps an
all-Rendered stream as one joined Rendered rather than rendering it as a
table."
```

---

## Task 6: `JsCommand` streams generators

**Files:**
- Modify: `crates/bterm-wasm/src/js_command.rs`

- [ ] **Step 1: Detect and drive an async iterable**

A TS command may return an async generator (`Symbol.asyncIterator`). When it does, iterate it and `flatten`/`send` each yielded value as an item; when a downstream `head` closes the channel, call the iterator's `return()` so the generator's `finally` runs and `ctx.signal` fires.

Replace the collecting-only body from Task 2. After awaiting the resolved return value, branch:

```rust
            // (existing code builds args/ctx_obj, calls func, awaits the
            // resolved promise into `resolved: JsValue`)

            // Streaming: an async iterable yields items over time.
            let async_iter_sym = js_sys::Symbol::async_iterator();
            let iter_fn = js_sys::Reflect::get(&resolved, async_iter_sym.as_ref()).ok();
            let is_async_iterable = iter_fn
                .as_ref()
                .is_some_and(|f| f.is_function());

            if is_async_iterable {
                let iterator = js_sys::Reflect::get(&resolved, async_iter_sym.as_ref())
                    .and_then(|f| js_sys::Function::from(f).call0(&resolved))
                    .map_err(|e| js_error_to_shell(&e, span, &name))?;
                let next_fn = js_sys::Reflect::get(&iterator, &JsValue::from_str("next"))
                    .map(js_sys::Function::from)
                    .map_err(|e| js_error_to_shell(&e, span, &name))?;
                loop {
                    let step = wasm_bindgen_futures::JsFuture::from(
                        js_sys::Promise::resolve(
                            &next_fn.call0(&iterator).map_err(|e| js_error_to_shell(&e, span, &name))?,
                        ),
                    )
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
                    if output.send(PipelineData::Value(value)).await.is_err() {
                        // Downstream closed (e.g. `head`): stop the generator
                        // so its finally runs and ctx.signal has fired.
                        if let Ok(ret) = js_sys::Reflect::get(&iterator, &JsValue::from_str("return")) {
                            if ret.is_function() {
                                let _ = js_sys::Function::from(ret).call0(&iterator);
                            }
                        }
                        break;
                    }
                }
                drop((log, err, emit));
                return Ok(());
            }

            // Not streaming: the existing collecting path (array -> one List,
            // scalar -> one value), flattened into `output`.
            drop((log, err, emit));
            if resolved.is_undefined() {
                return Ok(());
            }
            let value = js_to_value(&resolved)
                .map_err(|msg| ShellError::runtime(format!("`{name}`: {msg}")).with_span(span))?;
            let _ = bterm_core::stream::flatten(PipelineData::Value(value), &output).await;
            Ok(())
```

**`ctx.signal` fires on downstream close:** it already fires on Ctrl-C/abort via `tasks::signal_for(run_id)`. The `output.send(...).is_err()` path above stops iterating, and the generator's `return()`/`finally` runs. To also fire the AbortSignal on early close (so a `fetch`/listener in the generator is cancelled even if it is not currently at a `yield`), abort this run's controller when `send` fails: call the same teardown Ctrl-C uses. Check `crate::tasks` for an `abort(run_id)` helper; if present, call it before `break`. If not, the `return()` call is sufficient for generators that clean up in `finally`; note the limitation in your report rather than inventing an API.

- [ ] **Step 2: Verify it builds**

Run: `just build`
Expected: succeeds. This path is wasm-only; native tests do not cover it. It is exercised in Task 7's browser test.

- [ ] **Step 3: Commit**

```bash
git add crates/bterm-wasm/src/js_command.rs
git commit -m "JsCommand streams async generators

A TS command returning an async iterable has its yields sent as items;
when a downstream head closes the channel, the iterator's return() runs so
the generator's finally fires. A plain array return still collects to one
List, so existing TS commands are unaffected."
```

---

## Task 7: `watch` demo command + browser verification

**Files:**
- Modify: `packages/demo/src/main.ts`, `packages/demo/index.html`, `packages/demo/tests/smoke.spec.ts`

- [ ] **Step 1: Add the `watch` command**

In `packages/demo/src/main.ts`, register a streaming DOM-event command inside `main()`:

```ts
  // #region watch
  // A live source: streams DOM events as they happen. `head N` closing the
  // stream removes the listener, so `watch click | head 3` stops after three
  // clicks and leaves no listener behind.
  bt.registerCommand(
    {
      name: 'watch',
      summary: 'Stream DOM events off the page (Ctrl-C or a downstream `head` stops it)',
      optional: [{ name: 'event', shape: 'str', desc: 'event type, default click' }],
    },
    async function* ({ positionals }, _input, ctx) {
      const type = String(positionals[0] ?? 'click');
      const queue: unknown[] = [];
      let wake: (() => void) | null = null;
      const onEvent = (e: Event) => {
        queue.push({ type: e.type, target: (e.target as Element)?.tagName ?? '' });
        wake?.();
      };
      document.addEventListener(type, onEvent);
      // Tear down on abort: Ctrl-C, dispose, or a downstream head closing us.
      ctx.signal.addEventListener('abort', () => {
        document.removeEventListener(type, onEvent);
        wake?.();
      });
      try {
        while (!ctx.signal.aborted) {
          if (queue.length === 0) {
            await new Promise<void>((r) => (wake = r));
            continue;
          }
          yield queue.shift();
        }
      } finally {
        document.removeEventListener(type, onEvent);
      }
    },
  );
  // #endregion
```

- [ ] **Step 2: Show it in the Try block**

In `packages/demo/index.html`, add to the Try `<pre>`:

```
watch click | map @type | head 3   # click 3 times; stops, listener removed
```

Also add a `codePanel('A live streaming source (watch)', selfSource, 'watch')` call next to the existing `codePanel(...)` calls in `main.ts`.

- [ ] **Step 3: Browser test**

Add to `packages/demo/tests/smoke.spec.ts`:

```ts
test('watch streams DOM events and head terminates it', async ({ page }) => {
  await page.goto('/');
  await waitForTerminal(page);

  // Start `watch click | head 3` programmatically; it resolves once three
  // clicks have streamed through and head closes the source.
  const done = page.evaluate(() =>
    window.bt.run('watch click | map @type | head 3').then((r) => r.value),
  );

  // Give the pipeline a beat to register the listener, then click 4 times.
  await page.waitForTimeout(200);
  for (let i = 0; i < 4; i++) {
    await page.mouse.click(10, 10);
    await page.waitForTimeout(30);
  }

  const value = await done;
  expect(value).toEqual(['click', 'click', 'click']);

  // The listener must be gone: a further click does not grow anything.
  // Re-running proves the source still works (no leaked state).
  const again = page.evaluate(() =>
    window.bt.run('watch click | head 1').then((r) => r.value),
  );
  await page.waitForTimeout(100);
  await page.mouse.click(10, 10);
  expect(await again).toHaveLength(1);
});
```

- [ ] **Step 4: Build and verify**

```bash
just build
npm --prefix packages/demo run build
cd packages/demo && npx playwright test
```
Expected: all pass — the existing 11 plus the new one (12).

Then the wasm size:
```bash
ls -l packages/browser-terminal/dist/wasm/bterm_wasm_bg.wasm | awk '{print $5}'
```
Pre-stage-3 size is 447388 bytes. Report the delta. If it moved by more than 2KB, update `README.md` (`Current wasm size:`) and the spec's size note.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "watch: a live streaming demo source, and browser proof

watch <event> streams DOM events; watch click | map @type | head 3 stops
after three clicks and removes its listener, verified in a real browser.
Records the wasm size after streaming."
```

---

## Self-review notes

**Spec coverage:** the stream model (Task 1 helpers + Task 2 adapter); the `Command` contract / Approach B (Task 2); which commands stream (Tasks 3–4); help as a line stream (Task 5); `JsCommand` generators + `ctx.signal` on close (Task 6); the `watch` demo and browser proof (Task 7); the borrow-invariant re-test — **gap: add it.** The spec says the stage-2 borrow test becomes falsifiable now that stages overlap. Add a step to Task 4: after streaming lands, re-run `concurrent_stages_never_hold_overlapping_engine_borrows` and confirm it still passes with genuinely-overlapping streaming stages; if the fixture needs to become a streaming command to truly overlap, update it and its doc comment (removing the "cannot currently fail" hedge from the stage-2 fix).

**Not behaviour-preserving edges:** Task 2 Step 7, Task 4 Step 3, and Task 5 all call out specific tests that may change, each requiring a justifying comment. Any *table* pipeline changing is a real regression, not an edge.

**Known thin spot:** Task 6's `ctx.signal`-fires-on-downstream-close depends on a `tasks::abort(run_id)` helper that may not exist; the plan says to check and report rather than invent one. If it is missing, the generator's `finally`/`return()` still tears down listeners, so the demo works; only an in-flight `fetch` inside a generator between yields would miss the abort. Flag it for a follow-up.

**Deferred, matching the spec:** progressive rendering (stage 5) — an infinite source without `head` never renders; pane self-throttling (stage 6).
