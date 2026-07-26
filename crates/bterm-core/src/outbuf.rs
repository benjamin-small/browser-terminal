//! Per-command output buffering.
//!
//! A command controls its own cadence: `line` (the stdio default) flushes on
//! a delimiter, `byte` flushes every write so a progress bar appears as it is
//! drawn, and `block` holds everything until `flush()`. Three invariants
//! hold in every mode: `flush()` always works, whatever is buffered when the
//! command ends is emitted rather than lost, and no mode holds more than
//! `BLOCK_LIMIT` — cadence is the command's to choose, memory is not.
//!
//! Pure: it returns the text to emit and knows nothing about sinks or JS.

/// How much any mode holds before flushing itself, so a runaway command
/// cannot buffer without bound. It applies to `line` as much as to `block`:
/// output with no delimiter in it (a minified JSON body, a `\r` progress bar
/// written in the default mode) is exactly as unbounded as output a command
/// never flushes.
pub const BLOCK_LIMIT: usize = 8 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Flush when the delimiter is written (default, delimiter `\n`), or at
    /// `BLOCK_LIMIT` if it never is.
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
    /// How much of `buf` has already been searched for the current
    /// delimiter and found not to contain one. Only bytes past this — plus
    /// a `delimiter.len() - 1` overlap, where a multi-byte delimiter can
    /// straddle the join — can hold a new match, so a write scans its own
    /// text rather than everything written before it. Reset to 0 whenever
    /// `buf` empties or the delimiter changes, since neither leaves the
    /// old scan valid.
    scanned: usize,
}

impl Default for OutputBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl OutputBuffer {
    pub fn new() -> Self {
        OutputBuffer {
            mode: Mode::Line,
            delimiter: "\n".to_string(),
            buf: String::new(),
            scanned: 0,
        }
    }

    /// Switch modes. `delimiter` applies to `Line` only; `None` keeps the
    /// current one (default `\n`). An empty delimiter is ignored -- it would
    /// make every write flush, which is what `Byte` is for.
    pub fn set_mode(&mut self, mode: Mode, delimiter: Option<String>) {
        self.mode = mode;
        if let Some(d) = delimiter {
            if !d.is_empty() {
                self.delimiter = d;
            }
        }
        // A new delimiter says nothing about the old scan: text already
        // buffered may hold it. Rescan from the start -- this happens once
        // per mode switch, over at most `BLOCK_LIMIT` bytes.
        self.scanned = 0;
    }

    /// The current line delimiter, appended by the `ctx.log(line)` sugar.
    pub fn delimiter(&self) -> &str {
        &self.delimiter
    }

    /// Buffer `text`, returning whatever should be emitted now.
    pub fn write(&mut self, text: &str) -> Option<String> {
        // The limit is checked BEFORE the append, and in every mode. Two
        // reasons:
        //
        // 1. A single write larger than the limit must not land in `buf` at
        //    all. Checking after the append meant one 64 MiB `write` copied
        //    all 64 MiB into the buffer and only then flushed it -- in wasm
        //    that grows a linear memory that never shrinks back.
        // 2. `line` mode needs the bound as much as `block` does. Output
        //    with no delimiter in it never flushed, so `buf` grew without
        //    limit. Emitting a partial line early is a visible cosmetic
        //    cost; holding until the tab freezes (or the allocation fails,
        //    which under `panic = "abort"` kills the module and takes the
        //    terminal with it) is not recoverable. Partial output beats a
        //    dead terminal.
        //
        // Over the limit the buffer is bypassed entirely: everything pending
        // plus this write goes out now, tail included. Line mode's
        // hold-back-the-partial-tail behaviour is a display nicety and is
        // the first thing to give when a command is running away.
        if self.buf.len() + text.len() >= BLOCK_LIMIT {
            return self.emit_through(text);
        }
        match self.mode {
            Mode::Byte => self.emit_through(text),
            Mode::Line => {
                // Flush through the LAST delimiter, keeping any partial tail
                // buffered -- that is what keeps a half-written line off
                // screen until it completes.
                //
                // Only the unscanned window can hold a new match. Searching
                // the whole buffer every write was quadratic in the output:
                // 16 MB of delimiter-free text measured at 69s of `rfind`
                // natively, and this runs on the browser's main thread.
                let from = self.scan_start();
                self.buf.push_str(text);
                match self.buf[from..].rfind(&self.delimiter) {
                    Some(idx) => {
                        // `idx` indexes a whole delimiter match, so the byte
                        // after it is a char boundary and `split_off` cannot
                        // panic on multi-byte text.
                        let split = from + idx + self.delimiter.len();
                        let rest = self.buf.split_off(split);
                        let out = std::mem::replace(&mut self.buf, rest);
                        // What is left is the tail after the last delimiter:
                        // searched, and known to hold none.
                        self.scanned = self.buf.len();
                        Some(out)
                    }
                    None => {
                        self.scanned = self.buf.len();
                        None
                    }
                }
            }
            Mode::Block => {
                self.buf.push_str(text);
                None
            }
        }
    }

    /// Where the next delimiter search must start.
    ///
    /// Everything below `scanned` has been searched under the current
    /// delimiter and holds none, so a match can only begin within
    /// `delimiter.len() - 1` bytes of that point. The result is walked back
    /// to a char boundary: slicing a `String` mid-character panics, and
    /// starting early only widens the window, never misses a match.
    fn scan_start(&self) -> usize {
        let overlap = self.delimiter.len().saturating_sub(1);
        let mut start = self.scanned.min(self.buf.len()).saturating_sub(overlap);
        while start > 0 && !self.buf.is_char_boundary(start) {
            start -= 1;
        }
        start
    }

    /// Emit everything pending plus `text`, without routing `text` through
    /// the buffer. `buf` is left empty, so an oversized write never grows it.
    fn emit_through(&mut self, text: &str) -> Option<String> {
        if self.buf.is_empty() {
            self.scanned = 0;
            return if text.is_empty() { None } else { Some(text.to_string()) };
        }
        let mut out = std::mem::take(&mut self.buf);
        out.push_str(text);
        self.scanned = 0;
        Some(out)
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
        self.scanned = 0;
        if self.buf.is_empty() {
            None
        } else {
            Some(std::mem::take(&mut self.buf))
        }
    }
}

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
    fn line_mode_flushes_through_the_last_delimiter_only() {
        // Two complete lines plus a partial tail: the complete lines go now,
        // the tail waits -- that is what keeps a half-written line off screen.
        let mut b = OutputBuffer::new();
        assert_eq!(b.write("a\nb\ntail"), Some("a\nb\n".to_string()));
        assert_eq!(b.finish(), Some("tail".to_string()));
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
        // A runaway command must not buffer without bound.
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
            let expected = if mode == Mode::Byte {
                None
            } else {
                Some("tail with no delimiter".to_string())
            };
            assert_eq!(b.finish(), expected, "mode {mode:?} lost its tail");
        }
    }

    #[test]
    fn switching_modes_does_not_drop_buffered_text() {
        // A command may set a mode after writing. Whatever is pending must
        // survive the switch rather than vanishing.
        let mut b = OutputBuffer::new();
        assert_eq!(b.write("pending"), None);
        b.set_mode(Mode::Byte, None);
        assert_eq!(b.write("!"), Some("pending!".to_string()));
    }

    #[test]
    fn line_mode_is_bounded_when_the_delimiter_never_arrives() {
        // The failure this pins: `line` is the DEFAULT mode, and text with
        // no delimiter in it (a minified JSON body, a `\r` progress bar)
        // used to flush nothing at all. The buffer grew without limit and
        // every write re-scanned all of it -- 16 MB measured at 69s of
        // `rfind` on the browser's main thread.
        //
        // Asserting the bound rather than a wall-clock time is deliberate:
        // the bound is what makes the work bounded. With `buf` held under
        // `BLOCK_LIMIT`, a write's scan window is at most its own text plus
        // the leftover, so cost is linear in output rather than quadratic --
        // and unlike a timing threshold this cannot go flaky in CI.
        let mut b = OutputBuffer::new();
        let chunk = "x".repeat(1024);
        let writes = 64;
        let mut emitted = 0usize;
        for _ in 0..writes {
            if let Some(out) = b.write(&chunk) {
                emitted += out.len();
                assert!(out.len() <= 2 * BLOCK_LIMIT, "a single flush ran away: {}", out.len());
            }
        }
        let held = b.finish().map(|s| s.len()).unwrap_or(0);
        assert!(
            held < BLOCK_LIMIT,
            "{held} bytes still held with no delimiter in sight; the bound is {BLOCK_LIMIT}"
        );
        assert_eq!(emitted + held, writes * chunk.len(), "output was lost, not just bounded");
    }

    #[test]
    fn one_oversized_write_is_emitted_whole_rather_than_buffered() {
        // The limit is checked before the append, so a write bigger than it
        // never lands in the buffer (measured before: one 64 MiB write
        // buffered all 64 MiB, then flushed it). Observable here as the
        // buffer keeping nothing back -- not even the partial tail line
        // mode would normally hold.
        let mut b = OutputBuffer::new();
        let big = format!("{}\ntail", "x".repeat(BLOCK_LIMIT));
        let out = b.write(&big).expect("an oversized write flushes straight through");
        assert_eq!(out.len(), big.len(), "the whole write was emitted");
        assert_eq!(b.finish(), None, "none of it was retained");
    }

    #[test]
    fn a_multibyte_delimiter_split_across_writes_is_still_found() {
        // The scan window starts `delimiter.len() - 1` bytes before the
        // join, walked back to a char boundary -- so a delimiter straddling
        // two writes is found, and the walk-back never slices mid-character.
        let mut b = OutputBuffer::new();
        b.set_mode(Mode::Line, Some("日本".to_string()));
        assert_eq!(b.write("x日"), None, "half a delimiter is not a delimiter");
        assert_eq!(b.write("本y"), Some("x日本".to_string()));
        // Now with a longer multi-byte prefix, so the overlap start lands
        // mid-character and has to walk back.
        assert_eq!(b.write("語彙"), None);
        assert_eq!(b.write("日本z"), Some("y語彙日本".to_string()));
        assert_eq!(b.finish(), Some("z".to_string()));
    }

    #[test]
    fn a_new_delimiter_rescans_what_is_already_buffered() {
        // The scan watermark is per-delimiter: changing it mid-stream must
        // not leave an already-buffered match unseen.
        let mut b = OutputBuffer::new();
        assert_eq!(b.write("a|b"), None, "\\n is the delimiter so far");
        b.set_mode(Mode::Line, Some("|".to_string()));
        assert_eq!(b.write("c"), Some("a|".to_string()));
        assert_eq!(b.finish(), Some("bc".to_string()));
    }

    #[test]
    fn a_multibyte_line_splits_on_a_char_boundary() {
        // rfind returns a byte index; splitting after a complete delimiter
        // match is always a char boundary, even when the line is full of
        // multi-byte text. A regression here would panic, not fail softly.
        let mut b = OutputBuffer::new();
        assert_eq!(b.write("日本語\n█▓░"), Some("日本語\n".to_string()));
        assert_eq!(b.finish(), Some("█▓░".to_string()));
    }
}
