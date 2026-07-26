# Progressive rendering & output buffering — design

Status: implemented. Sections below marked **As shipped** record where the
approved design changed during implementation; the surrounding prose is the
original reasoning, kept because it explains why.
Part of: the streaming line of work — combines what the
[channels/streaming spec](2026-07-23-output-channels-and-streaming-design.md)
called stage 5 (progressive rendering) and stage 6 (pane self-throttling),
plus a new command-facing output-buffering API.
Builds on: stage 1 (channels/sink), stage 2 (transport), stage 3 (streaming
commands) — all merged.

## Problem

Two gaps remain after stage 3.

1. **The data channel renders once, at the end.** `execute_line` collects the
   whole pipeline result, then calls `render()` once. So a live or long source
   only paints when it finishes (or a `head` cuts it off) — a `watch click`
   without a `head` shows nothing, ever. Streaming *works* but doesn't *feel*
   live.

2. **Commands can't control their text output cadence.** `ctx.log(line)` /
   `ctx.err(line)` write a whole line and flush per-write. There is no way to
   write a partial line, batch output and flush once, or drive an in-place
   progress bar. Diagnostics are also stripped of *all* escape sequences, so
   even `\r` can't reach the pane.

This design adds progressive rendering of the data channel, a command-facing
buffering API (byte / line / block modes + explicit flush + partial writes)
with a curated control-character allowlist, and the pane self-throttle that
keeps a fast source from freezing the tab.

## The output writer API

The two diagnostic channels stage 1 defined — `log` (channel 3, "stdout") and
`err` (channel 2, "stderr") — keep their names. This design does **not**
introduce a new `out`/`stdout` name (stage 1 deliberately rejected that
vocabulary). Instead `ctx.log` and `ctx.err` become **callable writer
objects**: still callable as the line sugar every existing command uses, now
also carrying `.write` / `.flush` / `.mode` for cadence control. (JS functions
are objects, so a function with attached methods is one value that is both.)

```ts
ctx.log(msg): void             // cooked message: sanitized, unbuffered, one entry
ctx.log.write(s: string): void // raw terminal bytes, buffered, NO implicit newline
ctx.log.flush(): void          // force buffered content to the pane now
ctx.log.mode(m: 'byte' | 'line' | 'block', opts?: { delimiter?: string }): void

ctx.err(line) / ctx.err.write / ctx.err.flush / ctx.err.mode   // stderr, identical
ctx.emit(line)                 // still an alias for ctx.log(line)
```

Existing commands are untouched — `ctx.log(line)` / `ctx.err(line)` still work
exactly as they do today. New commands reach for `ctx.log.write` + `flush` for
cadence control. Backward compatibility is why the writer is `ctx.log` rather
than a fresh `ctx.out`.

**As shipped: the call and the raw write are two different APIs, not two
spellings of one.** The approved draft specified `ctx.log(line)` as sugar for
`write(line + delimiter)` — buffered, sharing the writer's cadence and
delimiter. That was deliberately abandoned in stage 5b, because the two calls
carry different *kinds* of value:

- **`ctx.log('msg')` passes a message.** The shell owns the framing and the
  sanitizing: `render::diagnostic_text` strips every escape and collapses
  newlines to spaces, the record is written straight to the sink unbuffered,
  and it lands as exactly one clean single-line entry in `run().log`. A
  SECURITY test pins this — page-controlled text must not be able to fake
  extra lines in a caller's log viewer. Nothing is appended and the buffer's
  delimiter is never consulted; the pane supplies the line break at display
  time.
- **`ctx.log.write(s)` passes terminal bytes.** The command owns the framing;
  what makes it safe is the `render::writer_text` allowlist plus
  `OutputBuffer`.

Collapsing the two would have forced one of them to lose: either the line
call gives up its single-line guarantee, or the raw write gives up partial
output. Keeping them separate costs one sharp edge, which is the API's most
surprising behaviour and is documented on `ChannelWriter` in `types.ts`: only
`.write` / `.flush` / `.mode` touch the buffer, so a cooked call bypasses it
and goes out at once. A raw write still sitting in the buffer is therefore
overtaken — `ctx.log.write('a')` then `ctx.log('b')` shows `b` first, `a`
when the buffer drains. A command that needs ordering picks one API, or
`flush()`es before switching.

### Buffering modes

| mode | flushes when | for |
|------|--------------|-----|
| **line** (default) | the delimiter is written, or the command ends | ordinary log output |
| **byte** | every `write` | progress bars, spinners — immediate |
| **block** | `flush()`, buffer fills (~8 KB), or command ends | bulk output, one flush |

- **Line mode's delimiter is configurable, default `\n`.**
  `mode('line', { delimiter: '\r\n' })` for CRLF producers,
  `{ delimiter: '\0' }` for null-delimited framing (`find -print0` idiom), or
  any string. `byte`/`block` ignore the delimiter.
- Two invariants regardless of mode: **the buffer flushes when the command
  finishes** (nothing is silently lost), and **`flush()` always works** (in
  block mode it is the primary control).
- **As shipped**, the first of those is narrower than it reads: the
  finish-flush runs on the *normal* exit paths in `js_command.rs`, and does
  **not** run when a command is aborted, because `Abortable::poll` returns
  without ever resuming a suspended body. Ctrl-C on a hung command drops
  whatever it had buffered. That is fine for output, but it is why nothing
  safety-critical may rest on code running at the command boundary — see the
  SGR note below.

### Control-character allowlist on raw writes

`ctx.log.write` / `ctx.err.write` carry page-controlled text, so they are
sanitized — but with a **curated allowlist** rather than the strip-everything
pass used for auto-`Record` diagnostics. The unifying safety rule:

> **No allowed sequence moves the cursor upward or to an absolute position.**
> Nothing can move up, position the cursor absolutely, clear the screen, read
> the cursor back (that injects input), or touch OSC (title / clipboard /
> hyperlinks). The worst a malicious command can do is garble lines it opened
> itself — which it can already do with plain text.

**As shipped**, that rule is stated in terms of *upward* movement rather than
"confined to the current line", because `\n` is on the allowlist: a raw write
is not confined to one line, and a command can open new lines below itself.
What it cannot do is climb back to output it did not write — the prompt, or
an earlier command's rows — which is the property that actually matters.

| allowed | what | why safe |
|---|---|---|
| `\r` | CR → column 0 | current line only |
| `\b` | backspace | current line only |
| `\x1b[nC` / `\x1b[nD` | cursor forward / back | **horizontal only** |
| `\x1b[K`, `\x1b[0K/1K/2K` | erase to end / start / whole line | current line only |
| `\x1b[…m` | SGR styling (colour, bold, …) | non-spatial; leak-proofed below |
| `\n` | newline | *(as shipped)* opens a line **below**; cannot climb back |
| `\t` | tab | ordinary text a writer may emit |

**SGR is leak-proofed.** *As shipped, not by a boundary reset.* The draft
proposed force-appending `\x1b[0m` when a command ends. That was never built,
and would not have covered the case that matters: a command that sets a
colour — or `\x1b[8m` conceal, which would make every later command invisible
— and then *hangs* is stopped by Ctrl-C, and `Abortable::poll` returns without
resuming the suspended body, so nothing at the boundary runs. A guarantee that
evaporates in exactly the hostile case is not a guarantee.

What ships instead: `PaneSink` **reset-prefixes every record** it emits, raw
or cooked (`engine.rs`). Styling is bounded by the *next* record rather than
by the command's own exit, so it holds even when the command never exits. The
test is `every_pane_record_is_reset_prefixed_so_sgr_cannot_bleed`. Anything
auditing `writer_text`'s decision to allow SGR should look there, not for a
boundary reset.

Explicitly still stripped: `\x1b[A/B` (cursor up/down — cross-line),
`\x1b[row;colH` / `\x1b[nf` (absolute positioning), `\x1b[2J/3J` (clear
screen), `\x1b[6n` (cursor report — injects input), `\x1b[s/u` (save/restore
— escapes the line boundary when combined with moves), and all
OSC / DCS / APC / PM.

This is a real escalation in sanitizer complexity — from "strip every escape"
to "parse each escape, keep the allowlisted ones." It is security-critical and
gets adversarial tests (the muscle that caught the DCS/APC-body leak in
stage 1).

Deliberately out of scope: cursor up/down and absolute positioning (they are
the line between "rewrite my progress bar" and "overwrite the prompt or
another command's output"), so multi-line live dashboards are not supported;
and SGR leak beyond the forced reset.

## Progressive structured rendering

Today the terminal collector gathers the whole result and `execute_line`
renders it once. It becomes a **streaming renderer** for the data channel:

- **Probe.** Buffer incoming records until **N rows OR T ms after the first
  row**, whichever comes first. Defaults ~50 rows / ~150 ms, tunable and
  measured during implementation. The clock starts at the first row, so before
  any data there is nothing to paint (an empty table is meaningless) and after
  data starts the first paint is guaranteed within T.
- **Commit.** Compute column widths from the probe; paint the header and the
  probed rows.
- **Stream.** Each later row prints at the committed widths, truncating an
  over-wide cell with `…`. A late row carrying keys absent from the header
  gets a one-line note on channel 2 rather than silent loss.
- **Finite / fast pipelines are unchanged.** If the whole result arrives
  inside the probe window — every collected pipeline today — it renders
  exactly as now: byte-identical tables, no regression. This is the gate,
  like stage 2.
- **Non-record streams** (scalars, strings, `Rendered` help lines) print per
  item as they arrive; no probe.

### Why time-bounded, not row-bounded

A row-count-only probe hangs on a sporadic source (`watch click`, a row every
few seconds never reaches N, so nothing paints). The time bound is how
streaming-aligned output must work: `column -t` sidesteps the problem by being
batch-only (reads to EOF), which breaks on infinite input; nushell commits
widths after a bounded probe and truncates outliers. The time bound, with the
clock starting at the first row, guarantees first paint within T of the first
datum and never blocks indefinitely.

## Interleaving and throttling

Three channels now reach the pane concurrently and in real time, so **stdout
rows, stderr warnings, and log lines interleave as they happen** — like a real
terminal where stdout and stderr share one screen. Intended.

The **pane self-throttle** (stage 6): a fast source must not repaint per item.
The pane sink coalesces writes and flushes on a short interval / watermark, so
100k rows/sec self-throttle to display speed rather than spinning the thread.

This is the *engine's* pane throttle, separate from the *command's* buffering
mode:
- A command's `flush()` forces its bytes into the pipeline.
- The pane throttle governs how fast the pipeline paints.

They compose: `flush()` guarantees delivery; the throttle guarantees the tab
stays responsive. The `Sink` trait gains the `ready()` async hook deferred
from stage 1 — the final stage awaits it so a live producer self-throttles to
the pane instead of outrunning it.

## Architecture

| unit | change |
|---|---|
| `crates/bterm-core/src/sink.rs` | `Sink` gains `ready() -> LocalBoxFuture<()>` (pane sink throttles; collecting/test sinks return immediately). A new allowlist sanitizer for raw writer output, distinct from `diagnostic_text`. |
| `crates/bterm-core/src/render/` | streaming renderer: probe → commit widths → stream rows at fixed widths; the allowlist sanitizer lives near the existing `strip_escapes`. |
| `crates/bterm-core/src/engine.rs` | `execute_line` renders progressively as the data stream arrives, awaiting the pane sink's `ready()`; final render becomes "flush the probe / remaining". `PaneSink` coalesces + throttles, and reset-prefixes every record (the SGR containment mechanism). |
| `crates/bterm-wasm/src/js_command.rs` | `ctx.log` / `ctx.err` writer objects (callable + `.write`/`.flush`/`.mode`) backed by a per-run buffer; modes, delimiter, flush; wired to the sink. |
| `packages/browser-terminal/src/types.ts` | `CommandCtx`'s `log`/`err` become the `ChannelWriter` type (callable + `.write`/`.flush`/`.mode`), documenting the cooked/raw split and the interleaving hazard. |
| `packages/demo` | a progress-bar command (byte mode + `\r`) and a live-source render demo; browser tests. |

## Testing

- **Allowlist sanitizer** — adversarial tests: each allowed sequence passes;
  cursor up/down, absolute positioning, clear-screen, `\x1b[6n`, save/restore,
  OSC/DCS/APC all stripped; the stage-1 escape-injection cases still blocked.
  *As shipped*, the SGR-containment test lives with `PaneSink` instead of the
  sanitizer — every emitted record is reset-prefixed — since there is no
  boundary reset to test.
- **Buffering modes** — line flushes on the (configurable) delimiter and at
  command end; byte flushes per write; block flushes only on `flush()` / fill
  / end; a dropped-without-flush buffer still flushes at command end (nothing
  lost).
- **Progressive render** — probe commits widths and streams later rows at them
  (native, via a fake record stream); a slow source paints within T of its
  first row (not on EOF); a finite result inside the probe renders
  byte-identically to today (the gate).
- **Throttle** — a fast producer does not starve the executor; the pane sink's
  `ready()` applies backpressure (native test double counting paints).
- **Browser** — a progress-bar command rewrites in place via `\r`; a
  block-buffered command shows nothing until `flush()`; a live source paints
  rows progressively; existing 12 Playwright tests stay green.
- **Size** — wasm delta recorded (pre-stage-5: 456272 bytes).

## Delivery shape (for the plan)

Roughly, dependency-ordered:

1. Allowlist sanitizer in `render/` + adversarial tests (pure, standalone).
2. `Sink::ready()` + the per-command output buffer (modes, delimiter, flush,
   flush-at-end) in core, with a native test sink.
3. Progressive streaming renderer (probe → commit → stream) + the
   finite-pipeline gate.
4. `execute_line` renders progressively and awaits `ready()`; `PaneSink`
   coalesces + throttles.
5. `ctx.log`/`ctx.err` writer objects in the wasm layer + TS types.
6. Demo (progress bar + live render) and browser verification; size recorded.

## Risks

| risk | mitigation |
|---|---|
| Allowlist sanitizer lets a dangerous sequence through | One invariant ("current line only, or reset-bounded styling"); adversarial tests; the DCS/APC-leak review muscle |
| Progressive render regresses existing tables | Finite-inside-probe path is byte-identical; that gate must stay green |
| Throttle starves or spins | `ready()` backpressure bounded like the stage-2 driver; native paint-counting test double |
| Buffer loses output on early exit / error | Flush-at-command-end invariant, tested for the dropped-without-flush case |
| Borrow-across-await via the new async `ready()` | Same discipline as everywhere: `with_engine` closures never await; sink `ready()` awaits with no engine borrow held |
| wasm growth | Measured and recorded, as in prior stages |

## Known limitations, as shipped

**A failing command's buffered tail prints below its error, not above it.**
`JsCommand::run` drains at the stage boundary, but an early `?` skips that
drain, so the tail is emitted from `tasks::flush_buffers` at the run's end —
after `execute_line` has already rendered the error. Nothing is lost; the
order is wrong, and only on the error path in a pane. `run()` is unaffected,
since it collects both channels into arrays.

Draining on every exit path means wrapping the whole `JsCommand::run` body so
the drain survives a `?`. Declined deliberately: that reindent touches every
`RefCell` site in the file, each spelled `let tail = …;` rather than
`if let Some(t) = …borrow_mut()` precisely because the collapsed form holds a
borrow across a `sink.write` that re-enters JS and panics. Churning all of
them to reorder one cosmetic case is a bad trade.

**The pane is not an authentication surface.** Page-controlled data can paint
multi-line, column-0, prompt-shaped text. This predates the writer API —
`echo '"…\n…"' | from json` suffices, because `Value::Str` rendering keeps
newlines by design. The raw writer adds SGR, which makes such a forgery
byte-exact rather than merely shaped. Removing `\n` from the allowlist would
close neither hole (`\r` + `\x1b[K` + SGR reproduce `prompt_line()` exactly,
with no newline) and would break line mode, which passes its delimiter
through to the sanitizer. If prompt forgery ever needs addressing, it belongs
at the prompt — a reserved glyph, or moving the prompt out of the character
stream — not in the allowlist.
