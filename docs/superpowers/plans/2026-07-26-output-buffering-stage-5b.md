# Output Buffering API (Stage 5b) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let a command control its own output cadence — partial writes, byte/line/block buffering, an explicit `flush()` — so progress bars and spinners work.

**Architecture:** A new allowlist sanitizer permits only control sequences confined to the current line (plus reset-bounded SGR colour), distinct from the strip-everything pass used for auto-sanitized diagnostics. `Record` gains a *raw* variant carrying pre-sanitized text that sinks emit verbatim (no appended newline, no styling wrapper), leaving all 11 existing `Record::log`/`err` call sites untouched. A per-command `OutputBuffer` implements the three modes; `ctx.log`/`ctx.err` become callable JS objects with `.write`/`.flush`/`.mode` attached.

**Tech Stack:** Rust (`bterm-core` — no wasm deps, no `futures` crate), `bterm-wasm`, TypeScript, Playwright.

**Spec:** [docs/superpowers/specs/2026-07-25-progressive-output-and-buffering-design.md](../specs/2026-07-25-progressive-output-and-buffering-design.md). Stage 5a (progressive rendering) is merged; this is the other half.

---

## Context an engineer needs

**The security history.** Diagnostics are page-controlled text. Stage 1 made `Record` sanitize on *construction* (`crates/bterm-core/src/sink.rs`, `Record::new` calls `render::diagnostic_text`) after a security test caught `bt.run()` returning raw escape sequences to a caller. That property must not regress: **every existing `Record::log`/`Record::err` call site keeps strip-everything sanitizing.** This plan adds a second, *narrower* path rather than loosening the existing one.

**Why a raw variant is needed.** Three things in the current path make partial writes impossible:
1. `Record` strips `\r` (via `cell_text` → `strip_escapes(s, false)`), and a progress bar needs `\r` to rewrite its line.
2. `PaneSink::write` (`crates/bterm-core/src/engine.rs`) appends `\n` to every record and wraps `Err` in `RED…RESET`. A partial write must not get a newline, and re-emitting colour per write would fight a command's own SGR.
3. `CliSink::write` (`crates/bterm-cli/src/main.rs`) uses `println!`/`eprintln!` — again a forced newline.

**The allowlist rule** (from the spec): every permitted sequence is confined to the current line, or is non-spatial styling force-reset at the command boundary. Permitted: `\r`, `\b`, `\x1b[nC`/`\x1b[nD` (horizontal cursor), `\x1b[K`/`0K`/`1K`/`2K` (erase-line), `\x1b[…m` (SGR). Stripped: cursor up/down (`A`/`B`), absolute positioning (`H`/`f`), clear-screen (`2J`/`3J`), cursor report (`\x1b[6n` — *injects input*), save/restore (`s`/`u`), and all OSC/DCS/APC/PM/SOS.

**Existing sanitizer** to fork from: `strip_escapes(s, keep_lines)` in `crates/bterm-core/src/render/mod.rs` — read it first. It already parses CSI (`ESC [` params, final byte `0x40..=0x7e`) and string-type escapes (OSC/DCS/APC/PM/SOS bodies to BEL or ST). The allowlist version reuses that parsing shape but *keeps* matching sequences instead of dropping all of them.

**Clippy** denies `unwrap_used` on `bterm-core`. Comments explain WHY. No borrow across `.await`; `with` closures never call JS.

---

## File Structure

| File | Responsibility |
|---|---|
| `crates/bterm-core/src/render/mod.rs` | **Modify.** Add `pub fn writer_text(s: &str) -> String` — the allowlist sanitizer — beside `diagnostic_text`. |
| `crates/bterm-core/src/sink.rs` | **Modify.** `Record::raw_log`/`raw_err` + `is_raw()`; raw text goes through `writer_text`. |
| `crates/bterm-core/src/engine.rs` | **Modify.** `PaneSink::write` emits raw records verbatim (no `\n`, no colour wrapper). |
| `crates/bterm-cli/src/main.rs` | **Modify.** `CliSink::write` uses `print!`/`eprint!` + flush for raw records. |
| `crates/bterm-core/src/outbuf.rs` | **Create.** `OutputBuffer`: modes, delimiter, `write`/`flush`/`finish`. Pure; no sink, no JS. |
| `crates/bterm-wasm/src/js_command.rs` | **Modify.** `ctx.log`/`ctx.err` become callable objects with `.write`/`.flush`/`.mode`; flush-at-command-end; SGR reset at boundary. |
| `packages/browser-terminal/src/types.ts` | **Modify.** `CommandCtx` writer types. |
| `packages/demo/src/main.ts`, `index.html`, `tests/smoke.spec.ts` | **Modify.** A progress-bar command + browser proof. |

---

## Task 1: The allowlist sanitizer

**Files:**
- Modify: `crates/bterm-core/src/render/mod.rs`

- [ ] **Step 1: Write the failing tests**

Add to the `#[cfg(test)] mod tests` block at the bottom of `crates/bterm-core/src/render/mod.rs`:

```rust
    #[test]
    fn writer_text_keeps_only_current_line_sequences() {
        // Allowed: everything here can only affect the line being written.
        assert_eq!(writer_text("50%\r"), "50%\r");
        assert_eq!(writer_text("ab\x08"), "ab\x08");
        assert_eq!(writer_text("\x1b[5Cx"), "\x1b[5Cx");
        assert_eq!(writer_text("\x1b[3Dx"), "\x1b[3Dx");
        assert_eq!(writer_text("a\x1b[K"), "a\x1b[K");
        assert_eq!(writer_text("a\x1b[2K"), "a\x1b[2K");
        assert_eq!(writer_text("\x1b[31mred\x1b[0m"), "\x1b[31mred\x1b[0m");
    }

    #[test]
    fn writer_text_strips_everything_that_escapes_the_line() {
        // Cursor up/down would overwrite the prompt or another command's
        // output; absolute positioning goes anywhere on screen.
        assert_eq!(writer_text("\x1b[Aup"), "up");
        assert_eq!(writer_text("\x1b[2Bdown"), "down");
        assert_eq!(writer_text("\x1b[3;4Hxy"), "xy");
        assert_eq!(writer_text("\x1b[10fxy"), "xy");
        // Clearing the screen, and the cursor report -- which injects text
        // into the INPUT stream, not the output.
        assert_eq!(writer_text("\x1b[2Jgone"), "gone");
        assert_eq!(writer_text("\x1b[6nprobe"), "probe");
        // Save/restore escapes the line boundary when paired with a move.
        assert_eq!(writer_text("\x1b[sx\x1b[u"), "x");
        // String-type escapes: title-setting, clipboard, Sixel payloads.
        assert_eq!(writer_text("\x1b]0;title\x07after"), "after");
        assert_eq!(writer_text("\x1bPq#0;2;0;0#0~~@@\x1b\\tail"), "tail");
        assert_eq!(writer_text("\x1b_apc\x1b\\z"), "z");
        // A lone ESC, and a two-char escape.
        assert_eq!(writer_text("a\x1b"), "a");
        assert_eq!(writer_text("a\x1b7b"), "ab");
    }

    #[test]
    fn writer_text_keeps_newlines_but_drops_other_controls() {
        // A writer may legitimately emit newlines (line mode appends one);
        // other C0 controls are not on the allowlist.
        assert_eq!(writer_text("a\nb"), "a\nb");
        assert_eq!(writer_text("a\x07b"), "ab");
        assert_eq!(writer_text("a\x00b"), "ab");
    }
```

- [ ] **Step 2: Run, verify it fails**

Run: `cargo test -p bterm-core render::tests::writer_text`
Expected: compile error, `cannot find function writer_text`.

- [ ] **Step 3: Implement**

Add to `crates/bterm-core/src/render/mod.rs`, next to `diagnostic_text`:

```rust
/// Display text for a command's *raw* write (`ctx.log.write`), which is
/// allowed a narrow set of control sequences that `diagnostic_text` strips.
///
/// The rule that makes this safe: **every permitted sequence is confined to
/// the current line, or is non-spatial styling.** Nothing here can move to
/// another line, position the cursor absolutely, clear the screen, read the
/// cursor back (that injects into the input stream), or touch OSC. So the
/// worst a hostile command can do is garble the line it is already writing —
/// which it can do with plain text anyway.
///
/// SGR is permitted because it cannot move anything; the writer force-emits
/// a reset at the command boundary so colour cannot leak into the prompt.
pub fn writer_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            match chars.peek() {
                Some('[') => {
                    chars.next();
                    // Collect params up to the final byte, then decide.
                    let mut params = String::new();
                    let mut final_byte = None;
                    for c2 in chars.by_ref() {
                        if ('\x40'..='\x7e').contains(&c2) {
                            final_byte = Some(c2);
                            break;
                        }
                        params.push(c2);
                    }
                    let Some(fin) = final_byte else { continue };
                    // Horizontal moves, erase-line, and styling only. `K`
                    // erases within the line; `C`/`D` cannot leave it; `m`
                    // is not spatial at all.
                    let keep = matches!(fin, 'C' | 'D' | 'K' | 'm')
                        && params.chars().all(|p| p.is_ascii_digit() || p == ';');
                    if keep {
                        out.push('\x1b');
                        out.push('[');
                        out.push_str(&params);
                        out.push(fin);
                    }
                }
                // String-type escapes (OSC/DCS/APC/PM/SOS): swallow the whole
                // body, exactly as `strip_escapes` does — a partial body left
                // behind is visible litter.
                Some(']') | Some('P') | Some('_') | Some('^') | Some('X') => {
                    chars.next();
                    while let Some(c2) = chars.next() {
                        if c2 == '\x07' {
                            break;
                        }
                        if c2 == '\x1b' {
                            chars.next(); // the `\` of ST
                            break;
                        }
                    }
                }
                Some(_) => {
                    chars.next();
                }
                None => {}
            }
            continue;
        }
        // `\r` and `\b` rewrite within the line; `\n` and `\t` are ordinary
        // text a writer may emit. Every other C0 control is dropped.
        if matches!(c, '\r' | '\n' | '\t' | '\x08') || !c.is_control() {
            out.push(c);
        }
    }
    out
}
```

**Note on `\x1b[6n`:** the final byte is `n`, which is not in the keep set, so it is stripped by the same rule that drops `A`/`B`/`H` — no special case needed. Confirm the test covering it passes.

**Note on the digit guard:** `params.chars().all(...)` rejects private-mode sequences like `\x1b[?25l` (the `?` is not a digit or `;`), which is correct — hiding the cursor is not on the allowlist.

- [ ] **Step 4: Run, verify pass**

Run: `cargo test -p bterm-core render::`
Expected: the 3 new tests pass and every existing render test still passes.

Run: `cargo test --workspace` → 208 + 3 = 211.
Run: `cargo clippy -p bterm-core --all-targets` → clean.

- [ ] **Step 5: Commit**

```bash
git add crates/bterm-core/src/render/mod.rs
git commit -m "Add writer_text: the allowlist sanitizer for raw writes

diagnostic_text strips every escape, which is right for auto-sanitized
diagnostics but makes a progress bar impossible -- it needs \\r. This
permits only sequences confined to the current line (\\r, \\b, horizontal
cursor, erase-line) plus non-spatial SGR, and strips cursor up/down,
absolute positioning, clear-screen, the cursor report (which injects into
the input stream), and every string-type escape."
```

---

## Task 2: A raw `Record` variant

**Files:**
- Modify: `crates/bterm-core/src/sink.rs`

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)] mod tests` block in `crates/bterm-core/src/sink.rs`:

```rust
    #[test]
    fn a_raw_record_keeps_line_local_control_but_not_screen_control() {
        // Raw writes carry a progress bar's \r; the auto-sanitized path
        // still strips everything.
        let raw = Record::raw_log("50%\r");
        assert_eq!(raw.text(), "50%\r");
        assert!(raw.is_raw());

        let cooked = Record::log("50%\r");
        assert_eq!(cooked.text(), "50% ", "the existing path is unchanged");
        assert!(!cooked.is_raw());

        // Even raw text cannot clear the screen.
        assert_eq!(Record::raw_err("\x1b[2Jx").text(), "x");
    }
```

Check the exact expectation for `Record::log("50%\r")` first — `diagnostic_text` maps `\r` to a space (`strip_escapes(s, false)`), so `"50% "` is expected. **Run the assertion and match reality**; if it differs, fix the test to the real current behaviour (this arm is characterizing the existing path, not changing it).

- [ ] **Step 2: Run, verify it fails**

Run: `cargo test -p bterm-core sink::tests::a_raw_record`
Expected: `no function or associated item named raw_log`.

- [ ] **Step 3: Implement**

In `crates/bterm-core/src/sink.rs`, add a `raw` flag to `Record` and the two constructors:

```rust
pub struct Record {
    channel: Channel,
    text: String,
    /// Written through `ctx.log.write` rather than `ctx.log(line)`: the text
    /// went through the *allowlist* sanitizer, and a sink must emit it
    /// verbatim — no appended newline, no styling wrapper — so partial
    /// writes and in-place rewrites work.
    raw: bool,
}
```

Update `Record::new` to set `raw: false`, and add:

```rust
    /// A raw write to the log channel: allowlist-sanitized, emitted verbatim.
    pub fn raw_log(text: impl AsRef<str>) -> Self {
        Self::raw(Channel::Log, text)
    }

    /// A raw write to the err channel: allowlist-sanitized, emitted verbatim.
    pub fn raw_err(text: impl AsRef<str>) -> Self {
        Self::raw(Channel::Err, text)
    }

    fn raw(channel: Channel, text: impl AsRef<str>) -> Self {
        Record {
            channel,
            text: crate::render::writer_text(text.as_ref()),
            raw: true,
        }
    }

    /// Whether a sink must emit this verbatim (no newline, no styling).
    pub fn is_raw(&self) -> bool {
        self.raw
    }
```

Keep `#[derive(Clone, Debug, PartialEq, Eq)]` working (add `raw` to it implicitly — it derives over all fields).

**Do not change `Record::log`/`Record::err` or `Record::new`'s sanitizing.** Every existing call site must behave exactly as before.

- [ ] **Step 4: Run, verify pass**

Run: `cargo test -p bterm-core sink::` → passes.
Run: `cargo test --workspace` → 212. **Every existing test must still pass** — the 11 `Record::log`/`err` sites are untouched.
Run: `cargo clippy --workspace --all-targets` → clean.

- [ ] **Step 5: Commit**

```bash
git add crates/bterm-core/src/sink.rs
git commit -m "Record gains a raw variant for writer output

Raw text goes through the allowlist sanitizer and is flagged so sinks emit
it verbatim. The existing log/err constructors keep strip-everything
sanitizing -- that property came from a security test and does not move."
```

---

## Task 3: Sinks emit raw records verbatim

**Files:**
- Modify: `crates/bterm-core/src/engine.rs` (`PaneSink::write`)
- Modify: `crates/bterm-cli/src/main.rs` (`CliSink::write`)

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)] mod tests` block in `crates/bterm-core/src/engine.rs` (it has `engine()`, `active_pane()`, `output_text()` helpers):

```rust
    #[test]
    fn the_pane_emits_a_raw_record_verbatim() {
        // A partial write must not gain a newline, and must not be wrapped
        // in the err channel's colour -- a progress bar owns its own line.
        let access = engine();
        let pane = active_pane(&access);
        let sink = PaneSink { access: access.clone(), pane };
        sink.write(crate::sink::Record::raw_log("50%\r"));
        let out = output_text(&access.with(|e| e.drain_events()));
        assert!(out.contains("50%\r"), "raw text altered: {out:?}");
        assert!(!out.contains("50%\r\n"), "newline appended: {out:?}");

        let access2 = engine();
        let pane2 = active_pane(&access2);
        let sink2 = PaneSink { access: access2.clone(), pane: pane2 };
        sink2.write(crate::sink::Record::raw_err("oops"));
        let out2 = output_text(&access2.with(|e| e.drain_events()));
        assert!(!out2.contains("\x1b[31m"), "raw err was colour-wrapped: {out2:?}");
    }
```

**Already verified — no action needed:** `emit_output` CRLF-converts via `crlf(text)`, which is `text.replace("\r\n", "\n").replace('\n', "\r\n")` (engine.rs:372). It only rewrites `\n`; a **bare `\r` passes through untouched**, which is exactly what a progress bar needs. The assertion above is therefore correct as written.

- [ ] **Step 2: Run, verify it fails**

Run: `cargo test -p bterm-core engine::tests::the_pane_emits_a_raw_record`
Expected: FAIL — the current `PaneSink::write` appends `\n` and wraps `Err`.

- [ ] **Step 3: Implement**

In `crates/bterm-core/src/engine.rs`, `PaneSink::write` becomes:

```rust
    fn write(&self, record: crate::sink::Record) {
        const RED: &str = "\x1b[31m";
        const RESET: &str = "\x1b[0m";
        let clean = record.text();
        // A raw write owns its own formatting: no appended newline (a
        // partial write must stay partial) and no colour wrapper (the
        // command may be emitting its own SGR, and re-wrapping per write
        // would fight it).
        let line = if record.is_raw() {
            clean.to_string()
        } else {
            match record.channel() {
                crate::sink::Channel::Log => format!("{clean}\n"),
                crate::sink::Channel::Err => format!("{RED}{clean}{RESET}\n"),
            }
        };
        self.access.with(|e| e.emit_output(self.pane, &line));
        self.access.events_ready();
    }
```

In `crates/bterm-cli/src/main.rs`, `CliSink::write`:

```rust
    fn write(&self, record: bterm_core::sink::Record) {
        use std::io::Write;
        if record.is_raw() {
            // No newline: a partial write must stay partial. Flush so a
            // progress bar appears as it is written rather than sitting in
            // stdout's line buffer.
            match record.channel() {
                bterm_core::sink::Channel::Log => {
                    print!("{}", record.text());
                    let _ = std::io::stdout().flush();
                }
                bterm_core::sink::Channel::Err => {
                    eprint!("{}", record.text());
                    let _ = std::io::stderr().flush();
                }
            }
            return;
        }
        match record.channel() {
            bterm_core::sink::Channel::Log => println!("{}", record.text()),
            bterm_core::sink::Channel::Err => eprintln!("{}", record.text()),
        }
    }
```

`CollectingSink` needs no change — it stores whole `Record`s, so `run()`'s `log`/`err` arrays keep working (a raw write appears as its own entry).

- [ ] **Step 4: Run, verify pass**

Run: `cargo test --workspace` → 213 (212 + 1). Existing sink/pane tests must still pass.
Run: `cargo clippy --workspace --all-targets` → clean.

- [ ] **Step 5: Commit**

```bash
git add crates/bterm-core/src/engine.rs crates/bterm-cli/src/main.rs
git commit -m "Sinks emit raw records verbatim

A raw write owns its formatting: no appended newline, no colour wrapper.
The CLI uses print!/eprint! plus an explicit flush so a progress bar is
visible as it is written rather than sitting in the line buffer."
```

---

## Task 4: `OutputBuffer` — the three modes

**Files:**
- Create: `crates/bterm-core/src/outbuf.rs`
- Modify: `crates/bterm-core/src/lib.rs`

- [ ] **Step 1: Register the module first**

Add `pub mod outbuf;` to `crates/bterm-core/src/lib.rs`, alphabetically — after `pub mod mux;`, before `pub mod parse;`. Do this before the failing test, or cargo reports "0 tests" rather than a compile error.

- [ ] **Step 2: Write the failing tests**

Create `crates/bterm-core/src/outbuf.rs` with only the test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_mode_flushes_on_the_delimiter() {
        let mut b = OutputBuffer::new();
        assert_eq!(b.write("no newline yet"), None);
        assert_eq!(b.write(" then\none"), Some("no newline yet then\n".to_string()));
        // The remainder stays buffered until the next delimiter or the end.
        assert_eq!(b.finish(), Some("one".to_string()));
    }

    #[test]
    fn the_line_delimiter_is_configurable() {
        // Null-delimited framing, the `find -print0` idiom.
        let mut b = OutputBuffer::new();
        b.set_mode(Mode::Line, Some("\0".to_string()));
        assert_eq!(b.write("a\nstill buffered"), None, "\\n is not the delimiter now");
        assert_eq!(b.write("\0rest"), Some("a\nstill buffered\0".to_string()));
    }

    #[test]
    fn byte_mode_flushes_every_write() {
        let mut b = OutputBuffer::new();
        b.set_mode(Mode::Byte, None);
        assert_eq!(b.write("5"), Some("5".to_string()));
        assert_eq!(b.write("0%\r"), Some("0%\r".to_string()));
        assert_eq!(b.finish(), None, "nothing left buffered");
    }

    #[test]
    fn block_mode_holds_until_flushed() {
        let mut b = OutputBuffer::new();
        b.set_mode(Mode::Block, None);
        assert_eq!(b.write("a\nb\nc\n"), None, "newlines do not flush in block mode");
        assert_eq!(b.flush(), Some("a\nb\nc\n".to_string()));
        assert_eq!(b.flush(), None, "nothing left to flush");
    }

    #[test]
    fn block_mode_flushes_when_the_buffer_fills() {
        let mut b = OutputBuffer::new();
        b.set_mode(Mode::Block, None);
        let big = "x".repeat(BLOCK_LIMIT + 10);
        let flushed = b.write(&big).expect("a full buffer flushes itself");
        assert_eq!(flushed.len(), big.len(), "everything buffered so far is flushed");
    }

    #[test]
    fn nothing_is_lost_when_a_command_ends_without_flushing() {
        // The invariant: whatever mode, a command that just returns still
        // gets its buffered output.
        for mode in [Mode::Line, Mode::Byte, Mode::Block] {
            let mut b = OutputBuffer::new();
            b.set_mode(mode, None);
            b.write("tail with no delimiter");
            assert_eq!(
                b.finish(),
                if mode == Mode::Byte { None } else { Some("tail with no delimiter".to_string()) },
                "mode {mode:?} lost its tail"
            );
        }
    }
}
```

- [ ] **Step 3: Run, verify it fails**

Run: `cargo test -p bterm-core outbuf::`
Expected: compile error, `cannot find type OutputBuffer`.

- [ ] **Step 4: Implement**

Above the test module in `crates/bterm-core/src/outbuf.rs`:

```rust
//! Per-command output buffering.
//!
//! A command controls its own cadence: `line` (the stdio default) flushes on
//! a delimiter, `byte` flushes every write so a progress bar appears as it is
//! drawn, and `block` holds everything until `flush()`. Two invariants hold
//! in every mode: `flush()` always works, and whatever is buffered when the
//! command ends is emitted rather than lost.
//!
//! Pure: it returns the text to emit and knows nothing about sinks or JS.

/// How much `block` mode holds before flushing itself, so a runaway command
/// cannot buffer without bound.
pub const BLOCK_LIMIT: usize = 8 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Flush when the delimiter is written (default, delimiter `\n`).
    Line,
    /// Flush on every write.
    Byte,
    /// Flush only on `flush()`, at `BLOCK_LIMIT`, or at the end.
    Block,
}

pub struct OutputBuffer {
    mode: Mode,
    delimiter: String,
    buf: String,
}

impl Default for OutputBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl OutputBuffer {
    pub fn new() -> Self {
        OutputBuffer { mode: Mode::Line, delimiter: "\n".to_string(), buf: String::new() }
    }

    /// Switch modes. `delimiter` applies to `Line` only; `None` keeps the
    /// current one (default `\n`).
    pub fn set_mode(&mut self, mode: Mode, delimiter: Option<String>) {
        self.mode = mode;
        if let Some(d) = delimiter {
            if !d.is_empty() {
                self.delimiter = d;
            }
        }
    }

    /// Buffer `text`, returning whatever should be emitted now.
    pub fn write(&mut self, text: &str) -> Option<String> {
        self.buf.push_str(text);
        match self.mode {
            Mode::Byte => self.take(),
            Mode::Line => {
                // Flush through the LAST delimiter, keeping any partial tail
                // buffered -- that is what makes a trailing prompt fragment
                // stay put until its line completes.
                match self.buf.rfind(&self.delimiter) {
                    Some(idx) => {
                        let split = idx + self.delimiter.len();
                        let rest = self.buf.split_off(split);
                        let out = std::mem::replace(&mut self.buf, rest);
                        Some(out)
                    }
                    None => None,
                }
            }
            Mode::Block => {
                if self.buf.len() >= BLOCK_LIMIT {
                    self.take()
                } else {
                    None
                }
            }
        }
    }

    /// Emit everything buffered now, whatever the mode.
    pub fn flush(&mut self) -> Option<String> {
        self.take()
    }

    /// The command ended: emit anything still buffered so nothing is lost.
    pub fn finish(&mut self) -> Option<String> {
        self.take()
    }

    fn take(&mut self) -> Option<String> {
        if self.buf.is_empty() {
            None
        } else {
            Some(std::mem::take(&mut self.buf))
        }
    }
}
```

- [ ] **Step 5: Run, verify pass**

Run: `cargo test -p bterm-core outbuf::` → 6 passed.
Run: `cargo test --workspace` → 219 (213 + 6).
Run: `cargo clippy -p bterm-core --all-targets` → clean.

- [ ] **Step 6: Commit**

```bash
git add crates/bterm-core/src/outbuf.rs crates/bterm-core/src/lib.rs
git commit -m "Add OutputBuffer: byte, line, and block modes

Line mode flushes through the last delimiter (configurable, default \\n)
so a partial tail stays buffered; byte flushes per write for progress
bars; block holds until flush() or BLOCK_LIMIT. Whatever the mode, the
buffer is emitted when the command ends -- nothing is silently lost."
```

---

## Task 5: `ctx.log` / `ctx.err` as callable writer objects

**Files:**
- Modify: `crates/bterm-wasm/src/js_command.rs`
- Modify: `packages/browser-terminal/src/types.ts`

- [ ] **Step 1: Read the current closure setup**

In `crates/bterm-wasm/src/js_command.rs`, `ctx.log`/`ctx.err`/`ctx.emit` are three `Closure::<dyn Fn(String)>` values written into `ctx_obj` via `js_sys::Reflect::set`, dropped after the awaited call. Read that block before editing — you are replacing each single closure with a callable object carrying methods.

- [ ] **Step 2: Implement the writer objects**

For each channel, build a JS *function* (the existing line-writing behaviour) and attach `.write`, `.flush`, `.mode` to it. In JS a function is an object, so `ctx.log('x')` and `ctx.log.write('x')` coexist.

Per channel you need an `Rc<RefCell<OutputBuffer>>` shared by the four closures. Sketch for the log channel — mirror it for err:

```rust
            use bterm_core::outbuf::{Mode, OutputBuffer};
            use std::cell::RefCell;

            let log_buf = Rc::new(RefCell::new(OutputBuffer::new()));

            // ctx.log(line) -- unchanged sugar: append the delimiter and let
            // the buffer decide when it goes out.
            let sink_a = ctx.sink.clone();
            let buf_a = log_buf.clone();
            let log_call = Closure::<dyn Fn(String)>::new(move |line: String| {
                let out = {
                    let mut b = buf_a.borrow_mut();
                    let d = b.delimiter().to_string();
                    b.write(&format!("{line}{d}"))
                };
                if let Some(text) = out {
                    sink_a.write(bterm_core::sink::Record::raw_log(text));
                }
            });

            // ctx.log.write(s) -- partial, no delimiter appended.
            let sink_b = ctx.sink.clone();
            let buf_b = log_buf.clone();
            let log_write = Closure::<dyn Fn(String)>::new(move |s: String| {
                let out = buf_b.borrow_mut().write(&s);
                if let Some(text) = out {
                    sink_b.write(bterm_core::sink::Record::raw_log(text));
                }
            });

            // ctx.log.flush()
            let sink_c = ctx.sink.clone();
            let buf_c = log_buf.clone();
            let log_flush = Closure::<dyn Fn()>::new(move || {
                let out = buf_c.borrow_mut().flush();
                if let Some(text) = out {
                    sink_c.write(bterm_core::sink::Record::raw_log(text));
                }
            });

            // ctx.log.mode(m, opts?)
            let buf_d = log_buf.clone();
            let log_mode = Closure::<dyn Fn(String, JsValue)>::new(move |m: String, opts: JsValue| {
                let mode = match m.as_str() {
                    "byte" => Mode::Byte,
                    "block" => Mode::Block,
                    _ => Mode::Line,
                };
                let delim = js_sys::Reflect::get(&opts, &JsValue::from_str("delimiter"))
                    .ok()
                    .and_then(|v| v.as_string());
                buf_d.borrow_mut().set_mode(mode, delim);
            });

            let log_fn = log_call.as_ref().clone();
            let _ = js_sys::Reflect::set(&log_fn, &JsValue::from_str("write"), log_write.as_ref());
            let _ = js_sys::Reflect::set(&log_fn, &JsValue::from_str("flush"), log_flush.as_ref());
            let _ = js_sys::Reflect::set(&log_fn, &JsValue::from_str("mode"), log_mode.as_ref());
            let _ = js_sys::Reflect::set(&ctx_obj, &JsValue::from_str("log"), &log_fn);
```

**Borrow care:** each closure takes `borrow_mut()`, computes the text, and drops the borrow *before* calling `sink.write` — note the `let out = { … };` block above. A sink write can re-enter JS, and a live `RefCell` borrow across that would panic. Keep that shape.

`OutputBuffer` needs a `delimiter()` accessor for the sugar path — add it in `crates/bterm-core/src/outbuf.rs`:

```rust
    /// The current line delimiter, appended by the `ctx.log(line)` sugar.
    pub fn delimiter(&self) -> &str {
        &self.delimiter
    }
```

`ctx.emit` stays an alias for the log *call* form: set it to the same `log_fn`.

**Flush at the command boundary, plus the SGR reset.** After the command's future resolves (both the streaming-generator branch and the collecting branch — find the `drop(log); drop(err); drop(emit);` sites from the prior stage), flush both buffers and emit a reset if anything raw was written:

```rust
            // Nothing buffered is lost, whatever mode the command chose; and
            // a command's SGR cannot leak into the prompt.
            let tail_log = log_buf.borrow_mut().finish();
            if let Some(text) = tail_log {
                ctx.sink.write(bterm_core::sink::Record::raw_log(text));
            }
            let tail_err = err_buf.borrow_mut().finish();
            if let Some(text) = tail_err {
                ctx.sink.write(bterm_core::sink::Record::raw_err(text));
            }
            ctx.sink.write(bterm_core::sink::Record::raw_log("\x1b[0m"));
```

Keep every closure alive until after these flushes (extend the existing `drop(...)` calls to the end).

- [ ] **Step 3: TypeScript types**

In `packages/browser-terminal/src/types.ts`, replace the `log`/`err` members of `CommandCtx`:

```ts
/** A diagnostic channel: callable for whole lines, with buffering control. */
export interface ChannelWriter {
  /** Write a whole line — the delimiter is appended for you. */
  (line: string): void;
  /** Write without appending a delimiter: partial lines, progress bars. */
  write(s: string): void;
  /** Emit everything buffered now. Always works, in every mode. */
  flush(): void;
  /**
   * `'line'` (default) flushes on the delimiter, `'byte'` on every write,
   * `'block'` only on `flush()` or when the buffer fills.
   */
  mode(m: 'byte' | 'line' | 'block', opts?: { delimiter?: string }): void;
}

export interface CommandCtx {
  /** Fires on Ctrl-C, dispose, or a downstream `head` closing the stream. */
  signal: AbortSignal;
  /** Channel 3 — progress and commentary. Never enters the pipe. */
  log: ChannelWriter;
  /** Channel 2 — warnings, rendered red. Non-fatal; throw to abort. */
  err: ChannelWriter;
  /** Alias for `log`, kept because it predates the channel split. */
  emit: ChannelWriter;
}
```

Export `ChannelWriter` from `packages/browser-terminal/src/index.ts`'s `export type { … } from './types.js'` block.

- [ ] **Step 4: Verify**

Run: `cargo test --workspace` → 219 (this is wasm-only plus a core accessor; no native test count change beyond Task 4's).
Run: `cargo clippy --workspace --all-targets` → clean.
Run: `just build` → succeeds.

The behaviour is browser-only — Task 6 proves it. Report any compile trouble with the `Closure::<dyn Fn(String, JsValue)>` signature (a two-arg closure with an optional second argument: JS calling `mode('byte')` passes `undefined`, which arrives as a `JsValue` — `Reflect::get` on `undefined` returns `Err`, handled by the `.ok()` above; confirm that is what happens rather than a panic).

- [ ] **Step 5: Commit**

```bash
git add crates/ packages/browser-terminal/src/
git commit -m "ctx.log and ctx.err become callable writer objects

Still callable for whole lines -- every existing command is untouched --
and now carrying .write for partial output, .flush, and .mode for
byte/line/block buffering with a configurable delimiter. Buffers flush at
the command boundary and a reset is emitted so a command's colour cannot
leak into the prompt."
```

---

## Task 6: Progress-bar demo and browser proof

**Files:**
- Modify: `packages/demo/src/main.ts`, `packages/demo/index.html`, `packages/demo/tests/smoke.spec.ts`

- [ ] **Step 1: Add the command**

In `packages/demo/src/main.ts`, inside `main()`:

```ts
  // #region progress
  // A progress bar: byte mode flushes every write, and `\r` returns to the
  // start of the line so each update overwrites the last instead of
  // scrolling. `\r` survives sanitizing because it cannot leave the current
  // line — cursor moves that could are stripped.
  bt.registerCommand(
    {
      name: 'progress',
      summary: 'Draw a progress bar in place, then finish',
      optional: [{ name: 'steps', shape: 'int', desc: 'how many steps (default 10)' }],
    },
    async ({ positionals }, _input, ctx) => {
      const steps = Number(positionals[0] ?? 10);
      ctx.log.mode('byte');
      for (let i = 1; i <= steps; i++) {
        await new Promise((r) => setTimeout(r, 80));
        const filled = '█'.repeat(i);
        const empty = '░'.repeat(steps - i);
        ctx.log.write(`\r${filled}${empty} ${Math.round((i / steps) * 100)}%`);
      }
      ctx.log.write('\n');
      return `done in ${steps} steps`;
    },
  );
  // #endregion
```

Add a code panel next to the others: `codePanel('A progress bar (byte mode + \\r)', selfSource, 'progress'),`

- [ ] **Step 2: Show it in the Try block**

In `packages/demo/index.html`, add to the Try `<pre>`:

```
progress           # byte mode + \r: redraws in place
```

- [ ] **Step 3: Browser test**

Add to `packages/demo/tests/smoke.spec.ts`. The proof that byte mode + `\r` works is that the bar **overwrites** rather than accumulating: after it finishes, the pane must contain exactly one bar line, not ten.

```ts
test('a progress bar redraws in place rather than scrolling', async ({ page }) => {
  await page.goto('/');
  await waitForTerminal(page);

  const ta = page.locator('[data-browser-terminal]').locator('.xterm-helper-textarea');
  await ta.click();
  await ta.pressSequentially('progress 5');
  await ta.press('Enter');

  // Wait for completion (5 steps x 80ms plus slack).
  await page.waitForFunction(
    () => {
      const root = document.querySelector('[data-browser-terminal]')!.shadowRoot!;
      return (root.querySelector('.xterm-rows')?.textContent ?? '').includes('done in 5 steps');
    },
    null,
    { timeout: 5000 },
  );

  const text = await page.evaluate(() => {
    const root = document.querySelector('[data-browser-terminal]')!.shadowRoot!;
    return root.querySelector('.xterm-rows')?.textContent ?? '';
  });

  // Ten writes, one visible bar: `\r` returned to column 0 each time, so
  // only the final 100% state survives. Without in-place rewrite there
  // would be one line per step.
  expect(text).toContain('100%');
  const barLines = (text.match(/100%/g) ?? []).length;
  expect(barLines).toBe(1);
  // The intermediate percentages must NOT still be on screen.
  expect(text).not.toContain('20%');
});
```

**Verify the observation is real:** temporarily assert on something absent (e.g. `expect(text).toContain('zzz')`) and confirm it fails, proving the scrape reads live content. Then restore. Report that you did this. If `.xterm-rows` textContent proves unreliable, use the xterm buffer API instead and say what you used.

- [ ] **Step 4: A block-mode test**

```ts
test('block mode holds output until flush', async ({ page }) => {
  await page.goto('/');
  await waitForTerminal(page);

  const result = await page.evaluate(async () => {
    window.bt.registerCommand({ name: 'batched', summary: 'block mode' }, async (_a, _i, ctx) => {
      ctx.log.mode('block');
      ctx.log.write('one ');
      ctx.log.write('two ');
      const beforeFlush = 'nothing emitted yet';
      ctx.log.flush();
      return beforeFlush;
    });
    const r = await window.bt.run('batched');
    return { value: r.value, log: r.log };
  });

  expect(result.value).toBe('nothing emitted yet');
  // The three writes arrive as ONE entry, not three -- that is what block
  // mode buys.
  expect(result.log.join('')).toContain('one two ');
  expect(result.log.length).toBeLessThanOrEqual(2);
});
```

Adjust the `length` expectation once you see the real shape (the boundary flush may add a reset entry) — but do not weaken the "one entry, not three" claim; that is the point.

- [ ] **Step 5: Run and verify**

```bash
just build && npm --prefix packages/demo run build
cd packages/demo && npx playwright test
```
Expected: 14 existing + 2 new = 16 passed.

If the progress-bar test fails, `\r` is not the cause — `crlf()` (engine.rs:372) only rewrites `\n` and leaves a bare `\r` intact, verified while writing this plan. Look instead at the buffer mode (is `byte` actually set?), the allowlist sanitizer (did `writer_text` keep the `\r`?), or whether the boundary flush emitted a stray reset mid-bar.

- [ ] **Step 6: wasm size**

```bash
ls -l packages/browser-terminal/dist/wasm/bterm_wasm_bg.wasm | awk '{print $5}'
```
Pre-stage-5b: **465827 bytes**. Report the delta; if > 2KB, update the `Current wasm size:` line in `README.md`.

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "Demo a progress bar, and prove it redraws in place

byte mode plus \\r overwrites the line each step: ten writes leave one
visible bar, verified in a browser. A block-mode command batches three
writes into one emission. Records the wasm size."
```

---

## Self-review notes

**Spec coverage:** the allowlist sanitizer with its current-line invariant (Task 1); raw records so sinks emit verbatim (Tasks 2–3); byte/line/block modes with a configurable delimiter, explicit flush, and the flush-at-end invariant (Task 4); `ctx.log`/`ctx.err` as callable writers plus the SGR boundary reset (Task 5); progress-bar demo and browser proof (Task 6).

**A structural decision the spec did not anticipate.** `Record` sanitizes on construction — a stage-1 security property with 11 call sites. Rather than loosen it, this plan *adds* `Record::raw_log`/`raw_err` using the allowlist sanitizer and an `is_raw()` flag sinks honour. Existing diagnostics keep strip-everything behaviour untouched, and the new capability is opt-in at the call site.

**Known soft spots to watch:**
- Task 5's `RefCell` borrow must drop before `sink.write` (which can re-enter JS). The plan shows the `let out = { … };` shape that guarantees it.
- The `mode('byte')` single-argument call passes `undefined` as the second parameter; the plan uses `.ok()` on `Reflect::get` so that path cannot panic, and asks the implementer to confirm.

**Deliberately out of scope** (from the spec): cursor up/down and absolute positioning, so multi-line live dashboards are not supported; and SGR leak protection beyond the command-boundary reset.
