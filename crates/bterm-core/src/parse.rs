//! Recursive-descent parser with recovery: on error it records a diagnostic
//! and syncs to the next `|` or `;`, so one line can produce multiple
//! diagnostics. Evaluation only proceeds when there are no diagnostics.

use crate::ast::{Arg, BinOp, Call, Closure, Expr, Line, Pipeline, Spanned, UnOp};
use crate::error::{ShellError, Span};
use crate::lex::{lex, Op, Token, TokenKind};
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

    /// An argument at pipeline level: a plain value or a closure literal.
    /// Operators are *not* valid here — only inside a closure body — so
    /// they produce the same "not supported here" diagnostic as before.
    fn parse_expr(&mut self) -> Result<Expr, ShellError> {
        if let Some(tok) = self.peek() {
            if tok.kind == TokenKind::Op(Op::LBrace) {
                return self.parse_closure();
            }
        }
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
            TokenKind::Op(op) => Err(ShellError::parse(
                format!("`{}` can only be used inside a closure", op.as_str()),
                tok.span,
            )
            .with_help("write a closure like `{|x| $x.n > 5}`")),
            other => Err(ShellError::parse(
                format!("expected a value, found {}", describe(&other)),
                tok.span,
            )),
        }
    }

    /// `{|x| <expr>}` — parameters between pipes, then one expression.
    fn parse_closure(&mut self) -> Result<Expr, ShellError> {
        let open = self.next().expect("caller peeked `{`").span;
        let mut params = Vec::new();

        match self.peek().map(|t| t.kind.clone()) {
            Some(TokenKind::Pipe) => {
                self.next();
                // Params run until the closing pipe. `||` lexes as OrOr, so
                // an empty list is written `{|| …}` and arrives as one token.
                loop {
                    match self.next() {
                        Some(Token { kind: TokenKind::Pipe, .. }) => break,
                        Some(Token { kind: TokenKind::Bareword(name), .. }) => params.push(name),
                        Some(Token { kind: TokenKind::Var(name), .. }) => params.push(name),
                        Some(tok) => {
                            return Err(ShellError::parse(
                                format!("expected a parameter name, found {}", describe(&tok.kind)),
                                tok.span,
                            ))
                        }
                        None => {
                            return Err(ShellError::parse(
                                "unterminated closure parameters: expected a closing `|`",
                                open,
                            ))
                        }
                    }
                }
            }
            Some(TokenKind::Op(Op::OrOr)) => {
                self.next(); // `||` — no parameters
            }
            _ => {
                return Err(ShellError::parse(
                    "a closure needs parameters: `{|x| …}`",
                    open,
                )
                .with_help("use `{|| …}` for a closure that takes nothing"))
            }
        }

        let body = self.parse_operator_expr(0)?;
        match self.next() {
            Some(Token { kind: TokenKind::Op(Op::RBrace), span }) => Ok(Expr::Closure(
                Box::new(Closure { params, body }),
                open.merge(span),
            )),
            Some(tok) => Err(ShellError::parse(
                format!("expected `}}` to close the closure, found {}", describe(&tok.kind)),
                tok.span,
            )),
            None => Err(ShellError::parse(
                "unterminated closure: expected a closing `}`",
                open,
            )),
        }
    }

    /// Precedence climbing. Binding powers, loosest first:
    /// `||` < `&&` < comparison < `+ -` < `* / %` < unary.
    fn parse_operator_expr(&mut self, min_bp: u8) -> Result<Expr, ShellError> {
        let mut lhs = self.parse_unary()?;
        while let Some(tok) = self.peek() {
            let TokenKind::Op(op) = tok.kind else { break };
            let Some((bin, bp)) = binop_of(op) else { break };
            if bp < min_bp {
                break;
            }
            self.next();
            // Left-associative: the right side must bind strictly tighter.
            let rhs = self.parse_operator_expr(bp + 1)?;
            let span = lhs.span().merge(rhs.span());
            lhs = Expr::Binary { op: bin, lhs: Box::new(lhs), rhs: Box::new(rhs), span };
        }
        Ok(lhs)
    }

    fn parse_unary(&mut self) -> Result<Expr, ShellError> {
        if let Some(tok) = self.peek().cloned() {
            let un = match tok.kind {
                TokenKind::Op(Op::Bang) => Some(UnOp::Not),
                TokenKind::Op(Op::Minus) => Some(UnOp::Neg),
                _ => None,
            };
            if let Some(op) = un {
                self.next();
                let operand = self.parse_unary()?;
                let span = tok.span.merge(operand.span());
                return Ok(Expr::Unary { op, operand: Box::new(operand), span });
            }
        }
        self.parse_postfix()
    }

    /// A primary followed by any number of `.field` accessors.
    fn parse_postfix(&mut self) -> Result<Expr, ShellError> {
        let mut expr = self.parse_primary()?;
        // The lexer already folds `.` into barewords, so `$x.a.b` arrives as
        // Var("x") followed by Bareword(".a.b").
        while let Some(Token { kind: TokenKind::Bareword(w), span }) = self.peek().cloned() {
            if !w.starts_with('.') {
                break;
            }
            self.next();
            let path: Vec<String> = w
                .trim_start_matches('.')
                .split('.')
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .collect();
            if path.is_empty() {
                return Err(ShellError::parse("expected a field name after `.`", span));
            }
            let full = expr.span().merge(span);
            expr = Expr::Field { base: Box::new(expr), path, span: full };
        }
        Ok(expr)
    }

    fn parse_primary(&mut self) -> Result<Expr, ShellError> {
        let tok = self
            .next()
            .ok_or_else(|| ShellError::parse("expected a value", Span::new(0, 0)))?;
        match tok.kind {
            TokenKind::Int(n) => Ok(Expr::Literal(Value::Int(n), tok.span)),
            TokenKind::Float(f) => Ok(Expr::Literal(Value::Float(f), tok.span)),
            TokenKind::StrRaw(s) => Ok(Expr::Literal(Value::Str(s), tok.span)),
            TokenKind::StrInterp(parts) => Ok(Expr::StrInterp(parts, tok.span)),
            TokenKind::Var(name) => Ok(Expr::Var(name, tok.span)),
            TokenKind::Bareword(w) => Ok(match w.as_str() {
                "true" => Expr::Literal(Value::Bool(true), tok.span),
                "false" => Expr::Literal(Value::Bool(false), tok.span),
                "null" => Expr::Literal(Value::Null, tok.span),
                _ => Expr::Bareword(w, tok.span),
            }),
            TokenKind::Op(Op::LParen) => {
                let inner = self.parse_operator_expr(0)?;
                match self.next() {
                    Some(Token { kind: TokenKind::Op(Op::RParen), .. }) => Ok(inner),
                    Some(t) => Err(ShellError::parse(
                        format!("expected `)`, found {}", describe(&t.kind)),
                        t.span,
                    )),
                    None => Err(ShellError::parse("expected `)`", tok.span)),
                }
            }
            TokenKind::Op(Op::Assign) => Err(ShellError::parse(
                "`=` is not an operator here",
                tok.span,
            )
            .with_help("use `==` to compare")),
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

/// Operator → (AST op, binding power). `None` for grouping/`!`/`=`, which
/// are never infix.
fn binop_of(op: Op) -> Option<(BinOp, u8)> {
    Some(match op {
        Op::OrOr => (BinOp::Or, 1),
        Op::AndAnd => (BinOp::And, 2),
        Op::EqEq => (BinOp::Eq, 3),
        Op::Ne => (BinOp::Ne, 3),
        Op::Lt => (BinOp::Lt, 3),
        Op::Le => (BinOp::Le, 3),
        Op::Gt => (BinOp::Gt, 3),
        Op::Ge => (BinOp::Ge, 3),
        Op::Plus => (BinOp::Add, 4),
        Op::Minus => (BinOp::Sub, 4),
        Op::Star => (BinOp::Mul, 5),
        Op::Slash => (BinOp::Div, 5),
        Op::Percent => (BinOp::Rem, 5),
        _ => return None,
    })
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
        TokenKind::Op(op) => format!("`{}`", op.as_str()),
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
        let line = ok("links --limit 20 | filter {|o| $o.text != ''} | head 5");
        assert_eq!(line.pipelines.len(), 1);
        let calls = &line.pipelines[0].calls;
        assert_eq!(calls.len(), 3);
        assert_eq!(calls[0].words[0].node, "links");
        // `filter {closure}` → head `filter` + one positional closure arg.
        assert_eq!(calls[1].words[0].node, "filter");
        assert_eq!(calls[1].args.len(), 1);
        assert!(matches!(&calls[1].args[0], Arg::Positional(e) if e.as_closure().is_some()));
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
    fn operators_outside_a_closure_point_at_closures() {
        // Arithmetic at pipeline level isn't supported, but the error now
        // tells you where operators *do* work rather than saying "reserved".
        let out = parse("echo (1 + 2)");
        assert_eq!(out.errors.len(), 1);
        assert!(out.errors[0].msg.contains("only be used inside a closure"), "{}", out.errors[0].msg);
        assert!(out.errors[0].help.as_deref().unwrap_or("").contains("{|x|"));
    }

    #[test]
    fn still_reserved_syntax_reports_not_supported() {
        let out = parse("echo & background");
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
