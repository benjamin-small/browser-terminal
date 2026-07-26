# Progressive Rendering (Stage 5a) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Paint a streaming data result to the pane as rows arrive, instead of only after the pipeline finishes — so a live source shows output progressively.

**Architecture:** A pure `StreamRenderer` (probe → commit column widths → stream rows at fixed widths) does the formatting. `eval_pipeline` gains a `FinalConsumer` seam: `run()`/CLI/tests use a *collecting* consumer (byte-identical to today — the gate), while the interactive pane uses a *progressive* consumer that paints via the engine and applies pane backpressure through a new `Sink::ready()`. The probe's row-count and end-of-stream commits live in the core (native-testable); the *time* bound and the paint throttle are host-driven (wasm `setTimeout` → an engine tick), because `bterm-core` has no clock.

**Tech Stack:** Rust (`bterm-core` — no wasm deps, no `futures` crate), `bterm-wasm`, Playwright.

**Spec:** [docs/superpowers/specs/2026-07-25-progressive-output-and-buffering-design.md](../specs/2026-07-25-progressive-output-and-buffering-design.md). This plan covers the **progressive-rendering + throttle** subsystem only. The **output-buffering API** (allowlist sanitizer, `ctx.log`/`ctx.err` writers, progress bars) is a separate follow-up plan (stage 5b).

---

## Context an engineer needs

**How output reaches the pane today.** `execute_line` (`crates/bterm-core/src/engine.rs`) runs `eval_line` → `eval_pipeline` (`crates/bterm-core/src/eval.rs`), which builds one future per stage plus a **terminal collector** future that drains the last channel into `outcome: Rc<RefCell<PipelineData>>`. After `drive()` returns, `execute_line` calls `render(value, cols)` once and emits the result via `access.with(|e| e.emit_output(pane, text))`. So the whole result is collected, then painted once.

**Why progressive rendering needs a seam.** `eval_pipeline` is engine-agnostic — it has a `CommandSource`, not an `EngineAccess`, so its terminal collector *cannot* paint (no `access`). Only `execute_line` (in `engine.rs`) has `access`. So progressive painting is introduced by letting `eval_pipeline` hand the last stage's items to a caller-supplied **`FinalConsumer`**, of which there are two: a collecting one (current behaviour, used by `run()`/CLI/tests) and a progressive one (used by the interactive pane, painting via `access`).

**The clock.** `bterm-core` has no time source; `block_on` has no timer. So:
- **Row-count commit and end-of-stream commit** live in the core and are native-testable.
- **The time bound** ("commit T ms after the first row") and **the throttle interval** are host nudges: the wasm layer arms a `setTimeout` that calls a new engine entry point to flush the pending render. Natively there is no timer, so a native pipeline commits on row-count or end-of-stream — which is exactly right for the CLI/tests (batch), and keeps the finite-pipeline gate byte-identical.

**The gate.** Every finite pipeline whose whole result arrives before the probe commits must render byte-identically to today. That is what the collecting consumer preserves for `run()`/CLI/tests, and what a native test on `execute_line` must confirm for the pane path.

**Invariant unchanged.** No borrow crosses `.await`; `with` closures never call JS. The new `Sink::ready()` is awaited with no engine borrow held.

**Existing render helpers** (`crates/bterm-core/src/render/mod.rs`): `render(value, width)` dispatches to `render_table(items, width)` for a `List` that `is_table()`. `render_table` computes column widths from all rows. This plan factors the width computation and row formatting so they can run incrementally; do not change `render()`'s existing output for a whole value.

---

## File Structure

| File | Responsibility |
|---|---|
| `crates/bterm-core/src/render/stream.rs` | **Create.** `StreamRenderer`: pure probe/commit/stream/finish producing text chunks. No engine, no clock. |
| `crates/bterm-core/src/render/mod.rs` | **Modify.** Expose the column-width + row-format helpers `StreamRenderer` needs; register `pub mod stream` (or inline). |
| `crates/bterm-core/src/sink.rs` | **Modify.** `Sink` gains `ready()`. |
| `crates/bterm-core/src/eval.rs` | **Modify.** `FinalConsumer` trait; `eval_pipeline` feeds the last stage to it; a `CollectingConsumer` preserving today's behaviour. |
| `crates/bterm-core/src/engine.rs` | **Modify.** A `ProgressiveConsumer` painting via `access`; `execute_line` uses it; a `commit_pending` engine entry point for the host timer; `PaneSink` throttle. |
| `crates/bterm-wasm/src/lib.rs` + `tasks.rs` | **Modify.** `setTimeout`-driven probe deadline + throttle tick calling `commit_pending`. |
| `packages/demo` | **Modify.** Live-render browser test. |

---

## Task 1: `StreamRenderer` — pure incremental table rendering

**Files:**
- Create: `crates/bterm-core/src/render/stream.rs`
- Modify: `crates/bterm-core/src/render/mod.rs`

- [ ] **Step 1: Expose the helpers and register the module**

In `crates/bterm-core/src/render/mod.rs`, add near the top (after the existing `pub fn render`):

```rust
pub mod stream;
```

`StreamRenderer` needs three things `render_table` already does internally: the ordered column set from a batch of records, per-column display widths, and formatting one row at fixed widths. Rather than duplicate, extract small `pub(crate)` helpers from the existing `render_table`. Read `render_table` and pull out:

```rust
/// The ordered union of record keys across `rows`, in first-seen order.
pub(crate) fn table_columns(rows: &[Value]) -> Vec<String> { /* existing logic */ }

/// Display width for each column given these sample rows (header vs cells,
/// unicode-width, capped). Mirrors what render_table computes.
pub(crate) fn column_widths(cols: &[String], rows: &[Value]) -> Vec<usize> { /* existing */ }

/// One row rendered at fixed widths, cells truncated with `…`. Includes the
/// box borders. Reuse render_table's cell formatting.
pub(crate) fn table_row(row: &Value, cols: &[String], widths: &[usize]) -> String { /* existing */ }

/// Top border + bold header + separator, at fixed widths.
pub(crate) fn table_header(cols: &[String], widths: &[usize]) -> String { /* existing */ }

/// Bottom border at fixed widths.
pub(crate) fn table_bottom(widths: &[usize]) -> String { /* existing */ }
```

Refactor `render_table` to call these so there is ONE width/formatting implementation. Its output must be unchanged — verified by the existing render snapshot tests.

- [ ] **Step 2: Write the failing test**

Create `crates/bterm-core/src/render/stream.rs` with only the test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::Value;
    use indexmap::IndexMap;

    fn rec(pairs: &[(&str, i64)]) -> Value {
        let mut m = IndexMap::new();
        for (k, v) in pairs {
            m.insert((*k).to_string(), Value::Int(*v));
        }
        Value::Record(m)
    }

    #[test]
    fn commits_after_probe_rows_and_streams_the_rest() {
        // Probe = 2 rows. First two rows buffer (no output yet); the 2nd
        // triggers commit -> header + those rows. Later rows stream one at a
        // time. finish() closes the box.
        let mut r = StreamRenderer::new(80, 2);
        assert_eq!(r.push(rec(&[("id", 1)])), None, "still probing");
        let committed = r.push(rec(&[("id", 2)])).expect("commit on 2nd row");
        assert!(committed.contains("id"), "header painted: {committed:?}");
        assert!(committed.contains('1') && committed.contains('2'), "probe rows: {committed:?}");

        let third = r.push(rec(&[("id", 3)])).expect("streamed row");
        assert!(third.contains('3'));
        assert!(!third.contains("id"), "header not repainted: {third:?}");

        let end = r.finish();
        assert!(end.contains('└') || end.contains("┘"), "bottom border: {end:?}");
    }

    #[test]
    fn commit_forces_paint_with_fewer_than_probe_rows() {
        // A slow source: only one row arrived before the deadline. commit()
        // paints header + that row using widths from just it.
        let mut r = StreamRenderer::new(80, 50);
        assert_eq!(r.push(rec(&[("id", 7)])), None);
        let out = r.commit().expect("forced commit");
        assert!(out.contains('7'), "the single probed row painted: {out:?}");
        // After commit, later rows stream.
        assert!(r.push(rec(&[("id", 8)])).expect("streamed").contains('8'));
    }

    #[test]
    fn empty_stream_finishes_without_painting_a_table() {
        // No rows ever arrived: finish() paints nothing (an empty table is
        // meaningless), not a header with zero columns.
        let mut r = StreamRenderer::new(80, 50);
        assert_eq!(r.commit(), None, "nothing to commit");
        assert_eq!(r.finish(), String::new());
    }
}
```

- [ ] **Step 3: Run, verify it fails**

Run: `cargo test -p bterm-core render::stream`
Expected: compile error, `cannot find type StreamRenderer`.

- [ ] **Step 4: Implement**

Above the test module in `crates/bterm-core/src/render/stream.rs`:

```rust
//! Incremental table rendering for a streaming data result.
//!
//! A box table needs column widths, which need rows — but a live stream has
//! no end. So we *probe*: buffer up to `probe_rows` records, commit widths
//! from them, paint the header + probed rows, then stream each later row at
//! those fixed widths (truncating an over-wide cell with `…`). The row-count
//! and end-of-stream commits live here; the *time* bound is external — the
//! host calls `commit()` when its deadline fires, because this crate has no
//! clock.

use crate::value::Value;

pub struct StreamRenderer {
    width: u16,
    probe_rows: usize,
    probe: Vec<Value>,
    committed: Option<Committed>,
}

struct Committed {
    cols: Vec<String>,
    widths: Vec<usize>,
}

impl StreamRenderer {
    pub fn new(width: u16, probe_rows: usize) -> Self {
        StreamRenderer { width, probe_rows, probe: Vec::new(), committed: None }
    }

    /// Feed one record. Returns text to paint, if any: `None` while still
    /// probing; the header + probed rows on the commit transition; one row
    /// once committed.
    pub fn push(&mut self, row: Value) -> Option<String> {
        if self.committed.is_some() {
            return Some(self.render_streamed_row(&row));
        }
        self.probe.push(row);
        if self.probe.len() >= self.probe_rows {
            self.commit()
        } else {
            None
        }
    }

    /// Force a commit now (host deadline, or end-of-stream). Paints the
    /// header + whatever rows have been probed. `None` if nothing to paint
    /// (no rows yet) or already committed.
    pub fn commit(&mut self) -> Option<String> {
        if self.committed.is_some() || self.probe.is_empty() {
            return None;
        }
        let rows = std::mem::take(&mut self.probe);
        let cols = super::table_columns(&rows);
        let widths = super::column_widths(&cols, &rows);
        let mut out = super::table_header(&cols, &widths);
        for row in &rows {
            out.push_str(&super::table_row(row, &cols, &widths));
        }
        self.committed = Some(Committed { cols, widths });
        Some(out)
    }

    /// Close the table. Commits any un-committed probe first, then the
    /// bottom border. Empty string if nothing was ever painted.
    pub fn finish(&mut self) -> String {
        let mut out = self.commit().unwrap_or_default();
        if let Some(c) = &self.committed {
            out.push_str(&super::table_bottom(&c.widths));
        }
        out
    }

    fn render_streamed_row(&self, row: &Value) -> String {
        let c = self.committed.as_ref().expect("committed");
        super::table_row(row, &c.cols, &c.widths)
    }
}
```

Adjust the helper names/signatures to whatever `render_table` actually uses once you extract them; the `width` field is passed through to width computation if the existing code caps by terminal width (thread it into `column_widths` if so).

- [ ] **Step 5: Run, verify pass**

Run: `cargo test -p bterm-core render::`
Expected: the 3 new tests pass AND the existing render tests still pass (the `render_table` refactor changed nothing observable).

Run: `cargo test --workspace` → 197 (unchanged) + 3 = 200.
Run: `cargo clippy -p bterm-core --all-targets` → clean.

- [ ] **Step 6: Commit**

```bash
git add crates/bterm-core/src/render/
git commit -m "Add StreamRenderer: incremental table rendering via a probe

Buffers up to probe_rows records, commits column widths from them, then
streams later rows at fixed widths. Pure and clockless: row-count and
end-of-stream commits live here; the time bound is a host concern. The
existing render_table is refactored onto shared width/format helpers so
there is one implementation and its output is unchanged."
```

---

## Task 2: `Sink::ready()` for pane backpressure

**Files:**
- Modify: `crates/bterm-core/src/sink.rs`, and every `Sink` impl.

- [ ] **Step 1: Write the failing test**

In `crates/bterm-core/src/sink.rs` test module:

```rust
    #[test]
    fn collecting_and_null_sinks_are_immediately_ready() {
        use crate::eval::block_on;
        let sink = CollectingSink::new();
        block_on(sink.ready()); // returns at once, no hang
        block_on(NullSink.ready());
    }
```

- [ ] **Step 2: Run, verify it fails**

Run: `cargo test -p bterm-core sink::tests::collecting_and_null`
Expected: `no method named ready`.

- [ ] **Step 3: Implement**

Add to the `Sink` trait in `crates/bterm-core/src/sink.rs`:

```rust
    /// Resolves when the sink can accept more output. A pane sink uses this
    /// to throttle a fast producer to display speed; capturing and test
    /// sinks return immediately. Awaited by the progressive consumer with no
    /// engine borrow held.
    fn ready(&self) -> crate::registry::LocalBoxFuture<()> {
        crate::registry::ready(())
    }
```

`crate::registry::ready` already exists (wraps a value in a ready future). The default impl makes every existing sink (`CollectingSink`, `NullSink`, `CliSink`, the test doubles) immediately ready with no change. `PaneSink` overrides it in Task 4.

- [ ] **Step 4: Run, verify pass**

Run: `cargo test -p bterm-core sink::` → passes.
Run: `cargo test --workspace` → 201.
Run: `cargo clippy --workspace --all-targets` → clean (the default method means no impl needs updating).

- [ ] **Step 5: Commit**

```bash
git add crates/bterm-core/src/sink.rs
git commit -m "Sink gains ready() for pane backpressure

Default returns immediately, so collecting/test/CLI sinks are unaffected.
The pane sink will override it to throttle a fast producer to display
speed; the progressive consumer awaits it between paints."
```

---

## Task 3: The `FinalConsumer` seam in `eval_pipeline`

**Files:**
- Modify: `crates/bterm-core/src/eval.rs`

This restructures the terminal collector into a pluggable consumer while
keeping `run()`/CLI/tests byte-identical.

- [ ] **Step 1: Write the failing test**

Add to the `eval.rs` test module (it already has `Emit`/`Double` fixtures, `registry()`, and an `eval()` helper that returns `Result<Vec<PipelineData>, ShellError>`):

```rust
    #[test]
    fn collecting_consumer_preserves_the_final_value() {
        // The default path used by run()/CLI/tests: the pipeline's value is
        // unchanged by the consumer seam.
        let out = eval("emit 5 | double").expect("eval");
        assert_eq!(
            out.into_iter().last().map(PipelineData::into_value),
            Some(Value::Int(10))
        );
    }
```

(This should PASS once the refactor keeps behaviour — it's a characterization test guarding the gate. Write it now; it fails to compile only if you change `eval`'s shape, which you must not.)

- [ ] **Step 2: Implement the seam**

Add to `crates/bterm-core/src/eval.rs`:

```rust
/// Receives the last stage's items. The pane path paints progressively; the
/// programmatic/CLI/test path collects into one value. Kept out of
/// `eval_pipeline` itself so the evaluator stays engine-agnostic.
///
/// `item` is synchronous — painting is `access.with(...)`, which is sync, and
/// a future borrowing `&mut self` cannot be boxed into the `'static`
/// `LocalBoxFuture`. Backpressure is a separate `ready()` that returns a
/// `'static` future by cloning the `Rc<dyn Sink>` (it borrows nothing of
/// `self`), so the collector can `await` it with no borrow held.
pub trait FinalConsumer {
    /// One item from the last stage. Synchronous: paint or buffer it now.
    fn item(&mut self, item: PipelineData);
    /// Resolves when the consumer can accept more (pane throttle). Default
    /// immediate; the progressive consumer returns its sink's `ready()`.
    fn ready(&self) -> crate::registry::LocalBoxFuture<()> {
        crate::registry::ready(())
    }
    /// End of stream; return the value `eval_line` should report.
    fn finish(&mut self) -> PipelineData;
}

/// Collects items exactly as the previous terminal collector did — the
/// behaviour `run()`, the CLI, and every test depend on. An all-`Rendered`
/// stream joins into one `Rendered`; otherwise 0 -> Empty, 1 -> that item,
/// N -> List.
pub struct CollectingConsumer {
    items: Vec<PipelineData>,
}

impl CollectingConsumer {
    pub fn new() -> Self {
        CollectingConsumer { items: Vec::new() }
    }
}

impl Default for CollectingConsumer {
    fn default() -> Self {
        Self::new()
    }
}

impl FinalConsumer for CollectingConsumer {
    fn item(&mut self, item: PipelineData) {
        self.items.push(item);
    }
    // ready() uses the default (immediate) — collecting has no backpressure.
    fn finish(&mut self) -> PipelineData {
        // Move the exact collecting logic here from the old terminal
        // collector in eval_pipeline (all-Rendered join; 0/1/N cases).
        let items = std::mem::take(&mut self.items);
        // ... same match the old collector used ...
    }
}
```

Change `eval_pipeline` to take `consumer: &mut dyn FinalConsumer`. In the terminal-collector future, per received item call `consumer.item(x)` (sync) then `consumer.ready().await` (the throttle):

```rust
    while let Some(item) = upstream.recv().await {
        consumer.item(item);
        consumer.ready().await;
    }
```

After the loop, do nothing — the caller calls `finish()`. `eval_pipeline` no longer returns the collected value; the caller reads it from `consumer.finish()`. Update `eval_line` to own a `CollectingConsumer`, pass `&mut` it to each `eval_pipeline` call, and push `consumer.finish()` into `results`.

**Borrow care:** `consumer.ready()` returns a `'static` future (it clones the `Rc<dyn Sink>`, borrows nothing of `consumer`), so the immutable borrow from the method call ends before the `.await` — no borrow across await. `consumer.item(x)` is sync. The progressive consumer (Task 4) paints inside `item` via `access.with(...)` (sync) then `access.events_ready()` (after the borrow drops) — same discipline as everywhere.

- [ ] **Step 3: Run — the gate**

Run: `cargo test --workspace`
Expected: **202 passed, no existing test changed** (201 + the new characterization test). `run()`, CLI, and every pipeline test must be byte-identical — the collecting consumer reproduces the old terminal collector exactly. If any fails, the `finish()` logic diverged from the old collector; fix it.

Run: `cargo clippy --workspace --all-targets` → clean.

- [ ] **Step 4: Commit**

```bash
git add crates/bterm-core/src/eval.rs
git commit -m "Add a FinalConsumer seam to eval_pipeline

The terminal collector becomes pluggable: run()/CLI/tests use a
CollectingConsumer that reproduces the old collect-once behaviour exactly
(the gate), while the interactive pane will supply a progressive consumer.
Keeps eval_pipeline engine-agnostic -- painting needs an engine, which
only the pane path has."
```

---

## Task 4: `ProgressiveConsumer` — paint the pane as rows arrive

**Files:**
- Modify: `crates/bterm-core/src/engine.rs`

- [ ] **Step 1: Write the failing test**

The engine test module builds `Rc<RefCell<Engine>>` via `engine()`, feeds input via `feed_and_run`, and reads `PaneOutput` via `output_text`. Add a streaming fixture and a progressive-paint test:

```rust
    /// Emits N records, then ends. Lets us assert the pane painted a header
    /// before the last row arrived.
    struct Rows(usize);
    impl Command for Rows {
        fn signature(&self) -> &Signature {
            static SIG: std::sync::OnceLock<Signature> = std::sync::OnceLock::new();
            SIG.get_or_init(|| Signature::build("rows", "emit n records"))
        }
        fn run(
            &self,
            _ctx: ExecContext,
            _call: BoundCall,
            _input: crate::chan::Receiver,
            output: crate::chan::Sender,
        ) -> crate::registry::LocalBoxFuture<Result<(), ShellError>> {
            let n = self.0;
            Box::pin(async move {
                for i in 0..n {
                    let mut m = indexmap::IndexMap::new();
                    m.insert("id".to_string(), Value::Int(i as i64));
                    if output.send(PipelineData::Value(Value::Record(m))).await.is_err() {
                        return Ok(());
                    }
                }
                Ok(())
            })
        }
    }

    #[test]
    fn a_finite_table_still_renders_correctly() {
        // The gate for the pane path: a small finite result paints the whole
        // table (header + all rows + bottom border), same as before.
        let access = engine();
        access.with(|e| e.registry.register_builtin(Rc::new(Rows(3))));
        let out = output_text(&feed_and_run(&access, "rows\r"));
        assert!(out.contains("id"), "header: {out:?}");
        assert!(out.contains('0') && out.contains('2'), "all rows: {out:?}");
    }
```

- [ ] **Step 2: Run, verify it fails**

Run: `cargo test -p bterm-core engine::tests::a_finite_table`
Expected: compile error (`Rows` references nothing new yet) — or, once it compiles, it should already pass under the collecting path. The point of THIS task is to route `execute_line` through a progressive consumer; write the test, then make `execute_line` use `ProgressiveConsumer` and confirm the test still passes (the finite case is byte-identical).

- [ ] **Step 3: Implement the progressive consumer**

Add to `crates/bterm-core/src/engine.rs`:

```rust
/// Paints the last stage's items to a pane as they arrive. Records feed a
/// `StreamRenderer` (probe -> commit -> stream); non-record items print via
/// the normal `render`. Between paints it awaits the sink's `ready()` so a
/// fast producer throttles to display speed.
struct ProgressiveConsumer<A: EngineAccess> {
    access: A,
    pane: u32,
    sink: Rc<dyn crate::sink::Sink>,
    renderer: crate::render::stream::StreamRenderer,
    // Buffers non-table output (scalars, Rendered) to fall back to render().
    fallback: Vec<PipelineData>,
    saw_record: bool,
}
```

Implement `FinalConsumer` for it:
- `item(x)` (sync): if `x` is a `Value::Record`, set `saw_record`, `renderer.push(record)`, and if it returns `Some(text)` paint it — `access.with(|e| e.emit_output(pane, &text))` then `access.events_ready()` (after the borrow drops). If `x` is not a record, push to `fallback` (mixing a live table and scalars is rare; render fallback at finish).
- `ready()`: return `self.sink.ready()` (clone the `Rc` — the throttle). The collector awaits this between items.
- `finish()`: if `saw_record`, paint `renderer.finish()` (via `access.with` + `events_ready`) and return `Empty` (already painted). Else fall back to the old collect+`render` for `fallback` and return that (so scalars/`Rendered`/help still render once, unchanged).

Add an engine method for the host timer (Task 5 calls it):

```rust
impl Engine {
    /// Host deadline fired: commit whatever the progressive renderer has
    /// probed so far, so a slow source paints without waiting for more rows.
    /// A no-op if there is no pending progressive render.
    pub fn commit_pending_render(&mut self, pane: u32) {
        // Delegate to the active ProgressiveConsumer's renderer.commit();
        // store a handle to the in-flight renderer on the pane/run so this
        // can reach it. See note below.
    }
}
```

**The wiring problem for `commit_pending_render`:** the `ProgressiveConsumer` lives inside the `execute_line` future, not on `Engine`. To let the host timer reach its `StreamRenderer`, store the renderer behind a shared handle the consumer and the engine both hold — e.g. `Rc<RefCell<Option<StreamRenderer>>>` kept on the `Pane`/run registry, set when the consumer starts and cleared at finish. `commit_pending_render` borrows that handle, calls `commit()`, and emits the text. Keep the `RefCell` borrow off any `.await`. If this proves awkward, an acceptable simpler v1: **row-count + end-of-stream commit only** (no host deadline) — a slow finite source still paints at end; only a slow *infinite* source waits. Ship that if the shared-handle wiring risks the borrow invariant, and note the deadline as a follow-up. Report which you did.

In `execute_line`, replace the collecting terminal path: build a `ProgressiveConsumer` (with the `PaneSink` as its `sink`) and pass it to `eval_line`/`eval_pipeline`; after `drive()`, `consumer.finish()` handles the last paint. The parse-error and pipeline-error paths are unchanged. `run()`/`eval_to_value` keep using the `CollectingConsumer`.

- [ ] **Step 4: Run, verify pass**

Run: `cargo test -p bterm-core engine::tests::a_finite_table` → PASS.
Run: `cargo test --workspace` → 203. Existing engine tests (which assert pane output for `echo`, tables, help, errors) must stay green — the finite path is byte-identical.
Run: `cargo clippy --workspace --all-targets` → clean.

CLI check (CLI uses the collecting consumer, so unchanged):
```bash
printf "echo '[{\"id\":1},{\"id\":2}]' | from json\n" | cargo run -q -p bterm-cli 2>&1 | tail -8
```
Expected: the box table, identical to before. Report it.

- [ ] **Step 5: Commit**

```bash
git add crates/bterm-core/src/engine.rs
git commit -m "Paint the pane progressively as records stream

execute_line routes the interactive pane through a ProgressiveConsumer:
records feed the StreamRenderer (probe -> commit -> stream), painting as
they arrive and awaiting sink.ready() between. A finite result inside the
probe renders byte-identically -- the gate. run()/CLI stay on the
collecting consumer. Records the deadline-wiring decision."
```

---

## Task 5: Host-driven time bound + pane throttle (wasm)

**Files:**
- Modify: `crates/bterm-wasm/src/lib.rs`, `crates/bterm-wasm/src/tasks.rs`, `crates/bterm-core/src/engine.rs` (`PaneSink`).

This is wasm-only (no native test); Task 6 proves it in the browser.

- [ ] **Step 1: `PaneSink::ready()` throttles**

`PaneSink` (in `engine.rs`) currently writes and calls `events_ready()`. Give it a `ready()` that yields cooperatively so a fast producer returns to the executor between paints rather than spinning — mirroring the stage-2 `yield_once` pattern (self-wake then `Pending` once). This bounds paint rate to the drive loop's pace without a timer:

```rust
impl<A: EngineAccess> crate::sink::Sink for PaneSink<A> {
    // ...existing write()...
    fn ready(&self) -> crate::registry::LocalBoxFuture<()> {
        Box::pin(async {
            let mut yielded = false;
            std::future::poll_fn(move |cx| {
                if yielded { std::task::Poll::Ready(()) }
                else { yielded = true; cx.waker().wake_by_ref(); std::task::Poll::Pending }
            }).await
        })
    }
}
```

This alone gives cooperative throttling with no host timer. Confirm `cargo test --workspace` still 203 and the native progressive test still passes (the extra yield changes timing, not output).

- [ ] **Step 2: The probe deadline via `setTimeout`**

In `crates/bterm-wasm/src/lib.rs`, when a pane run starts producing, arm a `setTimeout(T_MS)` (≈150ms) whose callback calls into the engine's `commit_pending_render(pane)` (through `WasmAccess::with`, then `events_ready()`), then clears itself. Cancel/replace it when the run finishes. Use `web_sys::Window::set_timeout_with_callback_and_timeout_and_arguments_0` and a `Closure`; store the timeout id alongside the run in `tasks.rs` so `finish`/abort clears it. `T_MS` a `const` with a comment that it is the probe's first-paint deadline.

If Task 4 shipped the row-count-only fallback (no `commit_pending_render`), skip this step and note it — the deadline becomes a follow-up.

- [ ] **Step 3: Build and verify**

Run: `just build` → succeeds.
Run: `cargo test --workspace` → 203 (wasm-only change).
Run: `cargo clippy --workspace --all-targets` → clean.

- [ ] **Step 4: Commit**

```bash
git add crates/
git commit -m "Pane throttle + host-driven probe deadline

PaneSink::ready() yields cooperatively so a fast producer paints at the
drive loop's pace instead of spinning. A setTimeout arms the probe's
first-paint deadline, calling commit_pending_render so a slow live source
paints within T of its first row -- the clock the core lacks, supplied by
the host."
```

---

## Task 6: Browser proof

**Files:**
- Modify: `packages/demo/tests/smoke.spec.ts`; possibly `packages/demo/src/main.ts` (a deterministic streaming source if `watch` timing is flaky).

- [ ] **Step 1: Build**

```bash
just build && npm --prefix packages/demo run build
```

- [ ] **Step 2: Progressive-paint browser test**

Add to `packages/demo/tests/smoke.spec.ts`. Use a command that emits rows over time and assert the pane shows a header/early rows *before* the stream ends. A clean approach: register (in the test) a generator that yields a row every ~60ms, run it piped to nothing (so it paints to the pane, not captured by `run()`), and poll the pane text for an early row while later rows haven't been produced yet:

```ts
test('a streaming table paints rows before the source finishes', async ({ page }) => {
  await page.goto('/');
  await waitForTerminal(page);

  await page.evaluate(() => {
    window.bt.registerCommand({ name: 'drip', summary: 'a row every 60ms' }, async function* () {
      for (let i = 0; i < 6; i++) {
        await new Promise((r) => setTimeout(r, 60));
        yield { id: i };
      }
    });
    // Fire-and-forget into the pane (not run(): we want it painted).
    // The panel exposes a way to run a line in the pane; use the same path
    // the prefix keymap uses, or type it. Simplest: type it into the pane.
  });

  // Type `drip` + Enter into the pane's textarea.
  await page.evaluate(() => {
    const ta = document
      .querySelector('[data-browser-terminal]')!
      .shadowRoot!.querySelector('.xterm-helper-textarea') as HTMLTextAreaElement;
    ta.focus();
    for (const ch of 'drip') {
      ta.value = ch;
      ta.dispatchEvent(new InputEvent('input', { data: ch, inputType: 'insertText', bubbles: true }));
    }
    ta.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', keyCode: 13, bubbles: true }));
  });

  const paneText = () =>
    page.evaluate(
      () =>
        document.querySelector('[data-browser-terminal]')!.shadowRoot!.querySelector('.xterm-screen')?.textContent ?? '',
    );

  // Within the drip window (< 360ms of rows), the header + an early row must
  // already be on screen — proof of progressive paint, not paint-at-end.
  await page.waitForFunction(
    () =>
      (document.querySelector('[data-browser-terminal]')!.shadowRoot!.querySelector('.xterm-screen')?.textContent ?? '').includes('id'),
    null,
    { timeout: 1000 },
  );
  const mid = await paneText();
  expect(mid).toContain('id'); // header painted mid-stream
});
```

If the DOM-scrape of `.xterm-screen` is unreliable (it was earlier in this project), assert via a `read`-style probe on the xterm buffer instead, or add a tiny hook. Adjust until the assertion is real; do not weaken it to always-pass. Report how you made the observation reliable.

- [ ] **Step 3: Fast source doesn't freeze**

Add a second assertion: a generator yielding 2000 rows as fast as possible still completes and the tab stays responsive (a follow-up `run('echo 42')` resolves promptly afterward). This exercises the throttle.

- [ ] **Step 4: Verify + size**

```bash
cd packages/demo && npx playwright test
```
Expected: existing 12 + your 1-2 new pass.

```bash
ls -l packages/browser-terminal/dist/wasm/bterm_wasm_bg.wasm | awk '{print $5}'
```
Pre-stage-5a: 456272 bytes. Report the delta; if > 2KB, update `README.md`'s size line.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "Browser proof: streaming tables paint progressively

A source dripping rows over time shows its header and early rows before it
finishes, and a fast 2000-row source stays responsive via the throttle.
Records the wasm size."
```

---

## Self-review notes

**Spec coverage:** progressive structured render via probe (Task 1 + 4), time-bounded probe with host clock (Task 5, with a documented row-count-only fallback if the wiring risks the invariant), `Sink::ready()` (Task 2), pane throttle/interleaving (Tasks 4-5), the finite-pipeline byte-identical gate (Tasks 3-4). The **output buffering API** (allowlist sanitizer, `ctx.log`/`ctx.err` writers, byte/line/block modes, progress bars) is explicitly the separate stage-5b plan — not here.

**Known soft spots (flag to reviewers):**
- The `commit_pending_render` wiring (reaching the in-flight `StreamRenderer` from a host timer) is the trickiest borrow question; Task 4 offers a row-count-only fallback if it endangers the no-borrow-across-await invariant.
- Probe defaults (50 rows / 150ms) are guesses to measure.
- The Task-6 pane-scrape observation has been unreliable before; the task requires making it genuinely observe progressive paint, not weakening it.

**Gate discipline:** Task 3's collecting consumer must reproduce the old terminal collector exactly (all-`Rendered` join; 0/1/N cases) — copy that logic verbatim, do not re-derive it.
