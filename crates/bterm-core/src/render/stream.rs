//! Incremental table rendering for a streaming result.
//!
//! A box table needs column widths, which need rows — but a live stream has
//! no end. So we *probe*: buffer up to `probe_rows` records, commit widths
//! from them, paint the header and those rows, then stream each later row at
//! the committed widths. A later value wider than its column is truncated
//! rather than reflowing the table, which would mean repainting rows already
//! scrolled past.
//!
//! Clockless by design: the row-count and end-of-stream commits live here,
//! while the *time* bound ("paint within T of the first row") is the host's
//! job — this crate has no timer. The host calls `commit()` when its
//! deadline fires.

use super::{column_widths, table_bottom, table_columns, table_header, table_row, DIM, RESET};
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

    /// Feed one record. `None` while probing; on the commit transition the
    /// header plus every probed row; afterwards, that one row.
    pub fn push(&mut self, row: Value) -> Option<String> {
        if let Some(c) = &self.committed {
            if c.cols.is_empty() {
                // Already committed to the "(N empty records)" placeholder
                // (see `commit`). There is no per-row rendering for a
                // record with no fields, and the count already painted
                // can't be updated in place, so later field-less rows are
                // silently absorbed rather than emitting a meaningless
                // empty row.
                return None;
            }
            return Some(table_row(&row, &c.cols, &c.widths));
        }
        self.probe.push(row);
        if self.probe.len() >= self.probe_rows {
            self.commit()
        } else {
            None
        }
    }

    /// Commit now — the host's deadline fired, or the stream ended. Paints
    /// the header and whatever was probed. `None` if nothing to paint or
    /// already committed.
    pub fn commit(&mut self) -> Option<String> {
        if self.committed.is_some() || self.probe.is_empty() {
            return None;
        }
        let rows = std::mem::take(&mut self.probe);
        let cols = table_columns(&rows);
        if cols.is_empty() {
            // Records with no fields have no columns to tabulate, but they
            // are still output -- `render_table` prints a placeholder
            // rather than nothing, and the pane must match it or a finite
            // result would render differently here than through
            // run()/the CLI. `widths` stays empty, which `finish` uses to
            // know not to append a bottom border for this case.
            self.committed = Some(Committed { cols, widths: Vec::new() });
            return Some(format!("{DIM}({} empty records){RESET}\n", rows.len()));
        }
        let widths = column_widths(&cols, &rows, self.width);
        let mut out = table_header(&cols, &widths);
        for row in &rows {
            out.push_str(&table_row(row, &cols, &widths));
        }
        self.committed = Some(Committed { cols, widths });
        Some(out)
    }

    /// Whether this renderer has committed widths (via `push` reaching
    /// `probe_rows`, or a forced `commit`). Lets a caller stop polling once
    /// there is nothing left for a deadline to do — `commit()` alone can't
    /// tell "already committed" apart from "still empty", since both return
    /// `None`.
    pub fn is_committed(&self) -> bool {
        self.committed.is_some()
    }

    /// Close the table: commit anything still probing, then the bottom
    /// border. Empty if nothing was ever painted.
    pub fn finish(&mut self) -> String {
        let mut out = self.commit().unwrap_or_default();
        if let Some(c) = &self.committed {
            // Empty `widths` is the field-less-records placeholder, which
            // has no box to close -- `render_table` prints only the
            // placeholder line, no border, and `table_bottom(&[])` would
            // otherwise append a stray "└\n".
            if !c.widths.is_empty() {
                out.push_str(&table_bottom(&c.widths));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::Value;
    use indexmap::IndexMap;

    fn rec(id: i64) -> Value {
        let mut m = IndexMap::new();
        m.insert("id".to_string(), Value::Int(id));
        Value::Record(m)
    }

    #[test]
    fn commits_after_probe_rows_and_streams_the_rest() {
        // Probe = 2 rows: the first buffers silently, the second triggers a
        // commit that paints the header plus both probed rows. Later rows
        // stream one at a time at the committed widths.
        let mut r = StreamRenderer::new(80, 2);
        assert_eq!(r.push(rec(1)), None, "still probing");
        let committed = r.push(rec(2)).expect("commit on 2nd row");
        assert!(committed.contains("id"), "header painted: {committed:?}");
        assert!(committed.contains('1') && committed.contains('2'), "probe rows: {committed:?}");

        let third = r.push(rec(3)).expect("streamed row");
        assert!(third.contains('3'));
        assert!(!third.contains("id"), "header not repainted: {third:?}");

        let end = r.finish();
        assert!(!end.is_empty(), "bottom border painted: {end:?}");
    }

    #[test]
    fn commit_forces_paint_with_fewer_than_probe_rows() {
        // A slow source: only one row arrived before the host's deadline.
        // commit() paints using widths from just that row.
        let mut r = StreamRenderer::new(80, 50);
        assert_eq!(r.push(rec(7)), None);
        let out = r.commit().expect("forced commit");
        assert!(out.contains('7'), "the probed row painted: {out:?}");
        assert!(r.push(rec(8)).expect("streams after commit").contains('8'));
    }

    #[test]
    fn empty_stream_paints_nothing() {
        // No rows ever arrived: an empty table is meaningless, so nothing is
        // painted rather than a zero-column header.
        let mut r = StreamRenderer::new(80, 50);
        assert_eq!(r.commit(), None, "nothing to commit");
        assert_eq!(r.finish(), String::new());
    }

    #[test]
    fn a_full_probe_matches_render_table_exactly() {
        // The gate: when every row arrives inside the probe, the streamed
        // output must be byte-identical to rendering the whole list at once.
        let rows = vec![rec(1), rec(2), rec(3)];
        let mut r = StreamRenderer::new(80, 50);
        let mut streamed = String::new();
        for row in &rows {
            if let Some(text) = r.push(row.clone()) {
                streamed.push_str(&text);
            }
        }
        streamed.push_str(&r.finish());
        assert_eq!(streamed, super::super::render(&Value::List(rows), 80));
    }

    #[test]
    fn field_less_records_match_render_table_exactly() {
        // Records with no fields have no columns to tabulate, but they are
        // still output: `render_table` prints "(N empty records)" rather
        // than a zero-column box. `commit` used to drop the buffered rows
        // and paint nothing at all in this case -- the gate here is the
        // same shape as `a_full_probe_matches_render_table_exactly`, byte
        // for byte, so the streaming path can't silently diverge from a
        // one-shot render again.
        let rows = vec![
            Value::Record(indexmap::IndexMap::new()),
            Value::Record(indexmap::IndexMap::new()),
            Value::Record(indexmap::IndexMap::new()),
        ];
        let mut r = StreamRenderer::new(80, 50);
        let mut streamed = String::new();
        for row in &rows {
            if let Some(text) = r.push(row.clone()) {
                streamed.push_str(&text);
            }
        }
        streamed.push_str(&r.finish());
        assert_eq!(streamed, super::super::render(&Value::List(rows), 80));
    }
}
