//! Spans, shell errors, and the caret diagnostic renderer.

use serde::{Deserialize, Serialize};
use unicode_width::UnicodeWidthStr;

/// Byte range into the source line. Attached to every token, AST node, and
/// (optionally) error.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}

impl Span {
    pub fn new(start: u32, end: u32) -> Self {
        Span { start, end }
    }

    pub fn merge(self, other: Span) -> Span {
        Span {
            start: self.start.min(other.start),
            end: self.end.max(other.end),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ErrorKind {
    Parse,
    UnknownCommand,
    Binding,
    Type,
    Runtime,
    Aborted,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShellError {
    pub kind: ErrorKind,
    pub msg: String,
    pub span: Option<Span>,
    pub help: Option<String>,
}

impl ShellError {
    pub fn new(kind: ErrorKind, msg: impl Into<String>) -> Self {
        ShellError {
            kind,
            msg: msg.into(),
            span: None,
            help: None,
        }
    }

    pub fn parse(msg: impl Into<String>, span: Span) -> Self {
        ShellError::new(ErrorKind::Parse, msg).with_span(span)
    }

    pub fn runtime(msg: impl Into<String>) -> Self {
        ShellError::new(ErrorKind::Runtime, msg)
    }

    pub fn type_error(msg: impl Into<String>) -> Self {
        ShellError::new(ErrorKind::Type, msg)
    }

    pub fn with_span(mut self, span: Span) -> Self {
        self.span = Some(span);
        self
    }

    pub fn with_help(mut self, help: impl Into<String>) -> Self {
        self.help = Some(help.into());
        self
    }

    /// Render the error against its source line, caret-style:
    ///
    /// ```text
    /// error: unknown flag `--dpth`
    ///   ❯ ls --dpth 2
    ///        ^^^^^^
    /// help: did you mean `--depth`?
    /// ```
    ///
    /// Uses ANSI colors (red label/carets, dim help). Lines end with `\n`.
    pub fn render(&self, source: &str) -> String {
        const RED: &str = "\x1b[31m";
        const BOLD_RED: &str = "\x1b[1;31m";
        const DIM: &str = "\x1b[2m";
        const RESET: &str = "\x1b[0m";

        let mut out = format!("{BOLD_RED}error:{RESET} {}\n", self.msg);
        if let Some(span) = self.span {
            let start = (span.start as usize).min(source.len());
            let end = (span.end as usize).min(source.len()).max(start);
            let prefix = &source[..start];
            let spanned = &source[start..end];
            let pad = " ".repeat(UnicodeWidthStr::width(prefix));
            let carets = "^".repeat(UnicodeWidthStr::width(spanned).max(1));
            out.push_str(&format!("  {DIM}❯{RESET} {source}\n"));
            out.push_str(&format!("    {pad}{RED}{carets}{RESET}\n"));
        }
        if let Some(help) = &self.help {
            out.push_str(&format!("{DIM}help: {help}{RESET}\n"));
        }
        out
    }
}

/// Edit distance for "did you mean" suggestions.
pub fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut curr = vec![0; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        curr[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            curr[j + 1] = (prev[j + 1] + 1).min(curr[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[b.len()]
}

/// Best "did you mean" candidate: closest name within a sane edit distance.
pub fn did_you_mean<'a>(target: &str, candidates: impl Iterator<Item = &'a str>) -> Option<String> {
    candidates
        .map(|c| (levenshtein(target, c), c))
        .filter(|(d, c)| *d <= 2.max(c.len() / 3))
        .min_by_key(|(d, _)| *d)
        .map(|(_, c)| c.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levenshtein_basics() {
        assert_eq!(levenshtein("depth", "dpth"), 1);
        assert_eq!(levenshtein("", "abc"), 3);
        assert_eq!(levenshtein("same", "same"), 0);
    }

    #[test]
    fn did_you_mean_picks_closest() {
        let names = ["first", "last", "length"];
        assert_eq!(
            did_you_mean("frist", names.iter().copied()),
            Some("first".to_string())
        );
        assert_eq!(did_you_mean("zzzzzz", names.iter().copied()), None);
    }

    #[test]
    fn render_points_at_span() {
        let err = ShellError::parse("unknown flag `--dpth`", Span::new(3, 9))
            .with_help("did you mean `--depth`?");
        let rendered = err.render("ls --dpth 2");
        assert!(rendered.contains("error:"));
        assert!(rendered.contains("ls --dpth 2"));
        assert!(rendered.contains("^^^^^^"));
        assert!(rendered.contains("did you mean `--depth`?"));
    }
}
