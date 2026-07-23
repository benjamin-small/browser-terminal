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

/// Destination for diagnostics. Synchronous by design: a command's `run`
/// future may call `write` from contexts where awaiting is not an option
/// (e.g. a synchronous JS callback), so the trait cannot require one. A
/// `PaneSink` implementation is expected to take its own short engine borrow
/// inside `write` rather than relying on one already being held — no borrow
/// is held at the call site itself.
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
