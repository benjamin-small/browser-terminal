//! Render a `Value` to ANSI text. Called exactly once per pipeline —
//! commands never format their own output (`table` / `to json` exist to
//! force a string mid-pipe).
//!
//! Lines end with `\n`; the pane layer converts to `\r\n` for xterm.

use crate::value::Value;
use indexmap::IndexSet;
use unicode_width::UnicodeWidthStr;

pub mod stream;

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

/// Strip escape sequences and control characters from user-supplied text, so
/// a value carrying page content can't move the cursor, clear the screen, or
/// set the window title.
///
/// Whole sequences go, not just the `ESC` byte: dropping the escape alone
/// leaves its body behind as visible litter (`[1m`) *and* still lets a
/// hostile string fake formatting. `keep_lines` preserves `\n`/`\t` for
/// scalar display; table cells flatten them to spaces so column widths stay
/// truthful.
fn strip_escapes(s: &str, keep_lines: bool) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            match chars.peek() {
                // CSI: ESC [ params… final(0x40..=0x7e)
                Some('[') => {
                    chars.next();
                    for c2 in chars.by_ref() {
                        if ('\x40'..='\x7e').contains(&c2) {
                            break;
                        }
                    }
                }
                // String-type escapes — OSC (`]`), DCS (`P`), APC (`_`), PM
                // (`^`), SOS (`X`) — all take an arbitrary-length body
                // terminated by BEL or ST (ESC \). Swallow the whole body:
                // leaving it behind (e.g. a Sixel DCS payload) is just as
                // much visible litter as leaving an escape's params would be.
                Some(']') | Some('P') | Some('_') | Some('^') | Some('X') => {
                    chars.next();
                    while let Some(c2) = chars.next() {
                        if c2 == '\x07' {
                            break;
                        }
                        if c2 == '\x1b' {
                            chars.next(); // consume the `\` of ST
                            break;
                        }
                    }
                }
                // Two-character escape (ESC c, ESC 7, …)
                Some(_) => {
                    chars.next();
                }
                None => {}
            }
            continue;
        }
        if c == '\n' || c == '\t' {
            out.push(if keep_lines { c } else { ' ' });
        } else if c == '\r' {
            if !keep_lines {
                out.push(' ');
            }
        } else if !c.is_control() {
            out.push(c);
        }
    }
    out
}

/// Display text for an untrusted scalar string.
fn sanitize(s: &str) -> String {
    strip_escapes(s, true)
}

/// Single-line, escape-free cell/key text for tables and records.
fn cell_text(s: &str) -> String {
    strip_escapes(s, false)
}

/// Display text for an untrusted diagnostic line (`ctx.log` / `ctx.err`).
///
/// Unicode bidi/formatting characters (e.g. U+202E RIGHT-TO-LEFT OVERRIDE)
/// are deliberately left in place: `char::is_control()` doesn't catch them,
/// and stripping them would corrupt legitimate Arabic/Hebrew text, which is
/// a worse failure than the Trojan-Source-style visual reordering a hostile
/// string could otherwise cause here.
///
/// Single-line and escape-free: a command's diagnostic is page-controlled
/// text, so it must not be able to move the cursor, clear the screen, or
/// inject colour codes into the styling the sink applies around it.
pub fn diagnostic_text(s: &str) -> String {
    cell_text(s)
}

/// Display text for a command's *raw* write (`ctx.log.write`), which is
/// allowed a narrow set of control sequences that `diagnostic_text` strips.
///
/// The rule that makes this safe: **no permitted sequence moves the cursor
/// upward or to an absolute position.** `\n` *is* permitted, so a raw write
/// is not confined to one line — a command can open new lines below itself.
/// What it cannot do is climb back: nothing here moves up, positions the
/// cursor absolutely, clears the screen, reads the cursor back (that injects
/// into the input stream), or touches OSC. So a hostile command can only
/// garble lines it opened itself, which it can already do with plain text;
/// output written before it — the prompt, an earlier command — is out of
/// reach.
///
/// SGR is permitted because it cannot move anything. Its containment comes
/// from `PaneSink` reset-prefixing *every* record (see `engine.rs`), not from
/// any reset at the command boundary — there is no such reset, and there
/// could not usefully be one: `Abortable::poll` returns without resuming a
/// suspended command, so boundary cleanup never runs on Ctrl-C, which is
/// exactly the case a command that sets a colour and then hangs would
/// exploit. Per-record reset holds even then.
pub fn writer_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            match chars.peek() {
                Some('[') => {
                    chars.next();
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
                    // `C`/`D` cannot leave the line, `K` erases within it,
                    // and `m` is not spatial at all. The digit guard rejects
                    // private modes like `\x1b[?25l`.
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
                // body, as `strip_escapes` does — a partial body left behind
                // is visible litter.
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

/// The ordered union of record keys across `rows`, first-seen order.
/// Non-`Record` rows contribute nothing (they render as blank cells).
pub(crate) fn table_columns(rows: &[Value]) -> Vec<String> {
    let mut columns: IndexSet<String> = IndexSet::new();
    for row in rows {
        if let Value::Record(map) = row {
            for k in map.keys() {
                columns.insert(k.clone());
            }
        }
    }
    columns.into_iter().collect()
}

/// Text and numeric-ness of one row's `col` cell (escape-stripped; numbers
/// right-align downstream).
fn cell(row: &Value, col: &str) -> (String, bool) {
    match row {
        Value::Record(map) => match map.get(col) {
            Some(v) => (cell_text(&plain(v)), matches!(v, Value::Int(_) | Value::Float(_))),
            None => (String::new(), false),
        },
        _ => (String::new(), false),
    }
}

fn truncate(s: &str, max: usize) -> String {
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
}

fn rule(widths: &[usize], left: &str, mid: &str, right: &str) -> String {
    let mut s = String::from(left);
    for (i, w) in widths.iter().enumerate() {
        s.push_str(&"─".repeat(w + 2));
        s.push_str(if i + 1 == widths.len() { right } else { mid });
    }
    s.push('\n');
    s
}

/// Final display widths for `cols` given these sample rows: natural widths
/// (header vs cells), shrunk to fit `width` (with a `MIN_COL_WIDTH` floor).
/// Called once against the probe rows so a streamed row later reuses the
/// exact same widths a full-list render would have produced.
pub(crate) fn column_widths(cols: &[String], rows: &[Value], width: u16) -> Vec<usize> {
    // Display names are sanitized; `cols` keeps raw keys for cell lookup.
    let headers: Vec<String> = cols.iter().map(|c| cell_text(c)).collect();

    // Natural widths.
    let mut widths: Vec<usize> = cols
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
    let overhead = 3 * cols.len() + 1; // "│ " per column + trailing "│" and padding
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
    widths
}

/// Top border + header row + separator, at fixed `widths`.
pub(crate) fn table_header(cols: &[String], widths: &[usize]) -> String {
    let headers: Vec<String> = cols.iter().map(|c| cell_text(c)).collect();
    let mut out = String::new();
    out.push_str(&rule(widths, "┌", "┬", "┐"));
    out.push('│');
    for (c, w) in headers.iter().zip(widths) {
        let text = truncate(c, *w);
        let pad = " ".repeat(w - UnicodeWidthStr::width(text.as_str()));
        out.push_str(&format!(" {BOLD}{text}{RESET}{pad} │"));
    }
    out.push('\n');
    out.push_str(&rule(widths, "├", "┼", "┤"));
    out
}

/// One data row at fixed `widths`: over-wide cells truncate with `…`,
/// numbers right-align.
pub(crate) fn table_row(row: &Value, cols: &[String], widths: &[usize]) -> String {
    let mut out = String::new();
    out.push('│');
    for (c, w) in cols.iter().zip(widths) {
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
    out
}

/// Bottom border at fixed `widths`.
pub(crate) fn table_bottom(widths: &[usize]) -> String {
    rule(widths, "└", "┴", "┘")
}

/// Box-drawn table for a `List` of `Record`s. Column set is the union of
/// keys in first-seen order. The widest column shrinks (with `…`) to fit the
/// width budget; numbers right-align; header is bold.
fn render_table(rows: &[Value], width: u16) -> String {
    let cols = table_columns(rows);
    if cols.is_empty() {
        return format!("{DIM}({} empty records){RESET}\n", rows.len());
    }
    let widths = column_widths(&cols, rows, width);
    let mut out = table_header(&cols, &widths);
    for row in rows {
        out.push_str(&table_row(row, &cols, &widths));
    }
    out.push_str(&table_bottom(&widths));
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
    fn whole_escape_sequences_go_not_just_the_esc_byte() {
        // Regression: stripping only ESC left the body behind, so help text
        // rendered as literal `[1mUsage:[0m` litter.
        let v = Value::Str("\x1b[1mbold\x1b[0m plain".into());
        let out = strip_ansi(&render(&v, 80));
        assert_eq!(out, "bold plain\n", "no leftover [1m litter: {out:?}");
    }

    #[test]
    fn osc_and_two_char_escapes_are_stripped() {
        // OSC can set the window title / clipboard in some terminals.
        let v = Value::Str("a\x1b]0;pwned\x07b\x1bcc".into());
        let out = strip_ansi(&render(&v, 80));
        assert_eq!(out, "abc\n", "got {out:?}");
    }

    #[test]
    fn dcs_apc_pm_sos_bodies_are_fully_dropped() {
        // These string-type escapes take an arbitrary-length body (e.g. a
        // Sixel image in a DCS). The old generic two-char-escape branch only
        // ate the introducer, leaving the payload as visible garbage.
        let v = Value::Str("a\x1bPsixel-junk\x1b\\b\x1b_apc-junk\x07c\x1b^pm-junk\x1b\\d\x1bXsos-junk\x1b\\e".into());
        let out = strip_ansi(&render(&v, 80));
        assert_eq!(out, "abcde\n", "got {out:?}");
    }

    #[test]
    fn escapes_in_table_cells_do_not_skew_column_width() {
        // A colored cell must not count its escape bytes as visible width.
        let v = Value::List(vec![Value::record([
            ("a".to_string(), Value::Str("\x1b[31mred\x1b[0m".into())),
        ])]);
        let out = strip_ansi(&render(&v, 80));
        let widths: Vec<usize> = out
            .lines()
            .map(UnicodeWidthStr::width)
            .collect();
        assert!(
            widths.windows(2).all(|w| w[0] == w[1]),
            "table rows must line up: {widths:?}\n{out}"
        );
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

    #[test]
    fn diagnostic_text_is_stripped_to_one_line() {
        // A page-controlled diagnostic must not be able to clear the screen
        // or smuggle colour codes into our styling.
        let hostile = "\x1b[2J\x1b[Hcleared\nsecond line";
        assert_eq!(diagnostic_text(hostile), "cleared second line");
    }

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
        // Private-mode sequences (hide cursor) are not on the allowlist.
        assert_eq!(writer_text("\x1b[?25lhidden"), "hidden");
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

    #[test]
    fn writer_text_handles_unterminated_and_malformed_escapes() {
        // An unterminated CSI consumes to the end rather than emitting a
        // partial sequence that a terminal might complete with later text.
        assert_eq!(writer_text("a\x1b[31"), "a");
        assert_eq!(writer_text("a\x1b["), "a");
        // An unterminated string escape swallows its body (no litter).
        assert_eq!(writer_text("a\x1b]0;never-ends"), "a");
        // An SGR with a huge/odd param list is still just styling.
        assert_eq!(writer_text("\x1b[1;38;5;196mx\x1b[0m"), "\x1b[1;38;5;196mx\x1b[0m");
    }
}
