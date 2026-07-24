//! Handwritten lexer. Small token set, shell-quirky rules:
//! `-2` is a number, `-f` is a short flag, `--name` / `--name=value` are long
//! flags, barewords are anything else unquoted. `( ) { } > & &&  || <` lex as
//! reserved tokens the parser rejects with "not yet supported".

use crate::error::{ShellError, Span};
use crate::value::MAX_SAFE_INT;

#[derive(Clone, Debug, PartialEq)]
pub enum TokenKind {
    Bareword(String),
    Int(i64),
    Float(f64),
    /// `'raw string'`
    StrRaw(String),
    /// `"text $var text"` — parts to interpolate at bind time.
    StrInterp(Vec<InterpPart>),
    /// `$name`
    Var(String),
    /// `--name` (long) or `-n` (short). `has_eq` means `--name=value`: the
    /// value expression is the immediately following token.
    Flag { name: String, long: bool, has_eq: bool },
    Pipe,
    Semi,
    /// Operators and grouping. These only mean anything inside a closure
    /// body; at pipeline level the parser rejects them with a spanned
    /// "not supported here", preserving the pre-closure error behavior.
    Op(Op),
    /// Syntax still fenced off for later: `&`, redirection, etc.
    Reserved(String),
}

/// Operator and grouping tokens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    LParen,
    RParen,
    LBrace,
    RBrace,
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    EqEq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    AndAnd,
    OrOr,
    Bang,
    /// A lone `=`, which is never valid on its own — kept as a token so the
    /// parser can say "did you mean `==`?" instead of a generic failure.
    Assign,
}

impl Op {
    pub fn as_str(self) -> &'static str {
        match self {
            Op::LParen => "(",
            Op::RParen => ")",
            Op::LBrace => "{",
            Op::RBrace => "}",
            Op::Plus => "+",
            Op::Minus => "-",
            Op::Star => "*",
            Op::Slash => "/",
            Op::Percent => "%",
            Op::EqEq => "==",
            Op::Ne => "!=",
            Op::Lt => "<",
            Op::Le => "<=",
            Op::Gt => ">",
            Op::Ge => ">=",
            Op::AndAnd => "&&",
            Op::OrOr => "||",
            Op::Bang => "!",
            Op::Assign => "=",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum InterpPart {
    Lit(String),
    Var(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

fn is_bareword_char(c: char) -> bool {
    !c.is_whitespace() && !matches!(c, '|' | ';' | '#' | '\'' | '"' | '$' | '(' | ')' | '{' | '}' | '>' | '<' | '&' | '=')
}

/// Flag names allow '-' (`--starts-with`); variable names do not, so `$a-b`
/// reads as `$a` followed by bareword `-b`, matching interpolation.
fn is_flag_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '-'
}

fn is_var_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

pub fn lex(src: &str) -> Result<Vec<Token>, ShellError> {
    let mut tokens = Vec::new();
    let bytes = src.as_bytes();
    let mut i = 0usize;

    while i < src.len() {
        let rest = &src[i..];
        let c = rest.chars().next().unwrap_or('\0');
        let start = i as u32;

        if c.is_whitespace() {
            i += c.len_utf8();
            continue;
        }
        if c == '#' {
            break; // comment to end of line
        }

        match c {
            '|' => {
                // `||` is an operator; a single `|` stays the pipeline
                // separator. Inside `{|x| …}` the parser reinterprets the
                // single pipes as parameter delimiters.
                if rest.starts_with("||") {
                    tokens.push(Token { kind: TokenKind::Op(Op::OrOr), span: Span::new(start, start + 2) });
                    i += 2;
                } else {
                    tokens.push(Token { kind: TokenKind::Pipe, span: Span::new(start, start + 1) });
                    i += 1;
                }
            }
            ';' => {
                tokens.push(Token { kind: TokenKind::Semi, span: Span::new(start, start + 1) });
                i += 1;
            }
            '&' => {
                if rest.starts_with("&&") {
                    tokens.push(Token { kind: TokenKind::Op(Op::AndAnd), span: Span::new(start, start + 2) });
                    i += 2;
                } else {
                    // Single `&` (background jobs) is still fenced off.
                    tokens.push(Token { kind: TokenKind::Reserved("&".into()), span: Span::new(start, start + 1) });
                    i += 1;
                }
            }
            '(' | ')' | '{' | '}' | '<' | '>' | '=' | '!' => {
                let (op, len) = lex_operator(rest);
                tokens.push(Token { kind: TokenKind::Op(op), span: Span::new(start, start + len as u32) });
                i += len;
            }
            '\'' => {
                let (s, consumed) = lex_raw_string(rest, start)?;
                tokens.push(Token { kind: TokenKind::StrRaw(s), span: Span::new(start, start + consumed as u32) });
                i += consumed;
            }
            '"' => {
                let (parts, consumed) = lex_interp_string(rest, start)?;
                tokens.push(Token { kind: TokenKind::StrInterp(parts), span: Span::new(start, start + consumed as u32) });
                i += consumed;
            }
            '$' => {
                let name: String = rest[1..].chars().take_while(|&ch| is_var_char(ch)).collect();
                if name.is_empty() {
                    return Err(ShellError::parse("expected a variable name after `$`", Span::new(start, start + 1)));
                }
                let len = 1 + name.len();
                tokens.push(Token { kind: TokenKind::Var(name), span: Span::new(start, start + len as u32) });
                i += len;
            }
            '-' => {
                let after = rest[1..].chars().next();
                if let Some(after_dashes) = rest.strip_prefix("--") {
                    let name: String = after_dashes.chars().take_while(|&ch| is_flag_char(ch)).collect();
                    if name.is_empty() {
                        return Err(ShellError::parse("expected a flag name after `--`", Span::new(start, start + 2)));
                    }
                    let mut len = 2 + name.len();
                    let has_eq = rest[len..].starts_with('=');
                    tokens.push(Token {
                        kind: TokenKind::Flag { name, long: true, has_eq },
                        span: Span::new(start, start + len as u32),
                    });
                    if has_eq {
                        len += 1; // skip '='; the value lexes as the next token
                    }
                    i += len;
                } else if after.is_some_and(|ch| ch.is_ascii_digit()) {
                    let (token, consumed) = lex_number(rest, start)?;
                    tokens.push(token);
                    i += consumed;
                } else if after.is_some_and(|ch| ch.is_alphabetic()) {
                    // Short flag or a bundle of them (`-i`, `-iv`). An
                    // all-letters run is a flag cluster that `bind` expands
                    // against the signature; anything with a digit or dot
                    // (`-v2.1`) stays a bareword, since it isn't a flag name.
                    let word: String = rest[1..].chars().take_while(|&ch| is_bareword_char(ch)).collect();
                    if word.chars().all(|ch| ch.is_ascii_alphabetic()) {
                        tokens.push(Token {
                            kind: TokenKind::Flag { name: word.clone(), long: false, has_eq: false },
                            span: Span::new(start, start + 1 + word.len() as u32),
                        });
                        i += 1 + word.len();
                    } else {
                        let len = 1 + word.len();
                        tokens.push(Token {
                            kind: TokenKind::Bareword(rest[..len].to_string()),
                            span: Span::new(start, start + len as u32),
                        });
                        i += len;
                    }
                } else {
                    // Lone `-` or `-<punct>`: bareword, unless it stands
                    // alone, in which case it's subtraction.
                    let word: String = rest.chars().take_while(|&ch| is_bareword_char(ch) || ch == '-').collect();
                    let len = word.len().max(1);
                    let kind = match standalone_operator(&word) {
                        Some(op) => TokenKind::Op(op),
                        None => TokenKind::Bareword(rest[..len].to_string()),
                    };
                    tokens.push(Token { kind, span: Span::new(start, start + len as u32) });
                    i += len;
                }
            }
            _ if c.is_ascii_digit() => {
                let (token, consumed) = lex_number(rest, start)?;
                tokens.push(token);
                i += consumed;
            }
            _ => {
                let word: String = rest.chars().take_while(|&ch| is_bareword_char(ch)).collect();
                debug_assert!(!word.is_empty(), "lexer made no progress at byte {i}");
                let kind = match standalone_operator(&word) {
                    Some(op) => TokenKind::Op(op),
                    None => TokenKind::Bareword(word.clone()),
                };
                tokens.push(Token { kind, span: Span::new(start, start + word.len() as u32) });
                i += word.len();
            }
        }
        let _ = bytes; // spans are byte offsets; bytes kept for clarity
    }
    Ok(tokens)
}

/// Operators and grouping, longest match first.
fn lex_operator(rest: &str) -> (Op, usize) {
    for (text, op) in [
        ("<=", Op::Le),
        (">=", Op::Ge),
        ("==", Op::EqEq),
        ("!=", Op::Ne),
    ] {
        if rest.starts_with(text) {
            return (op, 2);
        }
    }
    let op = match rest.as_bytes().first() {
        Some(b'(') => Op::LParen,
        Some(b')') => Op::RParen,
        Some(b'{') => Op::LBrace,
        Some(b'}') => Op::RBrace,
        Some(b'<') => Op::Lt,
        Some(b'>') => Op::Gt,
        Some(b'=') => Op::Assign,
        _ => Op::Bang,
    };
    (op, 1)
}

/// `+ * / %` are ordinary bareword characters (so `a+b` and `c++` still
/// lex as words); they only become operators when standing alone, which is
/// why closure bodies want spaces around binary operators.
fn standalone_operator(word: &str) -> Option<Op> {
    match word {
        "+" => Some(Op::Plus),
        "-" => Some(Op::Minus),
        "*" => Some(Op::Star),
        "/" => Some(Op::Slash),
        "%" => Some(Op::Percent),
        _ => None,
    }
}

/// Numbers: `123`, `-123`, `1.5`, `-0.25`. A trailing bareword char (e.g.
/// `2x`) makes the whole word a bareword instead.
fn lex_number(rest: &str, start: u32) -> Result<(Token, usize), ShellError> {
    let mut len = 0usize;
    let mut chars = rest.char_indices().peekable();
    if let Some((_, '-')) = chars.peek() {
        chars.next();
        len = 1;
    }
    let mut saw_dot = false;
    let mut digits = 0usize;
    while let Some((idx, ch)) = chars.peek().copied() {
        if ch.is_ascii_digit() {
            digits += 1;
            len = idx + 1;
            chars.next();
        } else if ch == '.' && !saw_dot && rest[idx + 1..].chars().next().is_some_and(|d| d.is_ascii_digit()) {
            saw_dot = true;
            len = idx + 1;
            chars.next();
        } else {
            break;
        }
    }
    debug_assert!(digits > 0);
    // If the number runs straight into bareword chars, it's a bareword.
    if rest[len..].chars().next().is_some_and(is_bareword_char) {
        let word: String = rest.chars().take_while(|&ch| is_bareword_char(ch)).collect();
        let wlen = word.len();
        return Ok((
            Token { kind: TokenKind::Bareword(word), span: Span::new(start, start + wlen as u32) },
            wlen,
        ));
    }
    let text = &rest[..len];
    let span = Span::new(start, start + len as u32);
    let kind = if saw_dot {
        let f: f64 = text
            .parse()
            .map_err(|_| ShellError::parse(format!("invalid number `{text}`"), span))?;
        if !f.is_finite() {
            return Err(ShellError::parse(
                format!("number `{text}` is too large to represent"),
                span,
            ));
        }
        TokenKind::Float(f)
    } else {
        let n: i64 = text
            .parse()
            .map_err(|_| ShellError::parse(format!("integer `{text}` is out of range"), span))?;
        // unsigned_abs: `.abs()` would overflow on i64::MIN.
        if n.unsigned_abs() > MAX_SAFE_INT as u64 {
            return Err(ShellError::parse(
                format!("integer `{text}` exceeds 2^53 and would lose precision in JavaScript"),
                span,
            )
            .with_help("use a string if you need arbitrary precision"));
        }
        TokenKind::Int(n)
    };
    Ok((Token { kind, span }, len))
}

fn lex_raw_string(rest: &str, start: u32) -> Result<(String, usize), ShellError> {
    let inner = &rest[1..];
    match inner.find('\'') {
        Some(end) => Ok((inner[..end].to_string(), end + 2)),
        None => Err(ShellError::parse(
            "unterminated string: expected a closing `'`",
            Span::new(start, start + rest.len() as u32),
        )),
    }
}

/// Double-quoted string with `$var` interpolation and `\"`, `\\`, `\n`, `\t`,
/// `\$` escapes.
fn lex_interp_string(rest: &str, start: u32) -> Result<(Vec<InterpPart>, usize), ShellError> {
    let mut parts = Vec::new();
    let mut lit = String::new();
    let mut chars = rest.char_indices().skip(1).peekable();
    while let Some((idx, ch)) = chars.next() {
        match ch {
            '"' => {
                if !lit.is_empty() {
                    parts.push(InterpPart::Lit(lit));
                }
                return Ok((parts, idx + 1));
            }
            '\\' => match chars.next() {
                Some((_, 'n')) => lit.push('\n'),
                Some((_, 't')) => lit.push('\t'),
                Some((_, '"')) => lit.push('"'),
                Some((_, '\\')) => lit.push('\\'),
                Some((_, '$')) => lit.push('$'),
                Some((i2, other)) => {
                    return Err(ShellError::parse(
                        format!("unknown escape `\\{other}`"),
                        Span::new(start + i2 as u32 - 1, start + i2 as u32 + other.len_utf8() as u32),
                    ))
                }
                None => break,
            },
            '$' => {
                let name: String = rest[idx + 1..]
                    .chars()
                    .take_while(|&c2| is_var_char(c2))
                    .collect();
                if name.is_empty() {
                    lit.push('$');
                } else {
                    if !lit.is_empty() {
                        parts.push(InterpPart::Lit(std::mem::take(&mut lit)));
                    }
                    parts.push(InterpPart::Var(name.clone()));
                    for _ in 0..name.chars().count() {
                        chars.next();
                    }
                }
            }
            other => lit.push(other),
        }
    }
    Err(ShellError::parse(
        "unterminated string: expected a closing `\"`",
        Span::new(start, start + rest.len() as u32),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &str) -> Vec<TokenKind> {
        lex(src).expect("lex ok").into_iter().map(|t| t.kind).collect()
    }

    #[test]
    fn barewords_and_pipes() {
        assert_eq!(
            kinds("links | head 5"),
            vec![
                TokenKind::Bareword("links".into()),
                TokenKind::Pipe,
                TokenKind::Bareword("head".into()),
                TokenKind::Int(5),
            ]
        );
    }

    #[test]
    fn flags_long_short_eq() {
        assert_eq!(
            kinds("ls --limit=20 -a --all"),
            vec![
                TokenKind::Bareword("ls".into()),
                TokenKind::Flag { name: "limit".into(), long: true, has_eq: true },
                TokenKind::Int(20),
                TokenKind::Flag { name: "a".into(), long: false, has_eq: false },
                TokenKind::Flag { name: "all".into(), long: true, has_eq: false },
            ]
        );
    }

    #[test]
    fn negative_number_vs_flag_vs_bareword() {
        assert_eq!(kinds("-2"), vec![TokenKind::Int(-2)]);
        assert_eq!(kinds("-1.5"), vec![TokenKind::Float(-1.5)]);
        assert_eq!(
            kinds("-f"),
            vec![TokenKind::Flag { name: "f".into(), long: false, has_eq: false }]
        );
        assert_eq!(kinds("-v2.1"), vec![TokenKind::Bareword("-v2.1".into())]);
    }

    #[test]
    fn strings_raw_and_interp() {
        assert_eq!(kinds("'a b'"), vec![TokenKind::StrRaw("a b".into())]);
        assert_eq!(
            kinds(r#""hi $name!""#),
            vec![TokenKind::StrInterp(vec![
                InterpPart::Lit("hi ".into()),
                InterpPart::Var("name".into()),
                InterpPart::Lit("!".into()),
            ])]
        );
    }

    #[test]
    fn comments_are_skipped() {
        assert_eq!(kinds("echo hi # rest ignored"), vec![
            TokenKind::Bareword("echo".into()),
            TokenKind::Bareword("hi".into()),
        ]);
    }

    #[test]
    fn operator_tokens() {
        assert_eq!(kinds("("), vec![TokenKind::Op(Op::LParen)]);
        assert_eq!(kinds("&&"), vec![TokenKind::Op(Op::AndAnd)]);
        assert_eq!(kinds("||"), vec![TokenKind::Op(Op::OrOr)]);
        assert_eq!(kinds(">="), vec![TokenKind::Op(Op::Ge)]);
        assert_eq!(kinds("=="), vec![TokenKind::Op(Op::EqEq)]);
        assert_eq!(kinds("!="), vec![TokenKind::Op(Op::Ne)]);
        // A single `&` (background jobs) is still fenced off.
        assert_eq!(kinds("&"), vec![TokenKind::Reserved("&".into())]);
    }

    #[test]
    fn arithmetic_symbols_are_operators_only_when_standalone() {
        assert_eq!(kinds("+"), vec![TokenKind::Op(Op::Plus)]);
        assert_eq!(kinds("*"), vec![TokenKind::Op(Op::Star)]);
        // Embedded in a word they stay part of the bareword, so ordinary
        // arguments like `c++` or `a/b` are unaffected.
        assert_eq!(kinds("c++"), vec![TokenKind::Bareword("c++".into())]);
        assert_eq!(kinds("a/b"), vec![TokenKind::Bareword("a/b".into())]);
    }

    #[test]
    fn closure_braces_and_pipes_lex() {
        assert_eq!(
            kinds("{|x| $x}"),
            vec![
                TokenKind::Op(Op::LBrace),
                TokenKind::Pipe,
                TokenKind::Bareword("x".into()),
                TokenKind::Pipe,
                TokenKind::Var("x".into()),
                TokenKind::Op(Op::RBrace),
            ]
        );
    }

    #[test]
    fn unterminated_string_errors() {
        assert!(lex("'oops").is_err());
        assert!(lex("\"oops").is_err());
    }

    #[test]
    fn huge_int_rejected() {
        assert!(lex("9007199254740993").is_err());
        assert!(lex("9007199254740992").is_ok());
    }

    #[test]
    fn i64_min_rejected_not_panicking() {
        // `.abs()` would overflow on i64::MIN (panic in debug, silent
        // acceptance in release).
        let err = lex("-9223372036854775808").expect_err("must reject");
        assert!(err.msg.contains("2^53"));
    }

    #[test]
    fn overflowing_float_literal_rejected() {
        let huge = format!("1{}0.5", "0".repeat(400));
        assert!(lex(&huge).is_err(), "non-finite float literal must be rejected");
    }

    #[test]
    fn var_names_stop_at_dash_like_interpolation() {
        let toks = kinds("$a-b");
        assert_eq!(toks[0], TokenKind::Var("a".into()));
        // And inside interpolation the same boundary applies.
        assert_eq!(
            kinds(r#""$a-b""#),
            vec![TokenKind::StrInterp(vec![
                InterpPart::Var("a".into()),
                InterpPart::Lit("-b".into()),
            ])]
        );
    }

    #[test]
    fn spans_are_byte_accurate() {
        let toks = lex("echo 'hi'").expect("lex ok");
        assert_eq!(toks[0].span, Span::new(0, 4));
        assert_eq!(toks[1].span, Span::new(5, 9));
    }
}
