//! Where a pipeline's diagnostic output goes.
//!
//! Three channels exist; only two appear here. Channel 1 (data) is the
//! pipeline's return value and has no write API at all — which is what makes
//! "diagnostics can never enter the data pipe" a property of the type system
//! rather than a rule authors must remember.

use std::cell::RefCell;

/// Which diagnostic channel a record belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channel {
    /// Channel 3 — progress and commentary.
    Log,
    /// Channel 2 — warnings and diagnostics. Non-fatal: a thrown
    /// `ShellError` still aborts the pipeline. This is the case we
    /// previously had no way to express, "warn and keep going".
    Err,
}

/// A line written to a diagnostic channel.
///
/// The text is sanitized on construction, so a `Record` cannot hold an
/// escape sequence. Diagnostics originate in page-controlled TypeScript,
/// and they reach more than one destination — a pane, a programmatic
/// caller's UI, a log aggregator. Sanitizing per-sink meant each new sink
/// had to remember; doing it here makes "diagnostics leaving the engine are
/// escape-clean" a property of the type instead.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    channel: Channel,
    text: String,
    /// Written through `ctx.log.write` rather than `ctx.log(line)`: the text
    /// went through the *allowlist* sanitizer, and a sink must emit it
    /// verbatim — no appended newline, no styling wrapper — so partial
    /// writes and in-place rewrites (progress bars) work.
    raw: bool,
}

impl Record {
    pub fn log(text: impl AsRef<str>) -> Self {
        Self::new(Channel::Log, text)
    }

    pub fn err(text: impl AsRef<str>) -> Self {
        Self::new(Channel::Err, text)
    }

    fn new(channel: Channel, text: impl AsRef<str>) -> Self {
        Record {
            channel,
            text: crate::render::diagnostic_text(text.as_ref()),
            raw: false,
        }
    }

    /// A raw write to the log channel: allowlist-sanitized, emitted verbatim.
    pub fn raw_log(text: impl AsRef<str>) -> Self {
        Self::new_raw(Channel::Log, text)
    }

    /// A raw write to the err channel: allowlist-sanitized, emitted verbatim.
    pub fn raw_err(text: impl AsRef<str>) -> Self {
        Self::new_raw(Channel::Err, text)
    }

    /// Sanitized by the allowlist rather than strip-everything: a raw write
    /// may keep `\r` and styling, which is what makes a progress bar
    /// possible. The narrower rules live in `render::writer_text`.
    fn new_raw(channel: Channel, text: impl AsRef<str>) -> Self {
        Record {
            channel,
            text: crate::render::writer_text(text.as_ref()),
            raw: true,
        }
    }

    pub fn channel(&self) -> Channel {
        self.channel
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// Whether a sink must emit this verbatim (no newline, no styling
    /// wrapper).
    pub fn is_raw(&self) -> bool {
        self.raw
    }
}

/// Destination for diagnostics. Synchronous by design: a command's `run`
/// future may call `write` from contexts where awaiting is not an option
/// (e.g. a synchronous JS callback), so the trait cannot require one. A
/// `PaneSink` implementation is expected to take its own short engine borrow
/// inside `write` rather than relying on one already being held — no borrow
/// is held at the call site itself.
pub trait Sink {
    fn write(&self, record: Record);

    /// Resolves when the sink can accept more output.
    ///
    /// A pane sink overrides this to throttle a fast producer to display
    /// speed; every other sink can absorb output as fast as it arrives, so
    /// the default resolves immediately. Awaited by the progressive renderer
    /// between paints, with no engine borrow held.
    fn ready(&self) -> crate::registry::LocalBoxFuture<()> {
        crate::registry::ready(())
    }
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

    pub fn log_lines(&self) -> Vec<String> {
        self.lines(|r| matches!(r.channel(), Channel::Log))
    }

    pub fn err_lines(&self) -> Vec<String> {
        self.lines(|r| matches!(r.channel(), Channel::Err))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collecting_sink_separates_channels() {
        let sink = CollectingSink::new();
        sink.write(Record::log("progress"));
        sink.write(Record::err("uh oh"));
        sink.write(Record::log("more"));

        assert_eq!(sink.log_lines(), vec!["progress", "more"]);
        assert_eq!(sink.err_lines(), vec!["uh oh"]);
    }

    #[test]
    fn a_record_cannot_hold_an_escape_sequence() {
        // Diagnostics are page-controlled; the type, not the sink, is what
        // guarantees they are safe to hand to any destination.
        let r = Record::err("\x1b[2J\x1b[Hcleared\nsecond");
        assert_eq!(r.text(), "cleared second");
        assert_eq!(r.channel(), Channel::Err);
    }

    #[test]
    fn a_raw_record_keeps_line_local_control_but_not_screen_control() {
        // Raw writes carry a progress bar's \r; the auto-sanitized path
        // still strips everything, so existing diagnostics are unchanged.
        let raw = Record::raw_log("50%\r");
        assert_eq!(raw.text(), "50%\r");
        assert!(raw.is_raw());

        let cooked = Record::log("50%\r");
        assert_eq!(cooked.text(), "50% ", "the existing path is unchanged");
        assert!(!cooked.is_raw());

        // Even raw text cannot clear the screen -- the allowlist decides,
        // not the caller.
        assert_eq!(Record::raw_err("\x1b[2Jx").text(), "x");
        assert!(Record::raw_err("x").is_raw());
    }

    #[test]
    fn sinks_without_backpressure_are_immediately_ready() {
        // The default: a sink that cannot fall behind resolves at once, so
        // the progressive renderer's await between paints costs nothing.
        use crate::eval::block_on;
        let sink = CollectingSink::new();
        block_on(sink.ready());
        block_on(NullSink.ready());
    }
}
