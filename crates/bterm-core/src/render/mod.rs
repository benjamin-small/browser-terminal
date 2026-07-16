//! Render a `Value` to ANSI text. Called exactly once per pipeline —
//! commands never format their own output (`table` / `to json` exist to
//! force a string mid-pipe).
//!
//! Lines end with `\n`; the pane layer converts to `\r\n` for xterm.

use crate::value::Value;
use indexmap::IndexSet;
use unicode_width::UnicodeWidthStr;

const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const CYAN: &str = "\x1b[36m";
const YELLOW: &str = "\x1b[33m";
const GREEN: &str = "\x1b[32m";
const RESET: &str = "\x1b[0m";

/// Minimum column width before we give up shrinking and let the table
/// overflow the pane.
const MIN_COL_WIDTH: usize = 5;

pub fn render(value: &Value, width: u16) -> String {
    match value {
        Value::List(items) if value.is_table() && !items.is_empty() => render_table(items, width),
        Value::List(items) => render_list(items),
        Value::Record(map) => render_record(map),
        scalar => format!("{}\n", colored_scalar(scalar)),
    }
}

/// Plain, uncolored, single-line display of a value (cell contents,
/// interpolation, `to json`-lite for nested).
pub fn plain(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::Bool(b) => b.to_string(),
        Value::Int(n) => n.to_string(),
        Value::Float(f) => format_float(*f),
        Value::Str(s) => s.clone(),
        Value::List(items) => format!("[{} items]", items.len()),
        Value::Record(map) => format!("{{{} fields}}", map.len()),
    }
}

/// Strip control characters from user-supplied text so a Str value cannot
/// inject ANSI escapes into the terminal. Newlines and tabs survive in
/// scalar display; table cells flatten them too (`cell_text`).
fn sanitize(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .collect()
}

/// Single-line, escape-free cell/key text for tables and records.
fn cell_text(s: &str) -> String {
    s.chars()
        .map(|c| if c == '\n' || c == '\t' || c == '\r' { ' ' } else { c })
        .filter(|c| !c.is_control())
        .collect()
}

fn format_float(f: f64) -> String {
    if f.fract() == 0.0 && f.abs() < 1e15 {
        format!("{f:.1}")
    } else {
        f.to_string()
    }
}

fn colored_scalar(value: &Value) -> String {
    match value {
        Value::Null => format!("{DIM}null{RESET}"),
        Value::Bool(b) => format!("{YELLOW}{b}{RESET}"),
        Value::Int(_) | Value::Float(_) => format!("{CYAN}{}{RESET}", plain(value)),
        Value::Str(s) => sanitize(s),
        other => plain(other),
    }
}

fn render_list(items: &[Value]) -> String {
    if items.is_empty() {
        return format!("{DIM}(empty list){RESET}\n");
    }
    let idx_width = (items.len() - 1).to_string().len();
    let mut out = String::new();
    for (i, item) in items.iter().enumerate() {
        out.push_str(&format!("{DIM}{i:>idx_width$}{RESET}  {}\n", colored_scalar(item)));
    }
    out
}

fn render_record(map: &indexmap::IndexMap<String, Value>) -> String {
    if map.is_empty() {
        return format!("{DIM}(empty record){RESET}\n");
    }
    let keys: Vec<String> = map.keys().map(|k| cell_text(k)).collect();
    let key_width = keys.iter().map(|k| UnicodeWidthStr::width(k.as_str())).max().unwrap_or(0);
    let mut out = String::new();
    for (k, v) in keys.iter().zip(map.values()) {
        let pad = " ".repeat(key_width - UnicodeWidthStr::width(k.as_str()));
        out.push_str(&format!("{GREEN}{k}{RESET}{pad}  {}\n", colored_scalar(v)));
    }
    out
}

/// Box-drawn table for a `List` of `Record`s. Column set is the union of
/// keys in first-seen order. The widest column shrinks (with `…`) to fit the
/// width budget; numbers right-align; header is bold.
fn render_table(rows: &[Value], width: u16) -> String {
    let mut columns: IndexSet<String> = IndexSet::new();
    for row in rows {
        if let Value::Record(map) = row {
            for k in map.keys() {
                columns.insert(k.clone());
            }
        }
    }
    let columns: Vec<String> = columns.into_iter().collect();
    if columns.is_empty() {
        return format!("{DIM}({} empty records){RESET}\n", rows.len());
    }

    let cell = |row: &Value, col: &str| -> (String, bool) {
        match row {
            Value::Record(map) => match map.get(col) {
                Some(v) => (cell_text(&plain(v)), matches!(v, Value::Int(_) | Value::Float(_))),
                None => (String::new(), false),
            },
            _ => (String::new(), false),
        }
    };

    // Display names are sanitized; `columns` keeps raw keys for cell lookup.
    let headers: Vec<String> = columns.iter().map(|c| cell_text(c)).collect();

    // Natural widths.
    let mut widths: Vec<usize> = columns
        .iter()
        .zip(&headers)
        .map(|(c, h)| {
            rows.iter()
                .map(|r| UnicodeWidthStr::width(cell(r, c).0.as_str()))
                .chain([UnicodeWidthStr::width(h.as_str())])
                .max()
                .unwrap_or(0)
        })
        .collect();

    // Shrink the widest column until the table fits (or we hit the floor).
    let overhead = 3 * columns.len() + 1; // "│ " per column + trailing "│" and padding
    let budget = (width as usize).saturating_sub(overhead);
    while widths.iter().sum::<usize>() > budget {
        let (widest, w) = match widths.iter().enumerate().max_by_key(|(_, w)| **w) {
            Some((i, w)) => (i, *w),
            None => break,
        };
        if w <= MIN_COL_WIDTH {
            break;
        }
        widths[widest] = w - 1;
    }

    let truncate = |s: &str, max: usize| -> String {
        if UnicodeWidthStr::width(s) <= max {
            return s.to_string();
        }
        let mut out = String::new();
        let mut used = 0usize;
        for ch in s.chars() {
            let w = UnicodeWidthStr::width(ch.to_string().as_str());
            if used + w > max.saturating_sub(1) {
                break;
            }
            used += w;
            out.push(ch);
        }
        out.push('…');
        out
    };

    let rule = |left: &str, mid: &str, right: &str| -> String {
        let mut s = String::from(left);
        for (i, w) in widths.iter().enumerate() {
            s.push_str(&"─".repeat(w + 2));
            s.push_str(if i + 1 == widths.len() { right } else { mid });
        }
        s.push('\n');
        s
    };

    let mut out = String::new();
    out.push_str(&rule("┌", "┬", "┐"));
    out.push('│');
    for (c, w) in headers.iter().zip(&widths) {
        let text = truncate(c, *w);
        let pad = " ".repeat(w - UnicodeWidthStr::width(text.as_str()));
        out.push_str(&format!(" {BOLD}{text}{RESET}{pad} │"));
    }
    out.push('\n');
    out.push_str(&rule("├", "┼", "┤"));
    for row in rows {
        out.push('│');
        for (c, w) in columns.iter().zip(&widths) {
            let (text, numeric) = cell(row, c);
            let text = truncate(&text, *w);
            let pad = " ".repeat(w - UnicodeWidthStr::width(text.as_str()));
            if numeric {
                out.push_str(&format!(" {pad}{CYAN}{text}{RESET} │"));
            } else {
                out.push_str(&format!(" {text}{pad} │"));
            }
        }
        out.push('\n');
    }
    out.push_str(&rule("└", "┴", "┘"));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::Value;

    fn strip_ansi(s: &str) -> String {
        let mut out = String::new();
        let mut chars = s.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                for c2 in chars.by_ref() {
                    if c2 == 'm' {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    fn table_fixture() -> Value {
        Value::List(vec![
            Value::record([
                ("text".to_string(), Value::Str("Rust language".into())),
                ("stars".to_string(), Value::Int(95000)),
            ]),
            Value::record([
                ("text".to_string(), Value::Str("WebAssembly".into())),
                ("stars".to_string(), Value::Int(12345)),
                ("extra".to_string(), Value::Str("only here".into())),
            ]),
        ])
    }

    #[test]
    fn table_renders_box_drawing_and_union_columns() {
        let out = strip_ansi(&render(&table_fixture(), 80));
        assert!(out.contains('┌') && out.contains('┘'));
        assert!(out.contains("text"));
        assert!(out.contains("stars"));
        assert!(out.contains("extra"), "union of keys:\n{out}");
        assert!(out.contains("Rust language"));
    }

    #[test]
    fn narrow_width_truncates_with_ellipsis() {
        let out = strip_ansi(&render(&table_fixture(), 24));
        assert!(out.contains('…'), "expected truncation:\n{out}");
        for line in out.lines() {
            assert!(
                UnicodeWidthStr::width(line) <= 26,
                "line too wide: {line}"
            );
        }
    }

    #[test]
    fn scalar_list_renders_indexed() {
        let v = Value::List(vec![Value::Str("a".into()), Value::Str("b".into())]);
        let out = strip_ansi(&render(&v, 80));
        assert!(out.contains("0  a"));
        assert!(out.contains("1  b"));
    }

    #[test]
    fn record_renders_key_value() {
        let v = Value::record([
            ("name".to_string(), Value::Str("bterm".into())),
            ("panes".to_string(), Value::Int(2)),
        ]);
        let out = strip_ansi(&render(&v, 80));
        assert!(out.contains("name   bterm"));
        assert!(out.contains("panes  2"));
    }

    #[test]
    fn scalars_render_plainly() {
        assert_eq!(strip_ansi(&render(&Value::Int(42), 80)), "42\n");
        assert_eq!(strip_ansi(&render(&Value::Str("hi".into()), 80)), "hi\n");
        assert_eq!(strip_ansi(&render(&Value::Null, 80)), "null\n");
    }

    #[test]
    fn escape_injection_is_stripped() {
        // A Str value must not be able to inject ANSI into the terminal.
        let v = Value::Str("evil\x1b[2Jwiped".into());
        let out = render(&v, 80);
        assert!(!out.contains("\x1b[2J"), "ESC must be stripped: {out:?}");
        assert!(out.contains("evilwiped") || out.contains("evil"), "{out:?}");
    }

    #[test]
    fn newlines_in_cells_do_not_break_table_geometry() {
        let v = Value::List(vec![Value::record([
            ("a".to_string(), Value::Str("line1\nline2".into())),
            ("b".to_string(), Value::Int(1)),
        ])]);
        let out = strip_ansi(&render(&v, 80));
        // Header + rules + exactly one data row.
        let data_rows = out.lines().filter(|l| l.contains("line1")).count();
        assert_eq!(data_rows, 1);
        assert!(out.contains("line1 line2"), "newline flattened: {out}");
    }

    #[test]
    fn empty_records_render_placeholder_not_degenerate_box() {
        let v = Value::List(vec![
            Value::Record(indexmap::IndexMap::new()),
            Value::Record(indexmap::IndexMap::new()),
        ]);
        let out = strip_ansi(&render(&v, 80));
        assert!(out.contains("(2 empty records)"), "{out}");
        assert!(!out.contains('┌'));
    }
}
