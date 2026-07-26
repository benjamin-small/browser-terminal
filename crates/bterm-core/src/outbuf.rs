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
    /// current one (default `\n`). An empty delimiter is ignored -- it would
    /// make every write flush, which is what `Byte` is for.
    pub fn set_mode(&mut self, mode: Mode, delimiter: Option<String>) {
        self.mode = mode;
        if let Some(d) = delimiter {
            if !d.is_empty() {
                self.delimiter = d;
            }
        }
    }

    /// The current line delimiter, appended by the `ctx.log(line)` sugar.
    pub fn delimiter(&self) -> &str {
        &self.delimiter
    }

    /// Buffer `text`, returning whatever should be emitted now.
    pub fn write(&mut self, text: &str) -> Option<String> {
        self.buf.push_str(text);
        match self.mode {
            Mode::Byte => self.take(),
            Mode::Line => {
                // Flush through the LAST delimiter, keeping any partial tail
                // buffered -- that is what keeps a half-written line off
                // screen until it completes.
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
    fn a_multibyte_line_splits_on_a_char_boundary() {
        // rfind returns a byte index; splitting after a complete delimiter
        // match is always a char boundary, even when the line is full of
        // multi-byte text. A regression here would panic, not fail softly.
        let mut b = OutputBuffer::new();
        assert_eq!(b.write("日本語\n█▓░"), Some("日本語\n".to_string()));
        assert_eq!(b.finish(), Some("█▓░".to_string()));
    }
}
