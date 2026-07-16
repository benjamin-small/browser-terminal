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
    /// Syntax fenced off for v2: `( ) { } > < & && | | (as ||)` etc.
    Reserved(String),
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

fn is_var_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '-'
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
                if rest.starts_with("||") {
                    tokens.push(Token { kind: TokenKind::Reserved("||".into()), span: Span::new(start, start + 2) });
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
                let len = if rest.starts_with("&&") { 2 } else { 1 };
                tokens.push(Token { kind: TokenKind::Reserved(rest[..len].into()), span: Span::new(start, start + len as u32) });
                i += len;
            }
            '(' | ')' | '{' | '}' | '>' | '<' | '=' => {
                tokens.push(Token { kind: TokenKind::Reserved(c.to_string()), span: Span::new(start, start + 1) });
                i += 1;
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
                if rest.starts_with("--") {
                    let name: String = rest[2..].chars().take_while(|&ch| is_var_char(ch)).collect();
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
                    // Short flag: single letter only; `-v2.1`-style barewords
                    // stay barewords.
                    let word: String = rest[1..].chars().take_while(|&ch| is_bareword_char(ch)).collect();
                    if word.chars().count() == 1 {
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
                    // Lone `-` or `-<punct>`: bareword.
                    let word: String = rest.chars().take_while(|&ch| is_bareword_char(ch) || ch == '-').collect();
                    let len = word.len().max(1);
                    tokens.push(Token {
                        kind: TokenKind::Bareword(rest[..len].to_string()),
                        span: Span::new(start, start + len as u32),
                    });
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
                tokens.push(Token {
                    kind: TokenKind::Bareword(word.clone()),
                    span: Span::new(start, start + word.len() as u32),
                });
                i += word.len();
            }
        }
        let _ = bytes; // spans are byte offsets; bytes kept for clarity
    }
    Ok(tokens)
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
        TokenKind::Float(f)
    } else {
        let n: i64 = text
            .parse()
            .map_err(|_| ShellError::parse(format!("integer `{text}` is out of range"), span))?;
        if n.abs() > MAX_SAFE_INT {
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
                    .take_while(|&c2| is_var_char(c2) && c2 != '-')
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
            kinds("links | first 5"),
            vec![
                TokenKind::Bareword("links".into()),
                TokenKind::Pipe,
                TokenKind::Bareword("first".into()),
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
    fn reserved_tokens() {
        assert_eq!(kinds("("), vec![TokenKind::Reserved("(".into())]);
        assert_eq!(kinds("&&"), vec![TokenKind::Reserved("&&".into())]);
        assert_eq!(kinds("||"), vec![TokenKind::Reserved("||".into())]);
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
    fn spans_are_byte_accurate() {
        let toks = lex("echo 'hi'").expect("lex ok");
        assert_eq!(toks[0].span, Span::new(0, 4));
        assert_eq!(toks[1].span, Span::new(5, 9));
    }
}
