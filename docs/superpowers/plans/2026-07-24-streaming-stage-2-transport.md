# Streaming Stage 2: Transport Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the pipeline's sequential await-chain with a bounded channel and a concurrent stage driver, changing no observable behaviour.

**Architecture:** A hand-rolled bounded MPSC channel connects stages; `eval_pipeline` becomes a single future that owns and polls all N stage futures round-robin, rather than awaiting them one at a time. Every command still collects its whole input, so output is byte-for-byte identical — the existing test suite passing untouched is the gate.

**Tech Stack:** Rust (`bterm-core`, no wasm deps, no `futures` crate — `std::future::poll_fn` only).

**Scope:** Stage 2 of six from [the spec](../specs/2026-07-23-output-channels-and-streaming-design.md). Stage 1 (channels/sink) is merged. This stage adds **no streaming commands, no `Command` trait change, and no user-visible behaviour**. Streaming commands are stage 3.

---

## Why this stage exists, and why it is shaped this way

The risky parts of streaming are the transport: wakers, backpressure, early termination, and whether concurrent stages can violate the `with_engine` borrow invariant. Landing them while behaviour is provably identical means a regression is unambiguous — if any of the 174 existing tests change, the transport is wrong.

**Three facts about this codebase that shape the design:**

1. **`Command` has only 6 implementors**, and just 2 in production: `Builtin` (`crates/bterm-core/src/builtins/mod.rs:21`, which wraps ~30 plain `fn` pointers) and `JsCommand` (`crates/bterm-wasm/src/js_command.rs:22`). The other 4 are test fixtures. **This plan changes none of them.** The driver collects a stage's input from the channel, calls `cmd.run` with today's signature, and sends the single result onward. Stage 3 changes the trait when it has a reason to.

2. **`block_on` uses a no-op waker** (`crates/bterm-core/src/eval.rs:115-137`). Wakes are discarded natively. A driver that polled each child once per poll would hang: if stage 1 sends *after* stage 2 has already returned `Pending` this round, nothing would ever re-poll stage 2. The driver must therefore detect "something woke during this poll" itself rather than trusting the executor.

3. **The borrow invariant survives concurrency, and the plan must prove it rather than assume it.** `with_engine` is a synchronous closure that never awaits inside, and JS is cooperatively scheduled with no preemption — so two borrows cannot overlap however many stage futures are live. Task 6 adds a test that would catch a violation.

**What concurrency buys in *this* stage: nothing observable.** Every command collects, so stage 2 cannot start until stage 1 finishes; the stages serialize on data dependency. The machinery is exercised and proven, not yet exploited. That also means real pipelines will not stress the channel's bounded buffer, so the channel needs thorough unit tests of its own (Tasks 1–2) rather than relying on integration coverage.

---

## File Structure

| File | Responsibility |
|---|---|
| `crates/bterm-core/src/chan.rs` | **Create.** Bounded MPSC channel: `channel()`, `Sender`, `Receiver`, close semantics. Knows nothing about pipelines. |
| `crates/bterm-core/src/pipeline.rs` | **Create.** The stage driver: polls N futures to completion with its own wake-detection. Knows nothing about commands. |
| `crates/bterm-core/src/lib.rs` | **Modify.** Register both modules. |
| `crates/bterm-core/src/eval.rs` | **Modify.** `eval_pipeline` rewired onto the two above. `eval_call` unchanged. |

Two new files rather than one because they are independently testable and have no dependency on each other — the driver takes futures, the channel carries values.

---

## Task 1: The bounded channel — send, receive, capacity

**Files:**
- Create: `crates/bterm-core/src/chan.rs`
- Modify: `crates/bterm-core/src/lib.rs`

- [ ] **Step 1: Register the module first**

Add `pub mod chan;` to `crates/bterm-core/src/lib.rs`, alphabetically (after `pub mod callable;`, before `pub mod editor;`). Do this **before** writing the test — otherwise the module is not part of the crate and `cargo test` reports "0 tests" instead of a compile error, and you cannot observe a real failure.

- [ ] **Step 2: Write the failing test**

Create `crates/bterm-core/src/chan.rs` with only this test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::block_on;
    use crate::registry::PipelineData;
    use crate::value::Value;

    fn item(n: i64) -> PipelineData {
        PipelineData::Value(Value::Int(n))
    }

    #[test]
    fn items_arrive_in_order_then_the_stream_ends() {
        let (tx, mut rx) = channel(4);
        block_on(async {
            tx.send(item(1)).await.expect("send 1");
            tx.send(item(2)).await.expect("send 2");
            drop(tx);
            assert_eq!(rx.recv().await, Some(item(1)));
            assert_eq!(rx.recv().await, Some(item(2)));
            // Sender dropped and buffer drained: end of stream, not pending.
            assert_eq!(rx.recv().await, None);
        });
    }

    #[test]
    fn capacity_is_a_real_bound() {
        let (tx, mut rx) = channel(2);
        block_on(async {
            tx.send(item(1)).await.expect("send 1");
            tx.send(item(2)).await.expect("send 2");
            // The buffer holds exactly `capacity`; this is the bounded-memory
            // guarantee, so assert on it rather than trusting the constant.
            assert_eq!(tx.len(), 2);
            assert_eq!(rx.recv().await, Some(item(1)));
            assert_eq!(tx.len(), 1);
        });
    }
}
```

- [ ] **Step 3: Run the test, verify it FAILS**

Run: `cargo test -p bterm-core chan::`
Expected: compile error, `cannot find function channel in this scope`.

- [ ] **Step 4: Implement**

Put above the test module in `crates/bterm-core/src/chan.rs`:

```rust
//! A bounded, single-producer single-consumer async channel.
//!
//! Hand-rolled because the workspace deliberately avoids the `futures`
//! crate — it is the only dependency that would pull in a scheduler we do
//! not need, and binary size is a standing constraint.
//!
//! The bound is the point: it is what makes "a pipeline never holds more
//! than N items in flight" true regardless of how fast a producer runs.

use crate::registry::PipelineData;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::future::poll_fn;
use std::rc::Rc;
use std::task::{Poll, Waker};

/// The receiver went away, so nothing will read what you are sending.
/// A producing command treats this as "stop", not as an error to report.
#[derive(Debug, PartialEq, Eq)]
pub struct Closed;

struct Inner {
    buffer: VecDeque<PipelineData>,
    capacity: usize,
    sender_gone: bool,
    receiver_gone: bool,
    /// Woken when the buffer gains an item or the sender goes away.
    recv_waker: Option<Waker>,
    /// Woken when the buffer frees a slot or the receiver goes away.
    send_waker: Option<Waker>,
}

pub fn channel(capacity: usize) -> (Sender, Receiver) {
    debug_assert!(capacity > 0, "a zero-capacity channel can never accept an item");
    let inner = Rc::new(RefCell::new(Inner {
        buffer: VecDeque::new(),
        capacity,
        sender_gone: false,
        receiver_gone: false,
        recv_waker: None,
        send_waker: None,
    }));
    (Sender { inner: inner.clone() }, Receiver { inner })
}

pub struct Sender {
    inner: Rc<RefCell<Inner>>,
}

impl Sender {
    /// Resolves once the item is buffered. Pending while the buffer is full —
    /// this is the backpressure. `Err(Closed)` means the receiver is gone.
    pub async fn send(&self, item: PipelineData) -> Result<(), Closed> {
        let mut slot = Some(item);
        poll_fn(|cx| {
            let mut inner = self.inner.borrow_mut();
            if inner.receiver_gone {
                return Poll::Ready(Err(Closed));
            }
            if inner.buffer.len() < inner.capacity {
                // `slot` is Some on the first poll; a second poll after
                // Ready cannot happen because the future is consumed.
                if let Some(item) = slot.take() {
                    inner.buffer.push_back(item);
                }
                let waker = inner.recv_waker.take();
                drop(inner);
                if let Some(w) = waker {
                    w.wake();
                }
                return Poll::Ready(Ok(()));
            }
            inner.send_waker = Some(cx.waker().clone());
            Poll::Pending
        })
        .await
    }

    /// Items currently buffered. For tests asserting the bound holds.
    pub fn len(&self) -> usize {
        self.inner.borrow().buffer.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// True once the receiver has gone away.
    pub fn is_closed(&self) -> bool {
        self.inner.borrow().receiver_gone
    }
}

impl Drop for Sender {
    fn drop(&mut self) {
        let mut inner = self.inner.borrow_mut();
        inner.sender_gone = true;
        let waker = inner.recv_waker.take();
        drop(inner);
        // Wake outside the borrow: a waker may re-enter this channel.
        if let Some(w) = waker {
            w.wake();
        }
    }
}

pub struct Receiver {
    inner: Rc<RefCell<Inner>>,
}

impl Receiver {
    /// `None` means end of stream: the sender is gone and the buffer is
    /// drained. Pending means more may yet arrive.
    pub async fn recv(&mut self) -> Option<PipelineData> {
        poll_fn(|cx| {
            let mut inner = self.inner.borrow_mut();
            if let Some(item) = inner.buffer.pop_front() {
                let waker = inner.send_waker.take();
                drop(inner);
                if let Some(w) = waker {
                    w.wake();
                }
                return Poll::Ready(Some(item));
            }
            if inner.sender_gone {
                return Poll::Ready(None);
            }
            inner.recv_waker = Some(cx.waker().clone());
            Poll::Pending
        })
        .await
    }
}

impl Drop for Receiver {
    fn drop(&mut self) {
        let mut inner = self.inner.borrow_mut();
        inner.receiver_gone = true;
        let waker = inner.send_waker.take();
        drop(inner);
        if let Some(w) = waker {
            w.wake();
        }
    }
}
```

- [ ] **Step 5: Run the tests, verify they PASS**

Run: `cargo test -p bterm-core chan::`
Expected: 2 passed.

Run: `cargo test --workspace`
Expected: 176 passed (174 existing + 2).

Run: `cargo clippy -p bterm-core --all-targets`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add crates/bterm-core/src/chan.rs crates/bterm-core/src/lib.rs
git commit -m "Add a bounded async channel for pipeline stages

Hand-rolled: the futures crate is the one dependency that would pull in a
scheduler we do not need, and binary size is a standing constraint. The
bound is the point -- it is what makes bounded memory true regardless of
how fast a producer runs."
```

---

## Task 2: Close semantics — the early-termination primitive

`head 5` terminating an infinite source works by the receiver going away and
the producer's next send failing. That is stage 3's payoff, but the mechanism
is built and tested here.

**Files:**
- Modify: `crates/bterm-core/src/chan.rs`

- [ ] **Step 1: Write the failing tests**

Add to the test module in `crates/bterm-core/src/chan.rs`:

```rust
    #[test]
    fn dropping_the_receiver_tells_the_producer_to_stop() {
        // This is the mechanism behind `head 5` terminating an infinite
        // source: the consumer goes away and the next send fails.
        let (tx, rx) = channel(4);
        block_on(async {
            tx.send(item(1)).await.expect("first send");
            drop(rx);
            assert!(tx.is_closed());
            assert_eq!(tx.send(item(2)).await, Err(Closed));
        });
    }

    #[test]
    fn a_send_into_a_full_buffer_does_not_complete() {
        // Backpressure, without needing a driver yet: poll the send future
        // once against a full buffer and confirm it parks rather than
        // over-filling. The end-to-end drain case lands in Task 3, once
        // there is something able to run both halves at once.
        use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};

        fn noop_waker() -> Waker {
            fn raw() -> RawWaker {
                fn clone(_: *const ()) -> RawWaker {
                    raw()
                }
                fn noop(_: *const ()) {}
                RawWaker::new(std::ptr::null(), &RawWakerVTable::new(clone, noop, noop, noop))
            }
            // SAFETY: every vtable entry is a no-op over a null pointer.
            unsafe { Waker::from_raw(raw()) }
        }

        let (tx, _rx) = channel(1);
        block_on(async {
            tx.send(item(1)).await.expect("first send fills the buffer");
        });
        assert_eq!(tx.len(), 1);

        let waker = noop_waker();
        let mut cx = Context::from_waker(&waker);
        let fut = tx.send(item(2));
        let mut fut = std::pin::pin!(fut);
        assert!(
            matches!(fut.as_mut().poll(&mut cx), Poll::Pending),
            "a full buffer must park the sender, not grow"
        );
        assert_eq!(tx.len(), 1, "the bound was exceeded");
    }

- [ ] **Step 2: Run, verify the first test fails**

Run: `cargo test -p bterm-core chan::tests::dropping_the_receiver`
Expected: FAIL — either a compile error on `Closed` comparison or a failed
assertion, depending on what is already implemented.

- [ ] **Step 3: Implement**

The `Sender::send` and `Drop` implementations from Task 1 already handle this
— `receiver_gone` is set on `Receiver::drop` and checked at the top of
`send`. If the first test passes without changes, that is correct; say so and
move on. If it fails, fix `chan.rs` so it passes without weakening the test.

- [ ] **Step 4: Verify**

Run: `cargo test -p bterm-core chan::`
Expected: the close test passes; the backpressure test fails to compile or is
ignored pending Task 3.

- [ ] **Step 5: Commit**

```bash
git add crates/bterm-core/src/chan.rs
git commit -m "Test channel close semantics

Dropping the receiver is the mechanism behind head terminating an
infinite source, so it gets a test before anything depends on it."
```

---

## Task 3: The stage driver

**Files:**
- Create: `crates/bterm-core/src/pipeline.rs`
- Modify: `crates/bterm-core/src/lib.rs`

- [ ] **Step 1: Register the module**

Add `pub mod pipeline;` to `crates/bterm-core/src/lib.rs`, alphabetically
(after `pub mod parse;`, before `pub mod protocol;`). Again, do this before
the failing test so the failure is a compile error rather than "0 tests".

- [ ] **Step 2: Write the failing test**

Create `crates/bterm-core/src/pipeline.rs` with only this test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::block_on;
    use std::cell::RefCell;
    use std::rc::Rc;

    /// A future that returns Pending `n` times before completing, recording
    /// the order in which it ran. Models a stage that awaits.
    fn yielding(
        label: &'static str,
        mut n: usize,
        log: Rc<RefCell<Vec<&'static str>>>,
    ) -> BoxedStage {
        Box::pin(std::future::poll_fn(move |cx| {
            log.borrow_mut().push(label);
            if n == 0 {
                return std::task::Poll::Ready(());
            }
            n -= 1;
            // Ask to be polled again: without this the driver would have no
            // reason to come back, and with `block_on`'s no-op waker the
            // wake is discarded — so this also proves the driver re-polls.
            cx.waker().wake_by_ref();
            std::task::Poll::Pending
        }))
    }

    #[test]
    fn every_stage_runs_to_completion() {
        let log = Rc::new(RefCell::new(Vec::new()));
        block_on(drive(vec![
            yielding("a", 2, log.clone()),
            yielding("b", 0, log.clone()),
            yielding("c", 1, log.clone()),
        ]));
        let seen = log.borrow();
        assert!(seen.iter().filter(|l| **l == "a").count() >= 3);
        assert!(seen.contains(&"b"));
        assert!(seen.contains(&"c"));
    }

    #[test]
    fn stages_interleave_rather_than_running_one_at_a_time() {
        // If the driver awaited stages sequentially, "a" would appear three
        // times before "b" ever ran. Interleaving is the whole point.
        let log = Rc::new(RefCell::new(Vec::new()));
        block_on(drive(vec![
            yielding("a", 2, log.clone()),
            yielding("b", 2, log.clone()),
        ]));
        let seen = log.borrow();
        let first_b = seen.iter().position(|l| *l == "b").expect("b ran");
        let last_a = seen.iter().rposition(|l| *l == "a").expect("a ran");
        assert!(first_b < last_a, "stages did not interleave: {seen:?}");
    }
}
```

- [ ] **Step 3: Run, verify it FAILS**

Run: `cargo test -p bterm-core pipeline::`
Expected: compile error, `cannot find function drive in this scope`.

- [ ] **Step 4: Implement**

Put above the test module in `crates/bterm-core/src/pipeline.rs`:

```rust
//! Drives a pipeline's stages concurrently inside a single future.
//!
//! There is deliberately no spawner. The wasm host has a real executor and
//! the native harness is a hand-rolled `block_on` that polls exactly one
//! future with a no-op waker; a spawner abstraction would mean the core's
//! own tests could not exercise concurrent stages at all. Owning the stage
//! futures here means one code path, and deterministic interleaving in
//! tests.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll, Wake, Waker};

/// One pipeline stage: runs to completion, communicating over channels.
///
/// The lifetime is load-bearing. A bare `Pin<Box<dyn Future>>` implies
/// `+ 'static`, and stage futures borrow the parsed call, the command
/// source, the context, and the scope — all owned by the caller. Without
/// `'a` this does not compile, and the error points at the call site rather
/// than here.
pub type BoxedStage<'a> = Pin<Box<dyn Future<Output = ()> + 'a>>;

/// A waker that records that it fired instead of scheduling anything.
///
/// This is what makes the driver correct under `block_on`'s no-op waker. A
/// stage that becomes ready *during* a polling pass — because an earlier
/// stage sent to its channel — would otherwise never be re-polled: the real
/// wake is discarded natively, and the driver has already passed it this
/// round. Recording the wake lets the driver loop again immediately.
/// `Arc` and `AtomicBool` are required by `std::task::Wake`, not chosen —
/// nothing here crosses a thread, since wasm is single-threaded. Do not try
/// to swap them for `Rc`/`Cell`; the trait will not accept it.
struct NudgeWaker {
    woken: AtomicBool,
    /// The executor's waker, so a genuinely external wake (a JS promise
    /// resolving in the browser) still reaches the real scheduler.
    outer: Waker,
}

impl Wake for NudgeWaker {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.woken.store(true, Ordering::Relaxed);
        self.outer.wake_by_ref();
    }
}

use std::sync::Arc;

/// Poll every stage until all complete.
///
/// Each pass polls all unfinished stages. If any stage woke during the pass
/// (including a wake caused by another stage in the same pass), it loops
/// again rather than returning Pending — otherwise progress would be lost
/// under a no-op waker.
pub async fn drive(stages: Vec<BoxedStage<'_>>) {
    let mut stages: Vec<Option<BoxedStage<'_>>> = stages.into_iter().map(Some).collect();

    std::future::poll_fn(move |cx| {
        loop {
            let nudge = Arc::new(NudgeWaker {
                woken: AtomicBool::new(false),
                outer: cx.waker().clone(),
            });
            let waker = Waker::from(nudge.clone());
            let mut inner = Context::from_waker(&waker);

            let mut remaining = 0;
            for slot in stages.iter_mut() {
                if let Some(stage) = slot {
                    match stage.as_mut().poll(&mut inner) {
                        Poll::Ready(()) => *slot = None,
                        Poll::Pending => remaining += 1,
                    }
                }
            }

            if remaining == 0 {
                return Poll::Ready(());
            }
            if !nudge.woken.load(Ordering::Relaxed) {
                // Nothing became ready during this pass; the real waker is
                // registered with whatever each stage is waiting on.
                return Poll::Pending;
            }
        }
    })
    .await;
}

```


- [ ] **Step 5: Run, verify PASS**

Run: `cargo test -p bterm-core pipeline::`
Expected: 2 passed.

- [ ] **Step 5b: Add the end-to-end backpressure test, now that a driver exists**

Task 2 proved a full buffer parks the sender. This proves it *un*-parks when
the consumer drains — which needs both halves running at once. Add to the
test module in `crates/bterm-core/src/chan.rs`:

```rust
    #[test]
    fn a_parked_sender_resumes_once_the_consumer_drains() {
        // Four items through a two-slot buffer: the producer must park
        // twice and be woken twice. This is the wakeup path that a no-op
        // waker would otherwise silently break.
        let (tx, mut rx) = channel(2);
        let sent = std::rc::Rc::new(std::cell::Cell::new(0));
        let seen = std::rc::Rc::new(std::cell::Cell::new(0));

        let producer = {
            let sent = sent.clone();
            async move {
                for n in 1..=4 {
                    tx.send(item(n)).await.expect("send");
                    sent.set(sent.get() + 1);
                }
            }
        };
        let consumer = {
            let seen = seen.clone();
            async move {
                while rx.recv().await.is_some() {
                    seen.set(seen.get() + 1);
                }
            }
        };

        block_on(crate::pipeline::drive(vec![
            Box::pin(producer),
            Box::pin(consumer),
        ]));

        assert_eq!(sent.get(), 4, "producer did not finish");
        assert_eq!(seen.get(), 4, "consumer did not see everything");
    }
```

Run: `cargo test -p bterm-core chan::`
Expected: 4 passed.

Run: `cargo test --workspace`
Expected: 180 passed (174 + 3 chan + 2 pipeline + 1 backpressure drain).

Run: `cargo clippy -p bterm-core --all-targets`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add crates/bterm-core/src/pipeline.rs crates/bterm-core/src/chan.rs crates/bterm-core/src/lib.rs
git commit -m "Add the concurrent stage driver

No spawner: the wasm host has an executor and the native harness polls one
future with a no-op waker, so a spawner would put concurrent stages beyond
the reach of the core's own tests. Owning the futures here gives one code
path and deterministic interleaving.

The nudge waker is what makes it correct under a no-op waker -- a stage
made ready by an earlier stage in the same pass would otherwise never be
re-polled."
```

---

## Task 4: Rewire `eval_pipeline` — the behaviour-preserving swap

This is the gate. **No existing test may change.**

**Files:**
- Modify: `crates/bterm-core/src/eval.rs:72-83`

- [ ] **Step 1: Read the current implementation**

`eval_pipeline` is currently:

```rust
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
```

`eval_call` keeps today's signature and is **not** modified.

- [ ] **Step 2: Implement the swap**

Replace `eval_pipeline` with:

```rust
/// Evaluate a pipeline by running every stage as a concurrent future joined
/// by bounded channels.
///
/// Every command still collects its whole input, so a stage cannot start
/// before its predecessor finishes and the output is identical to the
/// sequential version this replaced. What changes is the transport: the
/// machinery for streaming stages is in place and exercised, so stage 3 adds
/// streaming commands rather than rebuilding how stages talk.
pub async fn eval_pipeline(
    pipeline: &Pipeline,
    source: &impl CommandSource,
    ctx: &ExecContext,
    scope: &Scope,
) -> Result<PipelineData, ShellError> {
    // A single call has no channel to build; keep the direct path so the
    // common case pays nothing for machinery it does not use.
    if pipeline.calls.len() == 1 {
        return eval_call(&pipeline.calls[0], PipelineData::Empty, source, ctx, scope).await;
    }

    let outcome: Rc<RefCell<Option<Result<PipelineData, ShellError>>>> =
        Rc::new(RefCell::new(None));
    let failure: Rc<RefCell<Option<ShellError>>> = Rc::new(RefCell::new(None));

    let mut stages: Vec<crate::pipeline::BoxedStage> = Vec::new();
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
    outcome
        .borrow_mut()
        .take()
        .unwrap_or(Ok(PipelineData::Empty))
}

/// Items a stage may buffer before its producer is made to wait. The bound
/// is what makes memory usage independent of how fast a producer runs.
const STAGE_BUFFER: usize = 64;

/// One stage: drain the upstream channel, run the command, hand the result
/// downstream (or to `outcome`, for the last stage).
///
/// The first error wins and stops the pipeline; later stages find their
/// channel closed and return.
#[allow(clippy::too_many_arguments)]
async fn run_stage(
    call: &Call,
    upstream: Option<crate::chan::Receiver>,
    downstream: Option<crate::chan::Sender>,
    source: &impl CommandSource,
    ctx: &ExecContext,
    scope: &Scope,
    outcome: Option<Rc<RefCell<Option<Result<PipelineData, ShellError>>>>>,
    failure: Rc<RefCell<Option<ShellError>>>,
) {
    // Collect the whole upstream. Every command in this stage still wants
    // its complete input; streaming commands arrive in stage 3.
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
            (None, Some(slot)) => *slot.borrow_mut() = Some(Ok(data)),
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
```

Add to the imports at the top of `crates/bterm-core/src/eval.rs`:

```rust
use std::cell::RefCell;
```

(`std::rc::Rc` is already imported.)

- [ ] **Step 3: Run the FULL suite — this is the gate**

Run: `cargo test --workspace`
Expected: **179 passed**, with **zero changes to any existing test**. If any
previously-passing test now fails, the transport is wrong — fix the
transport, never the test.

Run: `cargo clippy --workspace --all-targets`
Expected: clean.

- [ ] **Step 4: Verify the CLI behaves identically**

Run:
```bash
printf "echo a b c | str upcase\necho '[{\"n\":3},{\"n\":1}]' | from json | sort-by n | to json\nmux\nstr upcsae hi\n" | cargo run -q -p bterm-cli
```
Expected, unchanged from before this task: the upcased list, the sorted JSON,
the `mux` group page, and the `str upcsae` did-you-mean error. Compare against
`git stash` + re-run if unsure.

- [ ] **Step 5: Commit**

```bash
git add crates/bterm-core/src/eval.rs
git commit -m "Run pipeline stages as concurrent futures over channels

Behaviour-preserving by construction: every command still collects its
whole input, so a stage cannot start before its predecessor finishes and
the output is identical. The existing suite passing untouched is the gate.

What this buys is the transport -- stage 3 adds streaming commands rather
than rebuilding how stages talk to each other."
```

---

## Task 5: Prove the transport does what it claims

Task 4's gate proves nothing *changed*. These prove the new machinery is
actually load-bearing rather than decorative.

**Files:**
- Modify: `crates/bterm-core/src/eval.rs` (test module)

- [ ] **Step 1: Write the failing tests**

Add to the `#[cfg(test)] mod tests` block in `crates/bterm-core/src/eval.rs`:

```rust
    #[test]
    fn a_failing_stage_stops_the_pipeline_and_keeps_its_own_error() {
        // The first error wins: a later stage must not overwrite it with a
        // downstream symptom of the same failure.
        let err = eval("emit 1 | boom | double").expect_err("boom should fail");
        assert!(err.msg.contains("boom"), "wrong error survived: {}", err.msg);
    }

    #[test]
    fn a_three_stage_pipeline_still_threads_values_end_to_end() {
        // Two channels, three stages: proves the wiring is not accidentally
        // correct only for the single-channel case.
        let out = eval("emit 5 | double | double").expect("eval");
        assert_eq!(
            out.into_iter().last().map(PipelineData::into_value),
            Some(Value::Int(20))
        );
    }
```

You will need a failing fake command. Add it next to the existing `Emit` and
`Double` fixtures in that module:

```rust
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
            _input: PipelineData,
        ) -> LocalBoxFuture<Result<PipelineData, ShellError>> {
            ready(Err(ShellError::runtime("boom")))
        }
    }
```

and register it in that module's `registry()` helper alongside `Emit` and
`Double`:

```rust
        r.register_builtin(Rc::new(Boom));
```

Check the existing `eval` helper in that module — if it returns
`Result<Vec<PipelineData>, ShellError>` the second test's `.into_iter().last()`
is right; if it returns a single value, assert on that directly instead. Match
what is there rather than changing it.

- [ ] **Step 2: Run, verify they FAIL**

Run: `cargo test -p bterm-core eval::tests::a_failing_stage eval::tests::a_three_stage`
Expected: compile error, `cannot find type Boom`.

- [ ] **Step 3: Add the fixture and run again**

Run: `cargo test -p bterm-core eval::`
Expected: all pass.

- [ ] **Step 4: Full suite**

Run: `cargo test --workspace`
Expected: 181 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/bterm-core/src/eval.rs
git commit -m "Test error propagation and multi-stage wiring

The refactor's gate proves nothing changed; these prove the new transport
is load-bearing -- three stages means two channels, and the first error
must survive rather than being replaced by a downstream symptom."
```

---

## Task 6: Prove the borrow invariant survives concurrency

The spec's central safety claim is that concurrent stages cannot violate the
`with_engine` discipline, because JS is cooperatively scheduled and no borrow
spans a yield point. That claim is currently an argument, not a test.

**Files:**
- Modify: `crates/bterm-core/src/engine.rs` (test module)

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)] mod tests` block in `crates/bterm-core/src/engine.rs`:

```rust
    /// Takes an engine borrow on every poll, and yields once, so that if the
    /// driver ever polled two stages with a borrow live the second would
    /// panic with "already borrowed".
    struct Borrower(Rc<RefCell<Engine>>);
    impl Command for Borrower {
        fn signature(&self) -> &Signature {
            static SIG: std::sync::OnceLock<Signature> = std::sync::OnceLock::new();
            SIG.get_or_init(|| Signature::build("borrower", "borrows the engine"))
        }
        fn run(
            &self,
            _ctx: ExecContext,
            _call: BoundCall,
            _input: PipelineData,
        ) -> LocalBoxFuture<Result<PipelineData, ShellError>> {
            let access = self.0.clone();
            Box::pin(async move {
                let mut yielded = false;
                std::future::poll_fn(|cx| {
                    // A short borrow, released before the yield — the
                    // discipline the whole design rests on.
                    let panes = access.with(|e| e.mux.sessions.len());
                    assert!(panes > 0);
                    if yielded {
                        std::task::Poll::Ready(())
                    } else {
                        yielded = true;
                        cx.waker().wake_by_ref();
                        std::task::Poll::Pending
                    }
                })
                .await;
                Ok(PipelineData::Value(Value::Int(1)))
            })
        }
    }

    #[test]
    fn concurrent_stages_never_hold_overlapping_engine_borrows() {
        let access = engine();
        access.with(|e| {
            e.registry.register_builtin(Rc::new(Borrower(access.clone())));
        });
        // Three stages, each borrowing on every poll and yielding between.
        // A RefCell double-borrow would panic rather than fail an assert.
        let events = feed_and_run(&access, "borrower | borrower | borrower\r");
        let out = output_text(&events);
        assert!(!out.contains("panicked"), "output: {out:?}");
    }
```

- [ ] **Step 2: Run, verify it FAILS**

Run: `cargo test -p bterm-core engine::tests::concurrent_stages`
Expected: compile error, `cannot find type Borrower`.

- [ ] **Step 3: Add the fixture**

The code above is the whole fixture. Add whatever imports the compiler asks
for, matching the module's existing import style.

- [ ] **Step 4: Run, verify PASS**

Run: `cargo test -p bterm-core engine::tests::concurrent_stages`
Expected: PASS — and specifically, no `already borrowed: BorrowMutError` panic.

Run: `cargo test --workspace`
Expected: 182 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/bterm-core/src/engine.rs
git commit -m "Test that concurrent stages cannot overlap engine borrows

The design rests on the claim that cooperative scheduling plus
never-borrow-across-await makes RefCell panics unreachable. That was an
argument; this makes it a test that fails loudly if the driver ever polls
a stage while another holds a borrow."
```

---

## Task 7: Verify in the browser and ship

Native tests cannot exercise the real executor, real wakers, or the JS
boundary. A green suite is not evidence the browser still works — that
mistake has been made twice on this project.

**Files:** none modified unless a defect is found.

- [ ] **Step 1: Build everything**

```bash
just build
npm --prefix packages/demo run build
npm --prefix packages/demo-react run build
npm --prefix packages/demo-svelte run build
```
Expected: all succeed.

- [ ] **Step 2: Run the browser suite**

```bash
cd packages/demo && npx playwright test
```
Expected: 11 passed, unchanged.

- [ ] **Step 3: Check the wasm size delta**

```bash
ls -l packages/browser-terminal/dist/wasm/bterm_wasm_bg.wasm | awk '{print $5}'
```
The pre-stage-2 size is 438188 bytes. Report the new number and the delta.
The README (`Current wasm size:`) and the spec's size note both quote this —
if it moved by more than 2KB, update both rather than letting them drift.

- [ ] **Step 4: Exercise a real pipeline in a real browser**

Start the demo dev server on port 5199, then run this and DELETE it after:

```js
import { chromium } from 'playwright';
const b = await chromium.launch();
const p = await b.newPage();
const errs = [];
p.on('pageerror', (e) => errs.push(String(e)));
await p.goto('http://localhost:5199/', { waitUntil: 'networkidle' });
await p.waitForFunction(() => !!window.bt, null, { timeout: 20000 });
const out = await p.evaluate(async () => {
  const four = await window.bt.run("links | filter {|o| $o.text != ''} | head 2 | length");
  const slow = await window.bt.run('slow 2');
  const failed = await window.bt.run('links | grep "(" | length').then(
    () => null,
    (e) => e.message,
  );
  return { four: four.value, slowLog: slow.log, slowValue: slow.value, failed };
});
console.log(JSON.stringify(out), JSON.stringify(errs));
await b.close();
```

Expected:
- `four` is `2` — a four-stage pipeline still threads values
- `slowLog` is `["tick 1/2","tick 2/2"]` — progressive output still flushes per write
- `slowValue` is `"done after 2s"`
- `failed` contains `invalid regex pattern` — an error mid-pipeline still propagates
- `errs` is `[]`

Report the exact output. If `slowLog` is empty or arrives all at once, the
driver has broken the per-write flush — investigate before proceeding.

- [ ] **Step 5: Commit any doc updates, then report**

```bash
git add -A
git commit -m "Stage 2: record wasm size after the transport change"
```
(Skip if nothing changed.)

---

## Self-review notes

**Spec coverage.** This plan implements the spec's stage 2 in full: the
bounded channel (Task 1), close semantics as the early-termination primitive
(Task 2), the spawner-free stage driver with its waker discipline (Task 3),
the behaviour-preserving rewire (Task 4), and the safety-invariant proof
(Task 6) the spec explicitly asked be stated rather than assumed.

**Deliberately deferred, matching the spec's staging:**
- Streaming commands, `Command` trait changes, and the `PipelineData` collapse
  into a stream-plus-tag — stage 3. This plan leaves `Command::run` untouched,
  which is what keeps Task 4's gate meaningful.
- `Sink::ready()` and pane self-throttling — stage 6.
- Streaming table rendering (probe window) — stage 5.

**Known thin spot.** With every command collecting, real pipelines put at most
one item in a channel, so the bounded buffer is never exercised end-to-end.
That is why Tasks 1–2 test the channel directly, including a backpressure case
that drives producer and consumer together. Anyone tempted to delete those as
"covered by integration tests" should read this paragraph first.

**A judgement call in Task 4.** Single-call pipelines keep the direct path
rather than building a one-stage driver. It costs an `if`, and it means the
overwhelmingly common case (`echo 42`, `links`, every `--help`) pays nothing
for machinery it cannot use. The multi-stage path is still exercised by every
piped test in the suite.
