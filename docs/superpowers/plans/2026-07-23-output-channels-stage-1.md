# Output Channels (Stage 1) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give the shell three named output channels — data (the return value), err, and log — where diagnostics reach the terminal through a swappable sink instead of a hardcoded pane write, and can be captured by programmatic callers.

**Architecture:** A new `sink` module in `bterm-core` defines a `Record` enum (`Log`/`Err` only — data has no variant, which is what makes "diagnostics cannot enter the pipe" structural) and a `Sink` trait with three implementations: pane, collecting, and test. `ExecContext` carries an `Rc<dyn Sink>`, replacing `HostHooks::emit_line`. TypeScript commands get `ctx.log()` and `ctx.err()`, and `bt.run()` resolves to `{ value, log, err }`.

**Tech Stack:** Rust (`bterm-core`, `bterm-wasm`, `bterm-cli`), wasm-bindgen, TypeScript, Vitest-free (cargo + Playwright).

**Scope:** This is stage 1 of six from [the spec](../specs/2026-07-23-output-channels-and-streaming-design.md). It deliberately contains **no streaming** — no bounded channel, no stage driver, no `Sink::ready()`. Those arrive in stages 2 and 6. Adding `ready()` now would mean an always-ready async method with no await site, which is complexity for nothing.

---

## Context an engineer needs before starting

**The `with_engine` discipline.** Engine state lives in a `thread_local!` `RefCell` reached only through a synchronous closure. No borrow may cross an `.await`. Code inside that closure must never call into JS — it queues `EngineEvent`s that are flushed after the borrow drops. Every change below respects this; `Sink::write` is synchronous for exactly this reason.

**Two bugs this stage fixes.** Both were found while writing the spec:

1. **Emitted text is never sanitized.** `HostHooks::emit_line` is called in exactly one place (`crates/bterm-wasm/src/js_command.rs:58`) and flows to `emit_output` → `crlf` → `PaneOutput` with no escape stripping. `sanitize()` exists in `crates/bterm-core/src/render/mod.rs:104` but is only applied to `Value::Str` during rendering. So a TS command calling `ctx.emit("\x1b[2J")` can clear the user's screen. This is the same class of bug as the `[1mUsage:[0m` litter, on the one path that accepts arbitrary page-controlled text.

2. **Programmatic runs write to the pane.** `bt.run()` is documented as "no prompt echo, no pane render", but `ctx.emit` inside a command still targets the pane. Stage 1's capturing sink is the fix.

**Sanitization rule.** Diagnostics from commands are untrusted and get `strip_escapes(s, false)` applied — single-line, escape-free. Styling is applied by the sink *after* sanitizing, so our own colour codes survive and the command's cannot be injected.

---

## File Structure

| File | Responsibility |
|---|---|
| `crates/bterm-core/src/sink.rs` | **Create.** `Record`, `Sink`, `CollectingSink`. No engine or host dependencies. |
| `crates/bterm-core/src/lib.rs` | **Modify.** Register `pub mod sink;`. |
| `crates/bterm-core/src/registry.rs` | **Modify.** Add `sink` to `ExecContext`; delete `HostHooks::emit_line`. |
| `crates/bterm-core/src/render/mod.rs` | **Modify.** Export `strip_escapes` for sink use. |
| `crates/bterm-core/src/engine.rs` | **Modify.** `PaneSink`; `make_ctx`/`execute_line`/`eval_to_value` take a sink. |
| `crates/bterm-core/src/builtins/mod.rs` | **Modify.** Test host/ctx construction. |
| `crates/bterm-cli/src/main.rs` | **Modify.** `CliSink` — log to stdout, err to stderr. |
| `crates/bterm-wasm/src/js_command.rs` | **Modify.** `ctx.log`/`ctx.err`/`ctx.emit` closures. |
| `crates/bterm-wasm/src/lib.rs` | **Modify.** `run()` builds `{ value, log, err }`. |
| `packages/browser-terminal/src/types.ts` | **Modify.** `CommandCtx`, `RunResult`. |
| `packages/browser-terminal/src/index.ts` | **Modify.** `run()` return type. |
| Demos, tests, README | **Modify.** Migrate `run()` call sites. |

---

## Task 1: `Record` and `Sink` with a collecting implementation

**Files:**
- Create: `crates/bterm-core/src/sink.rs`
- Modify: `crates/bterm-core/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/bterm-core/src/sink.rs` containing only the test module for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collecting_sink_separates_channels() {
        let sink = CollectingSink::new();
        sink.write(Record::Log("progress".into()));
        sink.write(Record::Err("uh oh".into()));
        sink.write(Record::Log("more".into()));

        assert_eq!(sink.log_lines(), vec!["progress", "more"]);
        assert_eq!(sink.err_lines(), vec!["uh oh"]);
    }

    #[test]
    fn take_drains_so_a_sink_can_be_reused() {
        let sink = CollectingSink::new();
        sink.write(Record::Log("one".into()));
        assert_eq!(sink.take().len(), 1);
        assert!(sink.take().is_empty());
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p bterm-core sink::`
Expected: FAIL — `cannot find type CollectingSink in this scope`.

- [ ] **Step 3: Write minimal implementation**

Put this above the test module in `crates/bterm-core/src/sink.rs`:

```rust
//! Where a pipeline's diagnostic output goes.
//!
//! Three channels exist; only two appear here. Channel 1 (data) is the
//! pipeline's return value and has no write API at all — which is what makes
//! "diagnostics can never enter the data pipe" a property of the type system
//! rather than a rule authors must remember.

use std::cell::RefCell;

/// A line written to a diagnostic channel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Record {
    /// Channel 3 — progress and commentary.
    Log(String),
    /// Channel 2 — warnings and diagnostics. Non-fatal: a thrown
    /// `ShellError` still aborts the pipeline. This is the case we
    /// previously had no way to express, "warn and keep going".
    Err(String),
}

impl Record {
    pub fn text(&self) -> &str {
        match self {
            Record::Log(s) | Record::Err(s) => s,
        }
    }
}

/// Destination for diagnostics. Synchronous by design: implementations are
/// called from inside `with_engine` borrows, where awaiting is forbidden.
pub trait Sink {
    fn write(&self, record: Record);
}

/// Accumulates records for later retrieval. Backs programmatic `run()` and
/// every native test.
#[derive(Default)]
pub struct CollectingSink {
    records: RefCell<Vec<Record>>,
}

impl CollectingSink {
    pub fn new() -> Self {
        Self::default()
    }

    /// Drain everything written so far.
    pub fn take(&self) -> Vec<Record> {
        std::mem::take(&mut self.records.borrow_mut())
    }

    pub fn log_lines(&self) -> Vec<String> {
        self.lines(|r| matches!(r, Record::Log(_)))
    }

    pub fn err_lines(&self) -> Vec<String> {
        self.lines(|r| matches!(r, Record::Err(_)))
    }

    fn lines(&self, keep: impl Fn(&Record) -> bool) -> Vec<String> {
        self.records
            .borrow()
            .iter()
            .filter(|r| keep(r))
            .map(|r| r.text().to_string())
            .collect()
    }
}

impl Sink for CollectingSink {
    fn write(&self, record: Record) {
        self.records.borrow_mut().push(record);
    }
}

/// Discards everything. For paths that have no destination yet.
pub struct NullSink;

impl Sink for NullSink {
    fn write(&self, _record: Record) {}
}
```

Add to `crates/bterm-core/src/lib.rs` after line 22 (`pub mod signature;`), keeping alphabetical order:

```rust
pub mod sink;
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p bterm-core sink::`
Expected: PASS — 2 tests.

- [ ] **Step 5: Commit**

```bash
git add crates/bterm-core/src/sink.rs crates/bterm-core/src/lib.rs
git commit -m "Add Record and Sink: diagnostics get a destination

Channel 1 has no variant here on purpose. Data is the pipeline's return
value, so there is no API that writes text into a pipe -- the separation
is structural rather than a rule."
```

---

## Task 2: Expose `strip_escapes` for sanitizing diagnostics

**Files:**
- Modify: `crates/bterm-core/src/render/mod.rs:104`

- [ ] **Step 1: Write the failing test**

Add to the test module at the bottom of `crates/bterm-core/src/render/mod.rs`:

```rust
    #[test]
    fn diagnostic_text_is_stripped_to_one_line() {
        // A page-controlled diagnostic must not be able to clear the screen
        // or smuggle colour codes into our styling.
        let hostile = "\x1b[2J\x1b[Hcleared\nsecond line";
        assert_eq!(diagnostic_text(hostile), "cleared second line");
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p bterm-core render::tests::diagnostic_text`
Expected: FAIL — `cannot find function diagnostic_text`.

- [ ] **Step 3: Write minimal implementation**

Add to `crates/bterm-core/src/render/mod.rs`, next to the existing `sanitize` and `cell_text` helpers around line 104:

```rust
/// Display text for an untrusted diagnostic line (`ctx.log` / `ctx.err`).
///
/// Single-line and escape-free: a command's diagnostic is page-controlled
/// text, so it must not be able to move the cursor, clear the screen, or
/// inject colour codes into the styling the sink applies around it.
pub fn diagnostic_text(s: &str) -> String {
    cell_text(s)
}
```

Note `cell_text` is `strip_escapes(s, false)` — escapes removed *and* newlines collapsed, which is what "one diagnostic is one line" requires.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p bterm-core render::tests::diagnostic_text`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/bterm-core/src/render/mod.rs
git commit -m "Expose diagnostic_text for sanitizing untrusted output

sanitize() only ever covered Value::Str during rendering; the emit path
that accepts arbitrary text from TS commands had no stripping at all."
```

---

## Task 3: Move emission from `HostHooks` onto `ExecContext`

This task is compile-breaking by design: deleting `emit_line` forces every
call site to be updated in one commit rather than leaving two paths alive.

**Files:**
- Modify: `crates/bterm-core/src/registry.rs:69-117` (trait), `:121-130` (struct)
- Modify: `crates/bterm-core/src/engine.rs:444-452` (`make_ctx`), `:375-377` (`EngineHost::emit_line`)
- Modify: `crates/bterm-core/src/builtins/mod.rs:673-685` (`TestHost`), `:687-698` + `:702-714` (`eval` helpers)
- Modify: `crates/bterm-cli/src/main.rs:20-23`
- Modify: `crates/bterm-wasm/src/js_command.rs:54-60`

- [ ] **Step 1: Write the failing test**

Add to the test module in `crates/bterm-core/src/builtins/mod.rs`:

```rust
    #[test]
    fn a_command_writes_diagnostics_to_the_context_sink() {
        let sink = Rc::new(CollectingSink::new());
        let mut registry = CommandRegistry::new();
        register_all(&mut registry);
        let ctx = ExecContext {
            host: Rc::new(TestHost),
            sink: sink.clone(),
            width: 80,
            pane: 0,
            run_id: 0,
        };
        ctx.sink.write(Record::Log("hello".into()));
        assert_eq!(sink.log_lines(), vec!["hello"]);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p bterm-core builtins::tests::a_command_writes`
Expected: FAIL — `struct ExecContext has no field named sink`.

- [ ] **Step 3: Write minimal implementation**

In `crates/bterm-core/src/registry.rs`, **delete** `emit_line` from the `HostHooks` trait (lines 70-71):

```rust
    /// Progressive output: print a line above the pipeline's final render.
    fn emit_line(&self, line: &str);
```

and add the sink to `ExecContext`:

```rust
pub struct ExecContext {
    pub host: Rc<dyn HostHooks>,
    /// Where `log` and `err` records go. Swappable so a programmatic
    /// `run()` can capture what a pane would have printed.
    pub sink: Rc<dyn crate::sink::Sink>,
    /// Render width (pane cols / terminal width).
    pub width: u16,
    /// Pane the pipeline is running in (0 for the CLI).
    pub pane: u32,
    /// Unique id of this pipeline run; the wasm layer keys AbortControllers
    /// by it so TS commands receive the right AbortSignal.
    pub run_id: u64,
}
```

In `crates/bterm-core/src/engine.rs`, **delete** the `emit_line` impl at lines 375-377 and change `make_ctx` to accept a sink:

```rust
fn make_ctx<A: EngineAccess>(
    access: &A,
    pane: u32,
    run_id: u64,
    sink: Rc<dyn crate::sink::Sink>,
) -> ExecContext {
    let cols = access.with(|e| e.pane(pane).map(|p| p.cols).unwrap_or(80));
    ExecContext {
        host: Rc::new(EngineHost { access: access.clone(), pane }),
        sink,
        width: cols,
        pane,
        run_id,
    }
}
```

In `crates/bterm-cli/src/main.rs`, **delete** the `emit_line` impl (lines 21-23).

In `crates/bterm-core/src/builtins/mod.rs`, **delete** `fn emit_line` from `TestHost`, add the imports, and give both `eval` helpers a sink. Replace both `let ctx = ExecContext { ... }` lines with:

```rust
        let ctx = ExecContext {
            host: Rc::new(TestHost),
            sink: Rc::new(crate::sink::NullSink),
            width: 80,
            pane: 0,
            run_id: 0,
        };
```

and add near the other test imports:

```rust
    use crate::sink::{CollectingSink, NullSink, Record, Sink};
```

In `crates/bterm-wasm/src/js_command.rs`, change the emit closure (lines 54-60) from `host.emit_line` to the sink:

```rust
            let sink = ctx.sink.clone();
            // Valid for the duration of the call; a TS command that stashes
            // `emit` and calls it after completing gets a JS error.
            let emit = Closure::<dyn Fn(String)>::new(move |line: String| {
                sink.write(Record::Log(line));
            });
```

and add `use bterm_core::sink::Record;` to that file's imports.

- [ ] **Step 4: Run the whole suite**

Run: `cargo test --workspace`
Expected: PASS — 166 tests in `bterm-core` (165 existing plus the new one). Any failure here is a missed call site, not a behaviour change.

- [ ] **Step 5: Commit**

```bash
git add crates/
git commit -m "Move emission from HostHooks onto a swappable ExecContext sink

emit_line hardcoded 'diagnostics go to the pane', which is wrong for
programmatic run() and untestable without a browser. Deleting it rather
than deprecating it forces every call site over in one commit."
```

---

## Task 4: `PaneSink` — sanitize, style, and route to the pane

**Files:**
- Modify: `crates/bterm-core/src/engine.rs` (add `PaneSink` near `EngineHost`, ~line 364)

- [ ] **Step 1: Write the failing test**

Add to the test module in `crates/bterm-core/src/engine.rs`:

The module's existing harness is `Rc<RefCell<Engine>>` as the `EngineAccess`,
built by the `engine()` helper, with `active_pane()` and `output_text()`
alongside it (`crates/bterm-core/src/engine.rs:552-583`). Use those rather
than adding a parallel one:

```rust
    #[test]
    fn pane_sink_strips_escapes_from_diagnostics() {
        let access = engine();
        let pane = active_pane(&access);
        let sink = PaneSink { access: access.clone(), pane };

        sink.write(Record::Log("\x1b[2Jcleared".into()));
        sink.write(Record::Err("bad\nthing".into()));

        let out = output_text(&access.with(|e| e.drain_events()));
        // The command's own escape is gone...
        assert!(!out.contains("[2J"), "clear-screen survived: {out:?}");
        assert!(out.contains("cleared"));
        // ...the newline is collapsed to keep one diagnostic on one line...
        assert!(out.contains("bad thing"));
        // ...and our styling is applied around the sanitized text.
        assert!(out.contains("\x1b[31m"), "err not styled: {out:?}");
    }
```

Add to the test module's imports:

```rust
    use crate::sink::{Record, Sink};
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p bterm-core engine::tests::pane_sink_strips`
Expected: FAIL — `cannot find struct PaneSink`.

- [ ] **Step 3: Write minimal implementation**

Add to `crates/bterm-core/src/engine.rs` next to `EngineHost`:

```rust
/// Routes diagnostics to a pane, sanitized and styled.
///
/// Sanitizing is not optional: the text comes from a TS command and is
/// therefore page-controlled. Styling is applied *after* stripping, so our
/// colour survives and the command's cannot be injected.
struct PaneSink<A: EngineAccess> {
    access: A,
    pane: u32,
}

impl<A: EngineAccess> crate::sink::Sink for PaneSink<A> {
    fn write(&self, record: crate::sink::Record) {
        const RED: &str = "\x1b[31m";
        const RESET: &str = "\x1b[0m";
        let clean = crate::render::diagnostic_text(record.text());
        let line = match record {
            crate::sink::Record::Log(_) => format!("{clean}\n"),
            crate::sink::Record::Err(_) => format!("{RED}{clean}{RESET}\n"),
        };
        self.access.with(|e| e.emit_output(self.pane, &line));
    }
}
```

Then wire it in: in `execute_line` (line 466), build the sink and pass it to `make_ctx`:

```rust
    let sink: Rc<dyn crate::sink::Sink> =
        Rc::new(PaneSink { access: access.clone(), pane });
    let ctx = make_ctx(&access, pane, run_id, sink);
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p bterm-core engine::tests::pane_sink_strips`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/bterm-core/src/engine.rs
git commit -m "PaneSink: sanitize diagnostics, then style them

Closes an escape-injection hole. ctx.emit went to the pane unsanitized,
so a TS command could clear the screen -- the same bug class as the
rendered-help litter, on the one path that takes arbitrary page text."
```

---

## Task 5: `CliSink` — log to stdout, err to stderr

**Files:**
- Modify: `crates/bterm-cli/src/main.rs`

- [ ] **Step 1: Write the implementation**

There is no unit test here; the CLI is verified by running it. Add to
`crates/bterm-cli/src/main.rs`:

```rust
/// The native harness maps the channels onto real file descriptors, so
/// `bterm -c '…' 2>/dev/null` behaves the way a shell user expects.
struct CliSink;

impl bterm_core::sink::Sink for CliSink {
    fn write(&self, record: bterm_core::sink::Record) {
        match record {
            bterm_core::sink::Record::Log(s) => println!("{s}"),
            bterm_core::sink::Record::Err(s) => eprintln!("{s}"),
        }
    }
}
```

Then change the `ExecContext` at `crates/bterm-cli/src/main.rs:69` from:

```rust
    let ctx = ExecContext { host: host.clone(), width: terminal_width(), pane: 0, run_id: 0 };
```

to:

```rust
    let ctx = ExecContext {
        host: host.clone(),
        sink: Rc::new(CliSink),
        width: terminal_width(),
        pane: 0,
        run_id: 0,
    };
```

- [ ] **Step 2: Verify by running**

Run:
```bash
cargo run -q -p bterm-cli 2>/dev/null <<'EOF'
slow 1
EOF
```
Expected: the `tick 1/1` line still appears (it is a `Log`, so stdout).

Then confirm the split works — a command writing to `err` should vanish when
stderr is discarded, and this is worth checking again once Task 7 lands
`ctx.err` in the demos.

- [ ] **Step 3: Commit**

```bash
git add crates/bterm-cli/src/main.rs
git commit -m "CliSink: map log to stdout and err to stderr

The native harness gets real shell redirection for free, which is the
cheapest possible check that the channel split is meaningful."
```

---

## Task 6: `ctx.log` and `ctx.err` in TypeScript

**Files:**
- Modify: `crates/bterm-wasm/src/js_command.rs:50-68`
- Modify: `packages/browser-terminal/src/types.ts:48-53`

- [ ] **Step 1: Write the implementation**

In `crates/bterm-wasm/src/js_command.rs`, replace the single `emit` closure
with three. All three closures must stay alive until after the `await`, so
keep the existing `drop` placement and extend it:

```rust
            let log_sink = ctx.sink.clone();
            let log = Closure::<dyn Fn(String)>::new(move |line: String| {
                log_sink.write(Record::Log(line));
            });
            let err_sink = ctx.sink.clone();
            let err = Closure::<dyn Fn(String)>::new(move |line: String| {
                err_sink.write(Record::Err(line));
            });
            let emit_sink = ctx.sink.clone();
            // Retained alias: `emit` predates the channel split and is the
            // API every existing command uses.
            let emit = Closure::<dyn Fn(String)>::new(move |line: String| {
                emit_sink.write(Record::Log(line));
            });
            let _ = js_sys::Reflect::set(&ctx_obj, &JsValue::from_str("log"), log.as_ref());
            let _ = js_sys::Reflect::set(&ctx_obj, &JsValue::from_str("err"), err.as_ref());
            let _ = js_sys::Reflect::set(&ctx_obj, &JsValue::from_str("emit"), emit.as_ref());
```

and after the existing `.await`, replace `drop(emit);` with:

```rust
            drop(log);
            drop(err);
            drop(emit);
```

In `packages/browser-terminal/src/types.ts`, replace `CommandCtx`:

```ts
export interface CommandCtx {
  /** Fires when the pipeline is aborted (Ctrl-C / dispose). Pass to fetch(). */
  signal: AbortSignal;
  /** Channel 3 — progress and commentary. Never enters the pipe. */
  log(line: string): void;
  /** Channel 2 — warnings and diagnostics. Non-fatal; throw to abort. */
  err(line: string): void;
  /**
   * Alias for `log`, kept because it predates the channel split.
   * Prefer `log` in new code.
   */
  emit(line: string): void;
}
```

- [ ] **Step 2: Verify it compiles and the demo still runs**

Run: `just build`
Expected: no errors.

- [ ] **Step 3: Commit**

```bash
git add crates/bterm-wasm/src/js_command.rs packages/browser-terminal/src/types.ts
git commit -m "TS commands get ctx.log and ctx.err

emit stays as an alias for log -- it predates the split and every
existing command uses it."
```

---

## Task 7: `run()` resolves to `{ value, log, err }`

**Files:**
- Modify: `crates/bterm-core/src/engine.rs:514-536` (`eval_to_value`)
- Modify: `crates/bterm-wasm/src/lib.rs:303-331`
- Modify: `packages/browser-terminal/src/index.ts:211-217`, `types.ts`

- [ ] **Step 1: Change the core signature**

In `crates/bterm-core/src/engine.rs`, give `eval_to_value` a sink:

```rust
pub async fn eval_to_value<A: EngineAccess>(
    access: A,
    pane: u32,
    line: String,
    run_id: u64,
    sink: Rc<dyn crate::sink::Sink>,
) -> Result<Value, ShellError> {
    let parsed = parse(&line);
    if let Some(err) = parsed.errors.into_iter().next() {
        return Err(err);
    }
    let ctx = make_ctx(&access, pane, run_id, sink);
    let scope = scope_for_pane(&access, pane);
    let source = EngineCommands(access.clone());
    let (results, error) = eval_line(&parsed.line, &source, &ctx, &scope).await;
    if let Some(err) = error {
        return Err(err);
    }
    Ok(results
        .into_iter()
        .last()
        .map(PipelineData::into_value)
        .unwrap_or(Value::Null))
}
```

- [ ] **Step 2: Build the JS result object**

In `crates/bterm-wasm/src/lib.rs`, inside `run()`, create the capturing sink
before the abortable wrap and read it afterwards:

```rust
                let sink = Rc::new(bterm_core::sink::CollectingSink::new());
                let (fut, handle) = Abortable::wrap(eval_to_value(
                    WasmAccess,
                    pane,
                    line,
                    run_id,
                    sink.clone(),
                ));
                tasks::register(run_id, pane, handle, controller);
                let result = fut.await;
                tasks::finish(run_id);
                match result {
                    Ok(Ok(value)) => {
                        let out = js_sys::Object::new();
                        let _ = js_sys::Reflect::set(
                            &out,
                            &JsValue::from_str("value"),
                            &convert::value_to_js(&value),
                        );
                        let _ = js_sys::Reflect::set(
                            &out,
                            &JsValue::from_str("log"),
                            &string_array(&sink.log_lines()),
                        );
                        let _ = js_sys::Reflect::set(
                            &out,
                            &JsValue::from_str("err"),
                            &string_array(&sink.err_lines()),
                        );
                        Ok(out.into())
                    }
                    // …error arms unchanged…
                }
```

Add the helper to the same file:

```rust
fn string_array(lines: &[String]) -> js_sys::Array {
    lines.iter().map(|s| JsValue::from_str(s)).collect()
}
```

- [ ] **Step 3: Update the TypeScript types**

In `packages/browser-terminal/src/types.ts`:

```ts
/** What `run()` resolves to: the data channel plus both diagnostic channels. */
export interface RunResult {
  /** Channel 1 — the pipeline's final structured value. */
  value: Value;
  /** Channel 3 lines, in order. */
  log: string[];
  /** Channel 2 lines, in order. */
  err: string[];
}
```

In `packages/browser-terminal/src/index.ts`, export the type and change `run`:

```ts
  /**
   * Run a line programmatically in the active pane's session.
   *
   * Resolves with the final structured value **and** both diagnostic
   * channels, so a background call never writes on the user's terminal —
   * the caller decides what to surface.
   */
  run(line: string): Promise<RunResult> {
    if (this.disposed) {
      return Promise.reject(new Error('browser-terminal: instance is disposed'));
    }
    const pane = this.lastSnapshot?.active_pane ?? 0;
    return this.core.run(pane, line) as Promise<RunResult>;
  }
```

Add `RunResult` to the `export type { … }` block from `./types.js`.

- [ ] **Step 4: Build**

Run: `just build`
Expected: TypeScript errors at every `run()` call site — that is Task 8's work.

- [ ] **Step 5: Commit**

```bash
git add crates/ packages/browser-terminal/src/
git commit -m "run() resolves to { value, log, err }

A programmatic call no longer writes on whatever pane happens to be
active; the caller gets the diagnostics and decides."
```

---

## Task 8: Migrate every `run()` call site

**Files:**
- Modify: `packages/demo/src/main.ts:112-113`
- Modify: `packages/demo-react/src/App.tsx:174`
- Modify: `packages/demo-svelte/src/help.svelte.ts:28`
- Modify: `packages/demo/tests/smoke.spec.ts` (all `window.bt.run` assertions)
- Modify: `scripts/verify-site.mjs:80-90`
- Modify: `README.md:105-131` (API sketch)

- [ ] **Step 1: Update the library consumers**

`packages/demo/src/main.ts` — the help panels:

```ts
  help?.append(
    helpPanel('links --help', String((await bt.run('links --help')).value)),
    helpPanel('slow --help', String((await bt.run('slow --help')).value)),
  );
```

`packages/demo-react/src/App.tsx` — inside the help effect:

```tsx
    Promise.all(HELP_FOR.map((c) => bt.run(`${c} --help`))).then((results) => {
      if (live) setHelp(results.map((r) => ansiToHtml(String(r.value).trimEnd())));
    });
```

`packages/demo-svelte/src/help.svelte.ts` — inside `loadHelp`:

```ts
    commands.map(async (command) => ({
      command,
      html: ansiToHtml(String((await bt.run(`${command} --help`)).value).trimEnd()),
    })),
```

- [ ] **Step 2: Update the test suites**

In `packages/demo/tests/smoke.spec.ts`, every `window.bt.run(...)` used for
its value needs `.value`. Add this helper at the top of the file and route
assertions through it so the change is one line per call rather than a
parenthesis dance:

```ts
declare global {
  interface Window {
    bt: {
      run(line: string): Promise<{ value: unknown; log: string[]; err: string[] }>;
      // …existing members unchanged…
    };
  }
}

/** Run a line and return just the data channel. */
const value = (line: string) => window.bt.run(line).then((r) => r.value);
```

There are **20** `window.bt.run` call sites in that file. **5** of them are
rejection assertions of the form `.then(() => 'resolved?!', (e) => e.message)`
— those keep using `window.bt.run` directly, since they never touch `.value`.
The other **15** are used for their result and become `value("…")`. If your
count of changed sites is not 15, you have missed one or converted a
rejection case by mistake.

In `scripts/verify-site.mjs`, the probe becomes:

```js
    value = await page.evaluate((probe) => window.bt.run(probe).then((r) => r.value), demo.probe);
```

- [ ] **Step 3: Update the README**

In `README.md`, the API sketch line for `run` becomes:

```ts
run(line: string): Promise<{ value: Value; log: string[]; err: string[] }>
```

Add a sentence under it:

> Diagnostics written with `ctx.log` / `ctx.err` come back with the value
> rather than printing to the pane, so a programmatic call never writes on
> the user's terminal.

- [ ] **Step 4: Verify everything**

Run each and confirm:

```bash
cargo test --workspace
```
Expected: PASS.

```bash
just build && npm --prefix packages/demo run build && npm --prefix packages/demo-react run build && npm --prefix packages/demo-svelte run build
```
Expected: three clean builds, no TypeScript errors.

```bash
cd packages/demo && npx playwright test
```
Expected: 8 passed.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "Migrate run() call sites to the { value, log, err } shape"
```

---

## Task 9: Prove the channels are actually separated

The two properties worth a permanent test are security properties, not
ergonomics. Label them as such so nobody 'simplifies' them away later.

**Files:**
- Modify: `packages/demo/tests/smoke.spec.ts`

- [ ] **Step 1: Write the failing test**

```ts
test('SECURITY: diagnostics never enter the pipe and cannot inject escapes', async ({
  page,
}) => {
  await page.goto('/');
  await waitForTerminal(page);

  await page.evaluate(() => {
    window.bt.registerCommand({ name: 'noisy', summary: 'writes to every channel' }, (_a, _i, ctx) => {
      ctx.log('LOG-LINE');
      ctx.err('ERR-LINE');
      return [{ id: 1 }, { id: 2 }];
    });
  });

  // The pipe carries only the data channel: two rows, no diagnostic text.
  const piped = await page.evaluate(() => window.bt.run('noisy | length').then((r) => r.value));
  expect(piped).toBe(2);

  const asText = await page.evaluate(() =>
    window.bt.run('noisy | to json').then((r) => r.value),
  );
  expect(asText).not.toContain('LOG-LINE');
  expect(asText).not.toContain('ERR-LINE');

  // But the caller still receives them, on the right channels.
  const result = await page.evaluate(() => window.bt.run('noisy'));
  expect(result.log).toEqual(['LOG-LINE']);
  expect(result.err).toEqual(['ERR-LINE']);
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd packages/demo && npx playwright test -g SECURITY`
Expected: FAIL until Tasks 1-8 are complete; run it last.

- [ ] **Step 3: No implementation needed**

This test is a characterization of work already done. If it fails after
Task 8, something in the channel wiring is wrong — do not weaken the test.

- [ ] **Step 4: Run to verify it passes**

Run: `cd packages/demo && npx playwright test`
Expected: 9 passed.

- [ ] **Step 5: Commit**

```bash
git add packages/demo/tests/smoke.spec.ts
git commit -m "Test the channel separation as a security property

Diagnostics must never reach the pipe, and a page-controlled diagnostic
must never reach the terminal with escapes intact."
```

---

## Task 10: Demonstrate the split in a demo

**Files:**
- Modify: `packages/demo/src/main.ts` (the `slow` command)
- Modify: `packages/demo/index.html` (Try block)

- [ ] **Step 1: Use both channels in `slow`**

In `packages/demo/src/main.ts`, inside the `#region slow` block, change the
tick emission to use both channels so the styling difference is visible:

```ts
        if (i === seconds) {
          ctx.err(`finished late — ${seconds}s is a long time to wait`);
        }
        ctx.log(`tick ${i}/${seconds}`);
```

- [ ] **Step 2: Mention it on the page**

In `packages/demo/index.html`, add to the Try block after the `slow 5` line:

```
                   # log is plain, err is red
```

- [ ] **Step 3: Verify in the browser**

Start the demo, type `slow 2` into the pane, and confirm the tick lines are
plain while the final diagnostic renders red.

- [ ] **Step 4: Commit**

```bash
git add packages/demo/
git commit -m "Demo: show log and err rendering differently"
```

---

## Self-review notes

**Spec coverage.** Stage 1 of the spec's six covers: the `Record`/`Sink`
model (Task 1), sanitization (Tasks 2, 4), `ExecContext` carrying the sink
(Task 3), the three sink implementations — pane (Task 4), CLI (Task 5),
collecting/test (Tasks 1, 7) — `ctx.log`/`ctx.err` (Task 6), and the `run()`
shape change with its migration (Tasks 7, 8).

**Deliberately deferred within stage 1:**
- `Sink::ready()` — YAGNI until stage 6 gives it an await site.
- Text-vs-data stream tagging and the `PipelineData` collapse — stage 2.
- `help` emitting records instead of text — stage 3, once streams exist.
- Channel *numbers* — reserved by the spec, no code until the redirect spec.

**Open question surfaced while planning, worth answering during Task 7.**
`bt.run()` may not flush queued `PaneOutput` events, which would mean
emitted lines from programmatic runs currently appear at an arbitrary later
time rather than immediately. The capturing sink removes the symptom, but if
the flush gap is real it likely affects other events too and deserves its own
issue.
