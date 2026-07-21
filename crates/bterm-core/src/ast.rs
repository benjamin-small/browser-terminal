//! AST for the shell language. Expression-based so cell paths and
//! subexpressions can slot in post-v1.

use crate::error::Span;
use crate::lex::InterpPart;
use crate::value::Value;

#[derive(Clone, Debug, PartialEq)]
pub struct Spanned<T> {
    pub node: T,
    pub span: Span,
}

/// One submitted line: `;`-separated pipelines.
#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub pipelines: Vec<Pipeline>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Pipeline {
    pub calls: Vec<Call>,
    pub span: Span,
}

/// A command call. `words` is the maximal leading run of barewords; how many
/// of them form the command name is decided at eval time by longest-prefix
/// registry lookup (the rest become leading positional args).
#[derive(Clone, Debug, PartialEq)]
pub struct Call {
    pub words: Vec<Spanned<String>>,
    pub args: Vec<Arg>,
    pub span: Span,
}

impl Call {
    /// Span covering just the command words (used for error reporting).
    pub fn words_span(&self) -> Span {
        match (self.words.first(), self.words.last()) {
            (Some(a), Some(b)) => a.span.merge(b.span),
            _ => self.span,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Arg {
    Positional(Expr),
    Flag {
        name: String,
        long: bool,
        span: Span,
        /// Present only for `--name=value`. Space-separated values are
        /// resolved at bind time using the signature (a flag with a shape
        /// consumes the next positional).
        value: Option<Expr>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum Expr {
    /// Int/Float/Str-from-raw-quotes literal.
    Literal(Value, Span),
    /// Unquoted word in argument position; coerced against the declared
    /// shape at bind time (`5` stays what the lexer made it; `true` can
    /// become Bool; anything else becomes Str).
    Bareword(String, Span),
    /// `"text $var"` — resolved against the scope at bind time.
    StrInterp(Vec<InterpPart>, Span),
    /// `$var`
    Var(String, Span),
    /// `$x.field.nested` — field access on any expression.
    Field { base: Box<Expr>, path: Vec<String>, span: Span },
    /// `a + b`, `a > b`, `a && b`, …
    Binary { op: BinOp, lhs: Box<Expr>, rhs: Box<Expr>, span: Span },
    /// `-a`, `!a`
    Unary { op: UnOp, operand: Box<Expr>, span: Span },
    /// `{|x| expr}` — a closure literal. Only valid where a signature
    /// declares `Shape::Callable`; bind turns it into a callable.
    Closure(Box<Closure>, Span),
}

/// A closure literal's parameters and body.
#[derive(Clone, Debug, PartialEq)]
pub struct Closure {
    pub params: Vec<String>,
    pub body: Expr,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Not,
}

impl Expr {
    pub fn span(&self) -> Span {
        match self {
            Expr::Literal(_, s)
            | Expr::Bareword(_, s)
            | Expr::StrInterp(_, s)
            | Expr::Var(_, s)
            | Expr::Field { span: s, .. }
            | Expr::Binary { span: s, .. }
            | Expr::Unary { span: s, .. }
            | Expr::Closure(_, s) => *s,
        }
    }

    /// Closure literals are the only expression that can't be reduced to a
    /// `Value`; binding routes them separately.
    pub fn as_closure(&self) -> Option<&Closure> {
        match self {
            Expr::Closure(c, _) => Some(c),
            _ => None,
        }
    }
}
