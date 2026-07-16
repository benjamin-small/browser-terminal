//! Recursive-descent parser with recovery: on error it records a diagnostic
//! and syncs to the next `|` or `;`, so one line can produce multiple
//! diagnostics. Evaluation only proceeds when there are no diagnostics.

use crate::ast::{Arg, Call, Expr, Line, Pipeline, Spanned};
use crate::error::{ShellError, Span};
use crate::lex::{lex, Token, TokenKind};
use crate::value::Value;

pub struct ParseOutcome {
    pub line: Line,
    pub errors: Vec<ShellError>,
}

pub fn parse(src: &str) -> ParseOutcome {
    let tokens = match lex(src) {
        Ok(t) => t,
        Err(e) => {
            return ParseOutcome {
                line: Line { pipelines: vec![] },
                errors: vec![e],
            }
        }
    };
    Parser { tokens, pos: 0, errors: Vec::new() }.parse_line(src)
}

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
    errors: Vec<ShellError>,
}

impl Parser {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn next(&mut self) -> Option<Token> {
        let t = self.tokens.get(self.pos).cloned();
        if t.is_some() {
            self.pos += 1;
        }
        t
    }

    fn parse_line(mut self, src: &str) -> ParseOutcome {
        let mut pipelines = Vec::new();
        while self.peek().is_some() {
            if matches!(self.peek().map(|t| &t.kind), Some(TokenKind::Semi)) {
                self.next();
                continue;
            }
            if let Some(p) = self.parse_pipeline() {
                pipelines.push(p);
            }
        }
        let _ = src;
        ParseOutcome {
            line: Line { pipelines },
            errors: self.errors,
        }
    }

    /// Returns None if every call in the pipeline failed to parse.
    fn parse_pipeline(&mut self) -> Option<Pipeline> {
        let mut calls = Vec::new();
        loop {
            match self.parse_call() {
                Ok(call) => calls.push(call),
                Err(e) => {
                    self.errors.push(e);
                    self.sync();
                }
            }
            match self.peek().map(|t| t.kind.clone()) {
                Some(TokenKind::Pipe) => {
                    self.next();
                }
                Some(TokenKind::Semi) | None => break,
                Some(_) => break, // parse_call stopped at something it couldn't eat; sync happened
            }
        }
        let span = match (calls.first(), calls.last()) {
            (Some(a), Some(b)) => a.span.merge(b.span),
            _ => return None,
        };
        Some(Pipeline { calls, span })
    }

    fn parse_call(&mut self) -> Result<Call, ShellError> {
        // Head: maximal leading run of barewords.
        let mut words: Vec<Spanned<String>> = Vec::new();
        while let Some(tok) = self.peek() {
            if let TokenKind::Bareword(w) = &tok.kind {
                words.push(Spanned { node: w.clone(), span: tok.span });
                self.next();
            } else {
                break;
            }
        }
        if words.is_empty() {
            let (msg, span) = match self.peek() {
                Some(tok) => (
                    match &tok.kind {
                        TokenKind::Reserved(r) => format!("`{r}` is not supported yet"),
                        other => format!("expected a command, found {}", describe(other)),
                    },
                    tok.span,
                ),
                None => ("expected a command".to_string(), Span::new(0, 0)),
            };
            return Err(ShellError::parse(msg, span));
        }

        let mut span = words[0].span.merge(words[words.len() - 1].span);
        let mut args = Vec::new();
        while let Some(tok) = self.peek().cloned() {
            match tok.kind {
                TokenKind::Pipe | TokenKind::Semi => break,
                TokenKind::Reserved(r) => {
                    return Err(ShellError::parse(format!("`{r}` is not supported yet"), tok.span)
                        .with_help("this syntax is reserved for a future version"));
                }
                TokenKind::Flag { name, long, has_eq } => {
                    self.next();
                    let value = if has_eq {
                        Some(self.parse_expr().map_err(|e| {
                            e.with_help(format!("`--{name}=` needs a value, e.g. `--{name}=5`"))
                        })?)
                    } else {
                        None
                    };
                    let fspan = match &value {
                        Some(v) => tok.span.merge(v.span()),
                        None => tok.span,
                    };
                    span = span.merge(fspan);
                    args.push(Arg::Flag { name, long, span: fspan, value });
                }
                _ => {
                    let expr = self.parse_expr()?;
                    span = span.merge(expr.span());
                    args.push(Arg::Positional(expr));
                }
            }
        }
        Ok(Call { words, args, span })
    }

    fn parse_expr(&mut self) -> Result<Expr, ShellError> {
        let tok = self
            .next()
            .ok_or_else(|| ShellError::parse("expected a value", Span::new(0, 0)))?;
        match tok.kind {
            TokenKind::Int(n) => Ok(Expr::Literal(Value::Int(n), tok.span)),
            TokenKind::Float(f) => Ok(Expr::Literal(Value::Float(f), tok.span)),
            TokenKind::StrRaw(s) => Ok(Expr::Literal(Value::Str(s), tok.span)),
            TokenKind::StrInterp(parts) => Ok(Expr::StrInterp(parts, tok.span)),
            TokenKind::Var(name) => Ok(Expr::Var(name, tok.span)),
            TokenKind::Bareword(w) => Ok(Expr::Bareword(w, tok.span)),
            TokenKind::Reserved(r) => Err(ShellError::parse(
                format!("`{r}` is not supported yet"),
                tok.span,
            )),
            other => Err(ShellError::parse(
                format!("expected a value, found {}", describe(&other)),
                tok.span,
            )),
        }
    }

    /// Skip to the next `|` or `;` so later parts of the line still parse.
    fn sync(&mut self) {
        while let Some(tok) = self.peek() {
            if matches!(tok.kind, TokenKind::Pipe | TokenKind::Semi) {
                break;
            }
            self.next();
        }
    }
}

fn describe(kind: &TokenKind) -> String {
    match kind {
        TokenKind::Bareword(w) => format!("`{w}`"),
        TokenKind::Int(n) => format!("`{n}`"),
        TokenKind::Float(f) => format!("`{f}`"),
        TokenKind::StrRaw(_) | TokenKind::StrInterp(_) => "a string".to_string(),
        TokenKind::Var(v) => format!("`${v}`"),
        TokenKind::Flag { name, .. } => format!("flag `--{name}`"),
        TokenKind::Pipe => "`|`".to_string(),
        TokenKind::Semi => "`;`".to_string(),
        TokenKind::Reserved(r) => format!("`{r}`"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(src: &str) -> Line {
        let out = parse(src);
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        out.line
    }

    #[test]
    fn flagship_demo_line_parses() {
        let line = ok("links --limit 20 | where text ne '' | first 5");
        assert_eq!(line.pipelines.len(), 1);
        let calls = &line.pipelines[0].calls;
        assert_eq!(calls.len(), 3);
        assert_eq!(calls[0].words[0].node, "links");
        // `where text ne ''` → words [where, text, ne] + one positional ''.
        assert_eq!(
            calls[1].words.iter().map(|w| w.node.as_str()).collect::<Vec<_>>(),
            vec!["where", "text", "ne"]
        );
        assert_eq!(calls[1].args.len(), 1);
    }

    #[test]
    fn multi_word_heads_collect_leading_barewords() {
        let line = ok("str upcase hello");
        let call = &line.pipelines[0].calls[0];
        assert_eq!(
            call.words.iter().map(|w| w.node.as_str()).collect::<Vec<_>>(),
            vec!["str", "upcase", "hello"]
        );
    }

    #[test]
    fn semicolons_split_pipelines() {
        let line = ok("echo a; echo b | length");
        assert_eq!(line.pipelines.len(), 2);
        assert_eq!(line.pipelines[1].calls.len(), 2);
    }

    #[test]
    fn flag_with_eq_value() {
        let line = ok("links --limit=20");
        let call = &line.pipelines[0].calls[0];
        match &call.args[0] {
            Arg::Flag { name, value: Some(Expr::Literal(Value::Int(20), _)), .. } => {
                assert_eq!(name, "limit");
            }
            other => panic!("unexpected arg: {other:?}"),
        }
    }

    #[test]
    fn reserved_syntax_reports_not_supported() {
        let out = parse("echo (1 + 2)");
        assert_eq!(out.errors.len(), 1);
        assert!(out.errors[0].msg.contains("not supported yet"));
    }

    #[test]
    fn recovery_syncs_to_next_pipeline() {
        let out = parse("echo ( | length; echo ok");
        assert!(!out.errors.is_empty());
        // The `echo ok` after `;` still parsed (both words are leading
        // barewords; head/positional split happens at eval time).
        assert!(out.line.pipelines.iter().any(|p| p.calls.iter().any(|c| {
            let words: Vec<&str> = c.words.iter().map(|w| w.node.as_str()).collect();
            words == ["echo", "ok"]
        })));
    }

    #[test]
    fn multiple_diagnostics_per_line() {
        let out = parse("echo ( ; echo {");
        assert_eq!(out.errors.len(), 2);
    }
}
