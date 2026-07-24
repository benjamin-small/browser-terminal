# Output channels and streaming pipelines — design

Status: approved, not yet implemented
Supersedes: the `PipelineData::Stream` seam noted in the v1 design
Companion: [2026-07-16-browser-terminal-design.md](2026-07-16-browser-terminal-design.md)

## Problem

The shell has one textual output path — `ctx.emit(line)` → `HostHooks::emit_line`
→ pane — and it is unlabelled. A command cannot distinguish "here is a
warning" from "here is progress chatter", the host cannot style or filter
either, and `bt.run()` called programmatically scribbles on whatever pane
happens to be active.

Separately, a pipeline collects: every stage runs to completion and hands a
whole `Value` to the next. That forecloses live sources (a DOM observer, an
SSE feed, a paginated fetch), makes large results freeze the pane until
complete, and holds every intermediate result in memory.

Both are addressed here because the second constrains the first: if
emissions are designed as one-shot side effects, streaming becomes a
redesign rather than an addition.

## What this is not

Deliberately out of scope, in the order they should be picked up:

- **Redirection grammar** (`2>`-style syntax). Channel *numbers* are reserved
  here so the grammar has something to refer to, but no syntax is added. Two
  constraints are already known and recorded below.
- **Output schemas** — commands advertising the shape of what they emit,
  feeding `--help` and possibly static pipeline checking. Orthogonal; slots
  into `Signature` without touching transport.
- **GNU flag mechanics** — combinable short flags (`-iv`), `--` terminator,
  flags after positionals. Independent of this work. Note that `-iv` today
  is not silently mis-parsed; it falls through to a bareword and produces a
  misleading "takes at most 1 positional arguments" error. The diagnostic
  should be fixed with that spec.

GNU *vocabulary* (renaming `sort-by` → `sort -k id -n`) was considered and
**rejected**. `-k` denotes a column number because whitespace-delimited text
has no field names; we have field names. `-n` would be a no-op because
`sort-by` already type-ranks via `total_cmp_values`. Partial familiarity is
worse than none: a `sort` that accepts `-k` but not `-t`/`-u`/`-b` promises
compatibility it cannot honour.

## The channel model

Three channels. **Only two have a write API.**

| ch | name | how a command produces it | flows to |
|----|------|---------------------------|----------|
| 1  | data | return value / yielded items | the next pipeline stage |
| 2  | err  | `ctx.err(line)` | the sink — never the pipe |
| 3  | log  | `ctx.log(line)` | the sink — never the pipe |

Channel 1 has no write method. Data is the return value or a `yield`. This
is the load-bearing decision: **"diagnostics are locked out of the data
pipe" is enforced structurally, not by discipline.** There is no API that
pushes text into a pipe, so no command can do it by accident, and no stage
needs forwarding boilerplate.

`log` and `err` records never enter the inter-stage channel at all. They go
out-of-band to a per-run sink shared by every stage of the pipeline. Only
`Value`s travel between stages.

Channel 2 is a **destination, not a verdict.** A thrown `ShellError` still
aborts the pipeline and still renders with its caret — it renders *to*
channel 2. `ctx.err()` is the case we currently cannot express: warn and
continue. This mirrors POSIX, where stderr is a stream and fatality is a
separate concern.

### The sink

```rust
pub enum Channel { Log, Err }

/// Text is sanitized on construction, so a `Record` cannot hold an escape
/// sequence.
pub struct Record { channel: Channel, text: String }

impl Record {
    pub fn log(text: impl AsRef<str>) -> Self { /* sanitizes */ }
    pub fn err(text: impl AsRef<str>) -> Self { /* sanitizes */ }
}

pub trait Sink {
    fn write(&self, record: Record);
    /// Resolves when the sink can accept more. Pane sinks use this to
    /// throttle; capturing and test sinks return immediately.
    /// **Deferred to stage 6** — until there is an await site, an
    /// always-ready async method is complexity for nothing.
    fn ready(&self) -> LocalBoxFuture<()>;
}
```

> **Amended during stage 1.** `Record` was originally specified as an enum
> with public `Log(String)` / `Err(String)` variants, with sanitization
> living in `PaneSink`. A security test caught the flaw: only the terminal
> sink sanitized, so `bt.run()`'s `{ log, err }` came back with escape
> sequences intact, and a caller putting them into their own UI or terminal
> received raw page-controlled text. An enum with public variants can always
> be constructed directly, so no constructor could be made mandatory — hence
> a struct with private fields. Sanitizing once at construction makes
> "diagnostics leaving the engine are escape-clean" a property of the type
> rather than something each sink must remember, which is the same reasoning
> that gives channel 1 no write API.

Three implementations, chosen once at pipeline start and carried on
`ExecContext`:

- **pane sink** — writes through to `PaneOutput`, live, as records arrive
- **capturing sink** — accumulates into the `{ log, err }` that `run()` resolves with
- **test sink** — collects into a `Vec` for assertions; no browser required

This is what makes the `run()` decision cheap. "Print to the pane" and "hand
back to the caller" become two implementations of one interface rather than
a conditional threaded through the engine.

### Text streams and trust

`PipelineData::Rendered` exists because untrusted strings have their escape
sequences stripped on the way to the terminal — a page-controlled value must
not be able to clear the screen — while our own styled output would be
destroyed by that same pass.

Streaming makes a one-shot `Rendered` blob untenable. Replace it: a channel-1
stream is tagged **`Data`** or **`Text`**. A `Text` stream's items are
trusted, pre-styled lines, printed verbatim when they reach the renderer.

Concretely, `PipelineData`'s three variants (`Empty` / `Value` / `Rendered`)
collapse into a stream plus a tag. `Empty` becomes a stream that closes with
no items; `Value` a stream carrying one item; `Rendered` a `Text`-tagged
stream. Every existing call site therefore has a direct translation, which is
what makes stage 2 a mechanical refactor rather than a rewrite.

**The tag is dropped the moment the stream is consumed by another stage.**
Piping a text stream into anything yields ordinary strings, sanitised like
any other value. This makes explicit a property today's code gets almost by
accident (`Rendered::into_value()` degrades to `Str`), and closes a
laundering path: without it, `cmd --help | map {|x| $x + injected}` could
ride page-controlled content out on a trusted stream.

Consequence for `help`, which is really two different shapes:

- **`help` (no args)** is a list of `(name, summary)` — that is *data*.
  Emit records, so `help | grep mux`, `help | length`, `help | sort-by name`
  operate on fields rather than lines.
- **`cmd --help`** is a document. Keep it a `Text` stream of styled lines,
  which also makes `cmd --help | grep flag` filter lines properly instead of
  grepping one blob.

The rule for authors: *is this the answer, or commentary about producing the
answer?* Help text is the answer (channel 1). `ctx.log("fetching page 3…")`
is commentary (channel 3).

## Transport

### Stages need no spawner

`eval_pipeline` remains a **single future that owns and drives all N stage
futures**, polling them round-robin and propagating the parent waker to each.

The alternative — `spawn_local` per stage — would fork the design. wasm has a
real executor; the native harness is a hand-rolled `block_on` that polls one
future with a no-op waker. A spawner abstraction would mean the core's own
tests could not exercise concurrent stages. With a combinator, concurrency
comes from the pipeline itself: identical code natively and in the browser,
deterministic interleaving in tests, nothing new required of either host.

### The safety invariant survives

`with_engine` as a sync closure with no borrow crossing `.await` is what
makes `RefCell` panics unreachable by construction. Concurrent stages do not
threaten it: JS is cooperatively scheduled with no preemption, and a borrow
can never span a yield point, so two borrows cannot overlap no matter how
many stage futures are live. This argument must be stated in the code, not
assumed — the existing debug assertion stays.

### The channel

Bounded MPSC, hand-rolled (~200 lines; the workspace deliberately avoids the
`futures` crate). `Rc<RefCell<>>` over a `VecDeque<Value>`, capacity 64, a
waker slot per end, close flags on both.

- full buffer → sender returns `Pending`
- empty and open → receiver returns `Pending`
- empty and closed → end of stream

That bound is the bounded-memory guarantee.

### Termination flows backward

`head 5` takes five items and drops its receiver. The channel flips to
receiver-closed, the producer's next `send` fails, and the producing command
returns.

**Early termination fires `ctx.signal`.** This unifies it with Ctrl-C: an
author writes `fetch(url, { signal: ctx.signal })` once and gets both. In
`fetch … | filter … | head 5`, the HTTP connection genuinely closes rather
than continuing to download rows nobody will read. For a TS async generator
we call `.return()`, so the author's `finally` runs.

Ctrl-C is unchanged: aborting the pipeline future drops the stage futures,
which drops the channels, which propagates identically.

### Two pressures, handled differently

Conflating these is how a live source melts a browser.

- **Between stages** — real backpressure via the bounded channel. A slow
  consumer stalls its producer.
- **Into the pane** — the display cannot render 100k lines/sec and should not
  try. The sink's `ready()` is awaited by the final stage; the TS write queue
  signals drain across the boundary (it already has a 256KB watermark). A
  live source then **self-throttles to display speed** instead of spinning.

The second is the fiddliest part of this design and is built last, behind a
simpler coalescing sink.

## Command classification

The streaming trait shape is invasive if every command must implement it:

```rust
fn run(&self, ctx, call, input: Input, out: Output)
    -> LocalBoxFuture<Result<(), ShellError>>
```

Most builtins have no business doing stream plumbing. **Collecting** commands
(`echo`, `length`, `sort-by`, `table`, `to json`, `tail`) keep today's shape
behind a blanket adapter that drains the input stream and sends one value.
Only genuinely **streaming** commands (`map`, `filter`, `grep`, `head`,
`take`) implement the streaming form. The classification becomes visible in
the type rather than living in a doc comment.

**`sort-by` on an unbounded stream never emits.** That is correct — `sort`
behaves identically in Unix — but it will surprise someone. It belongs in the
docs, and a collecting stage fed by a known-unbounded source should say so in
its error hint.

## TypeScript surface

### The adapter ladder

Applied in order to whatever a command returns:

1. `Symbol.asyncIterator` → stream (async generators; `ReadableStream` where
   async iteration is supported, `getReader()` fallback otherwise)
2. sync generator → stream
3. thenable → await, then re-run the ladder **once** on the result
4. **array or string → a single value, never a stream**
5. anything else → single value

Rule 4 is what keeps this backward-compatible. Every command today returns an
array meaning *one table*. If iterables became streams, `links` would
silently change from returning a table to emitting rows, and `--limit`,
`length`, and all three demos would shift underneath us. Authors say
`yield*` for N items and `yield` for one list — the fork is explicit, and
existing code is untouched.

This "adapt to what you got" approach matches the established idiom: the
current code already wraps returns in `Promise::resolve` to tolerate
sync-or-async.

### Context

```ts
interface CommandCtx {
  signal: AbortSignal;   // Ctrl-C, dispose, AND downstream termination
  log(line: string): void;
  err(line: string): void;
  emit(line: string): void;  // deprecated alias for log()
}
```

### Worked example

```ts
bt.registerCommand(
  {
    name: 'fetch',
    summary: 'Fetch a URL and emit its rows',
    required: [{ name: 'url', shape: 'str', desc: 'the URL to request' }],
    flags: [{ long: 'ndjson', desc: 'parse as newline-delimited JSON' }],
  },
  async function* ({ positionals, flags }, _input, ctx) {
    const url = String(positionals[0]);
    ctx.log(`GET ${url}`);                     // channel 3 — never enters the pipe

    const res = await fetch(url, { signal: ctx.signal });
    if (!res.ok) throw { message: `${res.status} ${res.statusText}`, help: `GET ${url}` };

    if (!flags.ndjson) {
      const rows = await res.json();           // JSON is not delimited; must complete
      yield* rows;                             // N items ( `yield rows` = ONE list )
      return;
    }

    let n = 0;
    for await (const line of lines(res.body)) {
      if (line.trim()) yield JSON.parse(line);
      if (++n % 1000 === 0) ctx.log(`${n} rows…`);
    }
  },
);
```

**A limitation to document honestly:** `fetch /api/path.json` cannot stream.
JSON is not delimited; the whole body must arrive before it parses. Streaming
pays off on NDJSON, SSE, paginated APIs, and DOM observers. "Streaming shell"
invites the assumption that everything streams.

### `run()`

Resolves to `{ value, log, err }` instead of a bare value. Two consequences:

- **Breaking API change.** The demos and README destructure it; migration is
  mechanical but must be done in the same commit.
- **`run()` on an unbounded stream never resolves.** Callers pipe through
  `head` or pass an abort signal. Inherent, not a defect — but it needs a doc
  line, not a support question later.

## Rendering

Table widths need all rows; streams do not have them.

The renderer collects until **either** end-of-stream **or** a probe window
fills (~50 rows / ~100ms). If the stream ends inside the window — every
bounded source, i.e. essentially everything today — output is
pixel-identical to current behaviour, so no existing demo, snapshot, or
Playwright assertion regresses.

Only genuinely long streams switch to fixed-width mode, taking widths from
the probe and truncating with the existing `…` shrink. A late row carrying
keys absent from the header gets one note on channel 2 rather than silent
data loss.

## Delivery

Six stages. The second is what makes this safe.

1. **Channels + sink** — `ctx.log`/`ctx.err`, three sink impls, `run()` shape
2. **Bounded channel + stage driver, every command still collecting** — the
   transport rewrite lands as a *pure refactor*, verified by all 165 existing
   tests passing untouched
3. **Streaming commands** — `map`, `filter`, `grep`, `head`; early
   termination; signal unification
4. **TS adapter ladder** — generators, `ReadableStream`
5. **Streaming render** — probe window
6. **Pane self-throttling** — async `ready()`, drain signalling

Stage 2 is deliberate: the risky part (concurrent stages, wakers,
backpressure) goes in while behaviour is provably identical. If the suite
passes unchanged, the transport is sound before any semantics move.

> **Size note, recorded after stage 2.** The bounded channel plus concurrent
> stage driver moved wasm from 438188 bytes (~438 KB raw, ~193 KB gzipped) to
> 447388 bytes (~447 KB raw, ~197 KB gzipped) — a ~9.2 KB raw increase for
> hand-rolled MPSC plumbing and the round-robin driver, with behaviour
> unchanged (all 185 native tests and all 11 Playwright tests pass
> untouched). See README.md for the current headline number.

## Testing

The test sink makes all of this native and deterministic — round-robin
polling gives reproducible interleaving. Beyond the existing suite:

- early termination actually stops the producer (not just the consumer)
- the bounded channel caps memory under a fast producer and slow consumer
- a collecting stage between two streaming stages
- a streaming stage fed by a collecting one
- **`log`/`err` can never appear in piped data** (security property)
- **trust is dropped on pipe** — `cmd --help | map {|x| …}` sanitises
- `Text` and `Data` streams both render correctly at pipeline end

The last two are security tests, not ergonomics tests, and should be
labelled as such.

## Constraints recorded for the future redirection spec

- **`>` is unavailable.** The closure work made it a comparison operator
  (`Op::Gt`, precedence 3). `echo hi > out.txt` currently errors with
  "`>` can only be used inside a closure". Redirect grammar must either
  reclaim `>` positionally outside closure bodies or choose other syntax.
- **There are no files.** Redirect targets must be non-file: discard
  (`/dev/null`), a shell variable, a registered JS sink, the clipboard, or a
  download. This is why fd *numbers* alone do not justify grammar — they
  earn their keep in POSIX almost entirely through file redirection.

## Risks

| risk | mitigation |
|------|------------|
| Concurrent stages break the `with_engine` invariant | Cooperative scheduling means borrows cannot overlap; argument stated in code, debug assertion retained; stage 2 lands as a behaviour-preserving refactor |
| Hand-rolled channel has waker bugs | Native, deterministic tests; no-op waker round-robin makes interleaving reproducible |
| Streaming changes existing output | Probe window makes bounded sources pixel-identical; stage 2 requires the existing suite to pass untouched |
| Array-as-stream breaks every command | Rule 4 carve-out; `yield*` vs `yield` makes intent explicit |
| Live source floods the pane | Bounded channels between stages, `ready()` self-throttling into the pane |
| `run()` API break | Mechanical migration in the same commit; documented |
