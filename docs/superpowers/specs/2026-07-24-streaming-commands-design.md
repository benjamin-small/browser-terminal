# Streaming commands (stage 3) — design

Status: approved, not yet implemented
Part of: [2026-07-23-output-channels-and-streaming-design.md](2026-07-23-output-channels-and-streaming-design.md), stage 3 of 6
Builds on: stage 1 (channels/sink, merged), stage 2 (bounded channel + concurrent stage driver, merged)

## Problem

Stage 2 replaced the pipeline's sequential await-chain with bounded channels
and a concurrent stage driver, but every command still collects its whole
input before running. So a live or unbounded source — a DOM event stream, an
SSE feed, a paginated fetch — cannot be consumed: the first stage would drain
it forever.

Stage 3 makes commands incremental. `map` / `filter` / `grep` / `head` /
`take` process items as they arrive; `head 5` terminates an infinite upstream
by ceasing to read. The transport for this already exists (stage 2's channels
carry the items and their close semantics are the termination mechanism) — this
stage changes the `Command` contract and the commands themselves.

## What this is not

- **Progressive rendering** — stage 5. Stage 3 collects the output stream at
  the end for rendering, so an infinite source *without* a `head` never renders.
  That is correct, not a defect.
- **Pane self-throttling / `Sink::ready()`** — stage 6.
- **Output schemas** — a command advertising its output shape. Deliberately
  not attempted: a command's output shape is invocation-dependent (`cmd` emits
  records, `cmd --help` emits lines), so there is no single static schema to
  advertise. Handled structurally instead (see "Item tags").

Unlike stage 2, **stage 3 is not behaviour-preserving.** The batch model
below changes some edge-case semantics on purpose; those test updates are
expected, not regressions.

## The stream model

A pipeline stage receives a **stream of items** and emits a stream of items.
Each item is a `PipelineData` — a `Value` (record, scalar, or list) or a
`Rendered` (trusted, pre-styled text). The stream is the stage-2 bounded
channel; nothing new is needed to carry it.

Two reconciliation rules connect the streaming world to the collecting
commands that still return one whole value. Each lives in exactly one place:

- **Flatten on the way out.** When a *collecting* producer returns a `List`,
  its adapter emits each element as a separate item — **one level only**.
  `[[1,2],[3,4]]` becomes two items, not four.
- **Collect on the way in.** When a *collecting* consumer receives a stream,
  its adapter gathers the items back: N → `List`, exactly 1 → that value
  (not wrapped), 0 → `Empty`.

Because both rules live in the collecting adapter, **streaming commands only
ever see individual items and never flatten.** A fully-collected pipeline
round-trips to byte-identical output: `links | map @host` flattens the one
`List` to N row-items, `map` transforms each, and the terminal re-collects
them into the same table as before.

### The terminal collector

The last stage's output stream is collected for `run()`'s `value` and for the
renderer, by the same collect rule: a stream of `Value` records → a table; a
stream of `Rendered` items → each printed verbatim (styled); one item → that
value; zero → `Empty`/`Null`.

### Item tags: `Value` vs `Rendered`

The "what shape does this stream carry" question is answered per-item, not
per-command, by the existing `PipelineData` variants:

- `Value` items carry data — records, scalars, lists.
- `Rendered` items carry trusted, pre-styled text (help output, `table`).

`--help` is intercepted in `eval_call` *before* a command's `run` runs, so
`cmd --help` producing a line-stream is the **evaluator's** decision, requiring
nothing of the command. The interception changes from emitting one `Rendered`
blob to emitting **one `Rendered` item per line**. Then:

- `cmd --help` at a terminal prints its styled lines verbatim (unchanged look).
- `cmd --help | grep flag` filters lines: `grep` consumes `Rendered` items,
  and `Rendered::into_value()` (which already yields `Str`) is exactly the
  "trust drops when a downstream stage consumes it" rule from the parent spec.
  So grep's output is plain, sanitized `Str` values — closing the laundering
  path where page-controlled help text could ride out on a trusted stream.

## The `Command` contract (Approach B)

```rust
pub trait Command {
    fn signature(&self) -> &Signature;
    fn run(
        &self,
        ctx: ExecContext,
        call: BoundCall,
        input: chan::Receiver,
        output: chan::Sender,
    ) -> LocalBoxFuture<Result<(), ShellError>>;
}
```

Rejected alternatives: **A** (every command rewritten to the streaming
signature — needless churn for ~30 trivially-collecting builtins) and **C**
(`PipelineData` collapses into a lazy stream handle — the largest rewrite,
most alien to the codebase). B has the smallest blast radius and keeps the
reconciliation logic in one place.

### Collecting commands are untouched

The ~30 collecting builtins keep their exact current signature,
`fn(ExecContext, BoundCall, PipelineData) -> Result<PipelineData, ShellError>`.
The `Builtin` adapter's `run` does the reconciliation:

1. drain `input` (all items) and **collect** to one `PipelineData`;
2. call the unchanged fn;
3. **flatten** its result into `output`.

So `sort-by` fed a stream of N record-items: the adapter collects them to a
`List`, calls `sort_by(List)`, flattens the sorted `List` back to N items.
The fn never knows streaming exists.

### Streaming commands

`map` / `filter` / `grep` / `head` / `take` get a `StreamingBuiltin` adapter
and are rewritten to loop over `input.recv()`, transforming and sending per
item. They handle `Rendered` items by converting to `Str` (via `into_value`)
before applying their operation, which is what drops the trust tag.

### `JsCommand`

Learns to stream. An async generator's `yield`s become items; the adapter
ladder from the parent spec still holds — a plain array return becomes one
flattened `List`, not N items, so existing TS commands are unaffected. Early
termination calls `.return()` on the generator (running its `finally`) and
fires `ctx.signal`.

### `eval.rs` gets simpler

`run_stage` currently drains the upstream `Receiver` into a `PipelineData`
before calling `cmd.run`. With `run` taking the `Receiver` directly, that
draining moves into the collecting adapter, and `run_stage` just wires each
command's `Receiver`/`Sender` and awaits it. The first stage receives an
already-closed empty `Receiver` (its `recv()` returns `None` at once); the
last stage's `Sender` feeds the terminal collector.

The single-call fast path from stage 2 is reworked to hand the one command an
empty-closed `Receiver` and a collecting `Sender`, rather than calling the old
`PipelineData` signature.

### Errors mid-stream

A streaming command may `send` several items and *then* hit an error. It
returns `Err(ShellError)` exactly as today; stage 2's driver already records
the first error and stops the pipeline, and downstream stages see their
channel close. Items sent before the error were real output and are not
un-sent — but because rendering is collected at the end (stage 3), a failed
pipeline still surfaces the error rather than a half-table, matching the
current "a failed pipeline shows the error" behaviour. Progressive rendering
(stage 5) will revisit how partial output and a later error coexist.

## Which commands stream

| command | mode | reason |
|---|---|---|
| `head`, `take` | streaming | Stop reading after N and drop the receiver — the mechanism that terminates an infinite upstream. |
| `map`, `filter`, `grep` | streaming | So they sit between a live source and `head` without collecting. |
| `sort-by`, `length`, `to json`, `table`, `echo`, `get` | collecting | Each needs the whole input. Before `head` on an infinite source they correctly hang — the honest semantics of `sort \| head` on `tail -f`. Documented. |

The rule: **stream if the command can decide per item; collect if it needs
the whole input.**

## Cancellation and the `watch` demo

`head 5` reads five items, then stops and drops its `Receiver`. The channel
flips to receiver-closed; the producer's next `send` returns `Err(Closed)` and
the producing command returns. For a TS streaming command, the closed channel
becomes a `.return()` on the generator plus a fired `ctx.signal`, so a
`watch`-style command that registered a DOM listener tears it down in its
`abort` handler / `finally`. Ctrl-C uses the identical path.

The vanilla demo ships **`watch <event>`** (default `click`), an async
generator that streams DOM events off the host page and removes its listener
on abort. This ties the flagship to the project's thesis — a terminal that
observes the live page — and it is drivable by Playwright.

## What observably changes

- **Unchanged** (must stay green): every table pipeline, e.g.
  `links | filter … | head 5 | to json`, by the round-trip guarantee.
- **May shift** (update the test with a note): `echo 5 | length` (length of a
  one-item stream), `get` over a flattened stream, and anything that assumed
  "input is exactly one `List` value."

## Testing

- **Native (the bulk):** a synthetic streaming fixture command plus a
  deterministic interval "ticker" prove incremental flow; `head`
  early-termination actually stopping the producer (not just the consumer);
  `filter` between a live source and `head` not collecting the source; and the
  flatten↔collect round-trip on nested lists.
- **Safety re-test:** the stage-2 borrow-invariant test
  (`concurrent_stages_never_hold_overlapping_engine_borrows`) becomes
  genuinely falsifiable now that streaming stages overlap in flight — its
  comment stops overclaiming.
- **Browser:** `watch click | map @target.tagName | head 3` — click three
  times, assert three tag names, assert the listener is gone afterward. The
  existing 11 Playwright tests stay green.

## Delivery shape (for the plan)

Roughly, in dependency order:

1. `Command` trait signature change; `Builtin` collecting adapter does
   drain-collect / flatten-out; all existing tests pass (the collecting
   round-trip is behaviour-preserving for collected pipelines).
2. `StreamingBuiltin` adapter; convert `head`/`take` to streaming; prove early
   termination stops a producer.
3. Convert `map`/`filter`/`grep` to streaming; prove a filter between source
   and `head` does not collect.
4. `--help` interception emits per-line `Rendered` items; `cmd --help | grep`
   works.
5. `JsCommand` streaming (async generators, `.return()` on early close);
   `ctx.signal` fires on downstream termination.
6. `watch <event>` demo command + browser verification; wasm size delta
   recorded.

## Risks

| risk | mitigation |
|---|---|
| The trait change ripples into every `impl Command` (2 production, 4 test fixtures) | Step 1 lands the signature + collecting adapter with all tests green; fixtures updated there. The collecting round-trip is behaviour-preserving, so it is a gate like stage 2's. |
| A streaming command holds an engine borrow across `recv().await` | The borrow discipline is unchanged; `with_engine` closures never await. The stage-2 borrow test, now falsifiable, guards it. |
| `head` fails to actually stop the producer (drains anyway) | Explicit test: a producer that increments a counter on every send, asserted to stop at N. |
| Batch-model edge changes slip in silently | The "may shift" list is checked item by item; unexpected diffs in table pipelines fail the gate. |
| wasm growth | Measured and recorded, as in stage 2 (which added 9.2 KB). |
