//! Per-pane line editor. Pure: `feed(input) -> Effects` — no I/O, no engine
//! access. The caller writes `Effects::echo` to the terminal in the same
//! tick and spawns evaluation for each submitted line.

use serde::Serialize;
use unicode_width::UnicodeWidthChar;

const MAX_HISTORY: usize = 200;
/// Visible width of the prompt (`❯ `).
const PROMPT_WIDTH: usize = 2;

const PASTE_START: &str = "\x1b[200~";
const PASTE_END: &str = "\x1b[201~";

/// What a `feed` call produced. `echo` goes to the terminal immediately;
/// each entry in `submitted` becomes its own evaluation task; `ctrl_c` asks
/// the engine to abort the pane's running pipeline (if any).
///
/// A `completion_request` field is reserved here as the v2 tab-completion
/// hook.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Effects {
    pub echo: String,
    pub submitted: Vec<String>,
    pub ctrl_c: bool,
}

pub struct LineEditor {
    buffer: Vec<char>,
    cursor: usize,
    history: Vec<String>,
    /// Some(i) while navigating history; the in-progress line is stashed.
    hist_idx: Option<usize>,
    stashed: Vec<char>,
    /// Carry-over for an escape sequence split across input chunks.
    pending: String,
    in_paste: bool,
    paste_buf: String,
    last_ok: bool,
}

impl Default for LineEditor {
    fn default() -> Self {
        Self::new()
    }
}

impl LineEditor {
    pub fn new() -> Self {
        LineEditor {
            buffer: Vec::new(),
            cursor: 0,
            history: Vec::new(),
            hist_idx: None,
            stashed: Vec::new(),
            pending: String::new(),
            in_paste: false,
            paste_buf: String::new(),
            last_ok: true,
        }
    }

    /// Color the prompt by the last command's status.
    pub fn set_last_status(&mut self, ok: bool) {
        self.last_ok = ok;
    }

    pub fn history(&self) -> &[String] {
        &self.history
    }

    pub fn prompt(&self) -> String {
        if self.last_ok {
            "\x1b[32m❯\x1b[0m ".to_string()
        } else {
            "\x1b[31m❯\x1b[0m ".to_string()
        }
    }

    /// Redraw the whole input line: CR, clear, prompt, buffer, cursor.
    pub fn prompt_line(&self) -> String {
        let mut out = format!("\r\x1b[K{}", self.prompt());
        let text: String = self.buffer.iter().collect();
        out.push_str(&text);
        let tail_width: usize = self.buffer[self.cursor..]
            .iter()
            .map(|c| UnicodeWidthChar::width(*c).unwrap_or(0))
            .sum();
        if tail_width > 0 {
            out.push_str(&format!("\x1b[{tail_width}D"));
        }
        let _ = PROMPT_WIDTH; // prompt width is constant; kept for cursor math extensions
        out
    }

    pub fn push_history(&mut self, line: &str) {
        if line.trim().is_empty() {
            return;
        }
        if self.history.last().map(String::as_str) == Some(line) {
            return;
        }
        self.history.push(line.to_string());
        if self.history.len() > MAX_HISTORY {
            self.history.remove(0);
        }
    }

    pub fn feed(&mut self, input: &str) -> Effects {
        let mut fx = Effects::default();
        let data = format!("{}{}", std::mem::take(&mut self.pending), input);
        let mut rest = data.as_str();

        while !rest.is_empty() {
            if self.in_paste {
                match rest.find(PASTE_END) {
                    Some(pos) => {
                        self.paste_buf.push_str(&rest[..pos]);
                        rest = &rest[pos + PASTE_END.len()..];
                        self.in_paste = false;
                        let payload = std::mem::take(&mut self.paste_buf);
                        self.insert_paste(&payload, &mut fx);
                    }
                    None => {
                        // Might end with a partial terminator — keep a tail back.
                        let keep = partial_suffix_len(rest, PASTE_END);
                        self.paste_buf.push_str(&rest[..rest.len() - keep]);
                        self.pending = rest[rest.len() - keep..].to_string();
                        rest = "";
                    }
                }
                continue;
            }

            let c = match rest.chars().next() {
                Some(c) => c,
                None => break,
            };

            match c {
                '\x1b' => match parse_escape(rest) {
                    EscParse::Incomplete => {
                        self.pending = rest.to_string();
                        rest = "";
                    }
                    EscParse::Parsed(seq, len) => {
                        rest = &rest[len..];
                        self.apply_escape(seq, &mut fx);
                    }
                },
                '\r' | '\n' => {
                    rest = &rest[1..];
                    // Swallow the LF of a CRLF pair.
                    if c == '\r' && rest.starts_with('\n') {
                        rest = &rest[1..];
                    }
                    self.submit(&mut fx);
                }
                '\x7f' | '\x08' => {
                    rest = &rest[1..];
                    if self.cursor > 0 {
                        self.cursor -= 1;
                        self.buffer.remove(self.cursor);
                        fx.echo.push_str(&self.prompt_line());
                    }
                }
                '\x03' => {
                    // Ctrl-C: cancel the edit line; the engine also aborts a
                    // running pipeline when it sees `ctrl_c`.
                    rest = &rest[1..];
                    fx.ctrl_c = true;
                    self.buffer.clear();
                    self.cursor = 0;
                    self.hist_idx = None;
                    fx.echo.push_str("^C\r\n");
                    fx.echo.push_str(&self.prompt_line());
                }
                '\x01' => {
                    rest = &rest[1..];
                    self.cursor = 0;
                    fx.echo.push_str(&self.prompt_line());
                }
                '\x05' => {
                    rest = &rest[1..];
                    self.cursor = self.buffer.len();
                    fx.echo.push_str(&self.prompt_line());
                }
                '\x15' => {
                    // C-u: kill to line start.
                    rest = &rest[1..];
                    self.buffer.drain(..self.cursor);
                    self.cursor = 0;
                    fx.echo.push_str(&self.prompt_line());
                }
                '\x0b' => {
                    // C-k: kill to line end.
                    rest = &rest[1..];
                    self.buffer.truncate(self.cursor);
                    fx.echo.push_str(&self.prompt_line());
                }
                '\x17' => {
                    // C-w: delete the word before the cursor.
                    rest = &rest[1..];
                    let mut start = self.cursor;
                    while start > 0 && self.buffer[start - 1] == ' ' {
                        start -= 1;
                    }
                    while start > 0 && self.buffer[start - 1] != ' ' {
                        start -= 1;
                    }
                    self.buffer.drain(start..self.cursor);
                    self.cursor = start;
                    fx.echo.push_str(&self.prompt_line());
                }
                '\x0c' => {
                    // C-l: clear screen, redraw the input line at the top.
                    rest = &rest[1..];
                    fx.echo.push_str("\x1b[2J\x1b[H");
                    fx.echo.push_str(&self.prompt_line());
                }
                _ if !c.is_control() => {
                    rest = &rest[c.len_utf8()..];
                    self.buffer.insert(self.cursor, c);
                    self.cursor += 1;
                    if self.cursor == self.buffer.len() {
                        // Fast path: appending at the end echoes just the char.
                        fx.echo.push(c);
                    } else {
                        fx.echo.push_str(&self.prompt_line());
                    }
                }
                _ => {
                    // Unhandled control char: swallow.
                    rest = &rest[c.len_utf8()..];
                }
            }
        }
        fx
    }

    fn submit(&mut self, fx: &mut Effects) {
        let line: String = self.buffer.iter().collect();
        self.buffer.clear();
        self.cursor = 0;
        self.hist_idx = None;
        fx.echo.push_str("\r\n");
        self.push_history(&line);
        fx.submitted.push(line);
    }

    /// Bracketed paste payload: inserted literally, newlines become single
    /// spaces — pasted text can never auto-execute.
    fn insert_paste(&mut self, payload: &str, fx: &mut Effects) {
        let cleaned: String = payload
            .replace("\r\n", " ")
            .replace(['\r', '\n'], " ")
            .chars()
            .filter(|c| !c.is_control())
            .collect();
        for c in cleaned.chars() {
            self.buffer.insert(self.cursor, c);
            self.cursor += 1;
        }
        fx.echo.push_str(&self.prompt_line());
    }

    fn apply_escape(&mut self, seq: Escape, fx: &mut Effects) {
        match seq {
            Escape::Up => {
                if self.history.is_empty() {
                    return;
                }
                let idx = match self.hist_idx {
                    None => {
                        self.stashed = std::mem::take(&mut self.buffer);
                        self.history.len() - 1
                    }
                    Some(0) => 0,
                    Some(i) => i - 1,
                };
                self.hist_idx = Some(idx);
                self.buffer = self.history[idx].chars().collect();
                self.cursor = self.buffer.len();
                fx.echo.push_str(&self.prompt_line());
            }
            Escape::Down => {
                let Some(idx) = self.hist_idx else { return };
                if idx + 1 < self.history.len() {
                    self.hist_idx = Some(idx + 1);
                    self.buffer = self.history[idx + 1].chars().collect();
                } else {
                    self.hist_idx = None;
                    self.buffer = std::mem::take(&mut self.stashed);
                }
                self.cursor = self.buffer.len();
                fx.echo.push_str(&self.prompt_line());
            }
            Escape::Left => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                    fx.echo.push_str(&self.prompt_line());
                }
            }
            Escape::Right => {
                if self.cursor < self.buffer.len() {
                    self.cursor += 1;
                    fx.echo.push_str(&self.prompt_line());
                }
            }
            Escape::Home => {
                self.cursor = 0;
                fx.echo.push_str(&self.prompt_line());
            }
            Escape::End => {
                self.cursor = self.buffer.len();
                fx.echo.push_str(&self.prompt_line());
            }
            Escape::Delete => {
                if self.cursor < self.buffer.len() {
                    self.buffer.remove(self.cursor);
                    fx.echo.push_str(&self.prompt_line());
                }
            }
            Escape::PasteStart => {
                self.in_paste = true;
                self.paste_buf.clear();
            }
            Escape::Ignored => {}
        }
    }
}

enum Escape {
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    Delete,
    PasteStart,
    Ignored,
}

enum EscParse {
    /// The chunk ends mid-sequence; stash it for the next feed.
    Incomplete,
    /// Parsed (or safely ignorable) sequence of the given byte length.
    Parsed(Escape, usize),
}

/// Parse one escape sequence at the start of `s` (which begins with ESC).
fn parse_escape(s: &str) -> EscParse {
    let bytes = s.as_bytes();
    if bytes.len() == 1 {
        return EscParse::Incomplete;
    }
    if bytes[1] != b'[' {
        // ESC + one char (alt-key etc.): swallow both.
        let len = 1 + s[1..].chars().next().map(char::len_utf8).unwrap_or(0);
        return EscParse::Parsed(Escape::Ignored, len);
    }
    // CSI: ESC [ params final — params are digits/;/?, final is 0x40..=0x7e.
    let mut i = 2;
    while i < bytes.len() {
        let b = bytes[i];
        if (0x40..=0x7e).contains(&b) {
            let seq = &s[..=i];
            let escape = match seq {
                "\x1b[A" => Escape::Up,
                "\x1b[B" => Escape::Down,
                "\x1b[C" => Escape::Right,
                "\x1b[D" => Escape::Left,
                "\x1b[H" | "\x1b[1~" | "\x1b[7~" => Escape::Home,
                "\x1b[F" | "\x1b[4~" | "\x1b[8~" => Escape::End,
                "\x1b[3~" => Escape::Delete,
                PASTE_START => Escape::PasteStart,
                _ => Escape::Ignored,
            };
            return EscParse::Parsed(escape, i + 1);
        }
        if !(b.is_ascii_digit() || b == b';' || b == b'?') {
            // Not a CSI we understand; swallow ESC [ and this byte.
            return EscParse::Parsed(Escape::Ignored, i + 1);
        }
        i += 1;
    }
    EscParse::Incomplete
}

/// Length of the longest suffix of `s` that is a proper prefix of `needle`.
fn partial_suffix_len(s: &str, needle: &str) -> usize {
    let max = needle.len().saturating_sub(1).min(s.len());
    for take in (1..=max).rev() {
        if needle.starts_with(&s[s.len() - take..]) {
            return take;
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strip_ansi(s: &str) -> String {
        let mut out = String::new();
        let mut chars = s.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                if chars.peek() == Some(&'[') {
                    chars.next();
                    for c2 in chars.by_ref() {
                        if (0x40..=0x7e).contains(&(c2 as u32)) {
                            break;
                        }
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    fn line(ed: &LineEditor) -> String {
        // Rebuild the visible line from a redraw.
        strip_ansi(&ed.prompt_line()).replace('\r', "")
    }

    #[test]
    fn typing_appends_and_submits() {
        let mut ed = LineEditor::new();
        let fx = ed.feed("echo hi");
        assert_eq!(fx.echo, "echo hi");
        assert!(fx.submitted.is_empty());
        let fx = ed.feed("\r");
        assert_eq!(fx.submitted, vec!["echo hi".to_string()]);
        assert!(fx.echo.contains("\r\n"));
        assert_eq!(ed.history(), &["echo hi".to_string()]);
    }

    #[test]
    fn backspace_and_cursor_edit() {
        let mut ed = LineEditor::new();
        ed.feed("hxi");
        ed.feed("\x1b[D"); // left, cursor between x and i
        ed.feed("\x7f"); // delete the x
        let fx = ed.feed("\r");
        assert_eq!(fx.submitted, vec!["hi".to_string()]);
    }

    #[test]
    fn insert_mid_line_redraws() {
        let mut ed = LineEditor::new();
        ed.feed("ac");
        ed.feed("\x1b[D");
        let fx = ed.feed("b");
        assert!(fx.echo.contains("abc"), "redraw shows full line: {:?}", fx.echo);
        let fx = ed.feed("\r");
        assert_eq!(fx.submitted, vec!["abc".to_string()]);
    }

    #[test]
    fn home_end_ctrl_a_e() {
        let mut ed = LineEditor::new();
        ed.feed("world");
        ed.feed("\x01"); // C-a
        ed.feed("hello ");
        ed.feed("\x05"); // C-e
        ed.feed("!");
        let fx = ed.feed("\r");
        assert_eq!(fx.submitted, vec!["hello world!".to_string()]);
    }

    #[test]
    fn kill_line_and_word() {
        let mut ed = LineEditor::new();
        ed.feed("one two three");
        ed.feed("\x17"); // C-w kills "three"
        let fx = ed.feed("\r");
        assert_eq!(fx.submitted, vec!["one two ".to_string()]);

        let mut ed = LineEditor::new();
        ed.feed("scrap this");
        ed.feed("\x15"); // C-u kills everything before cursor
        let fx = ed.feed("\r");
        assert_eq!(fx.submitted, vec!["".to_string()]);
    }

    #[test]
    fn history_navigation_with_stash() {
        let mut ed = LineEditor::new();
        ed.feed("first\r");
        ed.feed("second\r");
        ed.feed("draft");
        ed.feed("\x1b[A"); // -> second
        assert_eq!(line(&ed), "❯ second");
        ed.feed("\x1b[A"); // -> first
        assert_eq!(line(&ed), "❯ first");
        ed.feed("\x1b[B"); // -> second
        ed.feed("\x1b[B"); // -> draft restored
        assert_eq!(line(&ed), "❯ draft");
    }

    #[test]
    fn duplicate_history_collapsed_and_capped() {
        let mut ed = LineEditor::new();
        ed.feed("same\r");
        ed.feed("same\r");
        assert_eq!(ed.history().len(), 1);
        for i in 0..250 {
            ed.feed(&format!("cmd {i}\r"));
        }
        assert_eq!(ed.history().len(), 200);
        assert_eq!(ed.history()[0], "cmd 50");
    }

    #[test]
    fn ctrl_c_cancels_line_and_flags() {
        let mut ed = LineEditor::new();
        ed.feed("doomed");
        let fx = ed.feed("\x03");
        assert!(fx.ctrl_c);
        assert!(fx.echo.contains("^C"));
        let fx = ed.feed("\r");
        assert_eq!(fx.submitted, vec!["".to_string()]);
    }

    #[test]
    fn bracketed_paste_inserts_without_executing() {
        let mut ed = LineEditor::new();
        let fx = ed.feed("\x1b[200~rm -rf /\nrm again\x1b[201~");
        assert!(fx.submitted.is_empty(), "paste must not submit");
        let fx = ed.feed("\r");
        assert_eq!(fx.submitted, vec!["rm -rf / rm again".to_string()]);
    }

    #[test]
    fn bracketed_paste_split_across_chunks() {
        let mut ed = LineEditor::new();
        let fx = ed.feed("\x1b[200~part one ");
        assert!(fx.submitted.is_empty());
        let fx = ed.feed("part two\x1b[201");
        assert!(fx.submitted.is_empty());
        let fx = ed.feed("~");
        assert!(fx.submitted.is_empty());
        let fx = ed.feed("\r");
        assert_eq!(fx.submitted, vec!["part one part two".to_string()]);
    }

    #[test]
    fn split_escape_sequence_across_chunks() {
        let mut ed = LineEditor::new();
        ed.feed("ab");
        ed.feed("\x1b");
        ed.feed("[D"); // completes Left
        ed.feed("X");
        let fx = ed.feed("\r");
        assert_eq!(fx.submitted, vec!["aXb".to_string()]);
    }

    #[test]
    fn crlf_is_one_submit() {
        let mut ed = LineEditor::new();
        let fx = ed.feed("hi\r\n");
        assert_eq!(fx.submitted, vec!["hi".to_string()]);
    }

    #[test]
    fn multiple_submits_in_one_chunk() {
        let mut ed = LineEditor::new();
        let fx = ed.feed("a\rb\r");
        assert_eq!(fx.submitted, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn wide_chars_cursor_math() {
        let mut ed = LineEditor::new();
        ed.feed("日本");
        ed.feed("\x1b[D");
        let redraw = ed.prompt_line();
        // Tail is one double-width char → cursor moves back 2 columns.
        assert!(redraw.ends_with("\x1b[2D"), "got {redraw:?}");
    }

    #[test]
    fn prompt_color_tracks_status() {
        let mut ed = LineEditor::new();
        assert!(ed.prompt().contains("32m"));
        ed.set_last_status(false);
        assert!(ed.prompt().contains("31m"));
    }
}
