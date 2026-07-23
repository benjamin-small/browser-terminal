//! Command signatures, argument binding, coercion, and help generation.

use crate::ast::{Arg, Call, Expr};
use crate::error::{did_you_mean, ErrorKind, ShellError, Span};
use crate::lex::InterpPart;
use crate::value::Value;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::rc::Rc;

/// Declared type of a positional arg or flag value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Shape {
    Any,
    Str,
    Int,
    Float,
    Bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PosArg {
    pub name: String,
    #[serde(default = "default_shape")]
    pub shape: Shape,
    #[serde(default)]
    pub desc: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FlagSpec {
    pub long: String,
    #[serde(default)]
    pub short: Option<char>,
    /// `None` = switch (present → true); `Some(shape)` = takes a value.
    #[serde(default)]
    pub shape: Option<Shape>,
    #[serde(default)]
    pub desc: String,
}

fn default_shape() -> Shape {
    Shape::Any
}

/// Nushell-shaped command signature. TS command authors supply the same
/// structure (all collection fields optional there). Unknown fields are
/// rejected so a TS author's typo (`flag` vs `flags`, `type` vs `shape`)
/// errors loudly instead of silently degrading the command.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Signature {
    pub name: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub required: Vec<PosArg>,
    #[serde(default)]
    pub optional: Vec<PosArg>,
    #[serde(default)]
    pub rest: Option<PosArg>,
    #[serde(default)]
    pub flags: Vec<FlagSpec>,
}

impl Signature {
    pub fn build(name: impl Into<String>, summary: impl Into<String>) -> Self {
        Signature {
            name: name.into(),
            summary: summary.into(),
            required: vec![],
            optional: vec![],
            rest: None,
            flags: vec![],
        }
    }

    pub fn required_arg(mut self, name: &str, shape: Shape, desc: &str) -> Self {
        self.required.push(PosArg { name: name.into(), shape, desc: desc.into() });
        self
    }

    pub fn optional_arg(mut self, name: &str, shape: Shape, desc: &str) -> Self {
        self.optional.push(PosArg { name: name.into(), shape, desc: desc.into() });
        self
    }

    pub fn rest_arg(mut self, name: &str, shape: Shape, desc: &str) -> Self {
        self.rest = Some(PosArg { name: name.into(), shape, desc: desc.into() });
        self
    }

    pub fn flag(mut self, long: &str, short: Option<char>, shape: Option<Shape>, desc: &str) -> Self {
        self.flags.push(FlagSpec { long: long.into(), short, shape, desc: desc.into() });
        self
    }

    /// Declare the common `--on` parameter: "operate on *this part* of each
    /// item." Its meaning is identical everywhere it appears — a field name,
    /// a dotted path, inline source, or `@registered` — so it is learned
    /// once. Commands where it would be meaningless simply don't declare it
    /// and reject it with the usual unknown-flag diagnostic, rather than
    /// accepting it and silently doing nothing.
    pub fn on_selector(self, desc: &str) -> Self {
        self.flag("on", None, Some(Shape::Str), desc)
    }

    /// `help <name>` / `--help` text.
    pub fn render_help(&self) -> String {
        const BOLD: &str = "\x1b[1m";
        const DIM: &str = "\x1b[2m";
        const CYAN: &str = "\x1b[36m";
        const RESET: &str = "\x1b[0m";

        let mut usage = format!("{CYAN}{}{RESET}", self.name);
        for p in &self.required {
            usage.push_str(&format!(" <{}>", p.name));
        }
        for p in &self.optional {
            usage.push_str(&format!(" [{}]", p.name));
        }
        if let Some(rest) = &self.rest {
            usage.push_str(&format!(" [{}...]", rest.name));
        }
        if !self.flags.is_empty() {
            usage.push_str(" [flags]");
        }

        let mut out = String::new();
        out.push_str(&format!("{}\n\n", self.summary));
        out.push_str(&format!("{BOLD}Usage:{RESET} {usage}\n"));

        let positionals: Vec<&PosArg> = self
            .required
            .iter()
            .chain(&self.optional)
            .chain(self.rest.as_ref())
            .collect();
        if !positionals.is_empty() {
            out.push_str(&format!("\n{BOLD}Positionals:{RESET}\n"));
            for p in positionals {
                out.push_str(&format!(
                    "  {CYAN}{:<12}{RESET} {DIM}{:<7}{RESET} {}\n",
                    p.name,
                    format!("{:?}", p.shape).to_lowercase(),
                    p.desc
                ));
            }
        }
        if !self.flags.is_empty() {
            out.push_str(&format!("\n{BOLD}Flags:{RESET}\n"));
            for f in &self.flags {
                let mut names = format!("--{}", f.long);
                if let Some(s) = f.short {
                    names.push_str(&format!(", -{s}"));
                }
                if let Some(shape) = f.shape {
                    names.push_str(&format!(" <{}>", format!("{shape:?}").to_lowercase()));
                }
                out.push_str(&format!("  {CYAN}{names:<24}{RESET} {}\n", f.desc));
            }
        }
        out
    }
}

/// Help for a command *group* — a name like `task` or `mux` that isn't a
/// command itself but prefixes several that are.
///
/// Multi-word names are the subcommand mechanism (`str upcase` is just a
/// name containing a space), which means a group has no signature of its own
/// to render. Without this, `mux` was an unknown command whose did-you-mean
/// pointed at `map`: the shell knew about `mux split` and still couldn't
/// admit that `mux` meant anything.
///
/// `subs` is (full name, summary), sorted by the caller.
pub fn render_group_help(group: &str, subs: &[(String, String)]) -> String {
    const BOLD: &str = "\x1b[1m";
    const DIM: &str = "\x1b[2m";
    const CYAN: &str = "\x1b[36m";
    const RESET: &str = "\x1b[0m";

    let width = subs.iter().map(|(n, _)| n.len()).max().unwrap_or(0).max(12);

    let mut out = format!("`{group}` is a command group.\n\n");
    out.push_str(&format!(
        "{BOLD}Usage:{RESET} {CYAN}{group}{RESET} <subcommand> [args] [flags]\n"
    ));
    out.push_str(&format!("\n{BOLD}Subcommands:{RESET}\n"));
    for (name, summary) in subs {
        out.push_str(&format!("  {CYAN}{name:<width$}{RESET} {summary}\n"));
    }
    out.push_str(&format!(
        "\n{DIM}Run `{group} <subcommand> --help` for a subcommand's own page.{RESET}\n"
    ));
    out
}

/// A call after binding: evaluated, coerced values in place.
#[derive(Clone)]
pub struct BoundCall {
    pub head_span: Span,
    pub positionals: Vec<Value>,
    /// Keyed by long flag name. Switches bind to `Bool(true)`.
    pub flags: HashMap<String, Value>,
    /// Closure literals, keyed by the parameter or flag name they were
    /// bound to. A closure isn't a `Value` — keeping it out of `Value`
    /// leaves the JS boundary and serde derives untouched.
    pub closures: HashMap<String, Rc<dyn crate::callable::HostFn>>,
}

impl std::fmt::Debug for BoundCall {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BoundCall")
            .field("head_span", &self.head_span)
            .field("positionals", &self.positionals)
            .field("flags", &self.flags)
            .field("closures", &self.closures.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl BoundCall {
    pub fn flag(&self, name: &str) -> Option<&Value> {
        self.flags.get(name)
    }

    pub fn has_flag(&self, name: &str) -> bool {
        matches!(self.flags.get(name), Some(Value::Bool(true)))
    }

    /// A closure literal bound to this parameter or flag, if one was given.
    pub fn closure(&self, name: &str) -> Option<Rc<dyn crate::callable::HostFn>> {
        self.closures.get(name).cloned()
    }
}

/// Variable scope for `$var` and `"...$var..."`.
pub type Scope = HashMap<String, Value>;

/// Did the raw call ask for `--help`? Checked before binding so a bad call
/// can still get help.
pub fn wants_help(call: &Call) -> bool {
    call.args.iter().any(|a| matches!(a, Arg::Flag { name, long: true, .. } if name == "help"))
}

/// Evaluate an argument expression to a Value against the scope.
pub fn eval_expr(expr: &Expr, scope: &Scope) -> Result<Value, ShellError> {
    match expr {
        // Operators, field access and closures only appear inside closure
        // bodies; the shared evaluator owns them.
        Expr::Field { .. } | Expr::Binary { .. } | Expr::Unary { .. } | Expr::Closure(..) => {
            crate::expr::eval_expr(expr, scope)
        }
        Expr::Literal(v, _) => Ok(v.clone()),
        // Bare `true`/`false` are boolean literals; quoted 'true' stays a
        // string (a Str-shaped parameter turns the Bool back into text).
        Expr::Bareword(w, _) if w == "true" => Ok(Value::Bool(true)),
        Expr::Bareword(w, _) if w == "false" => Ok(Value::Bool(false)),
        Expr::Bareword(w, _) => Ok(Value::Str(w.clone())),
        Expr::Var(name, span) => scope.get(name).cloned().ok_or_else(|| {
            let e = ShellError::new(ErrorKind::Runtime, format!("unknown variable `${name}`")).with_span(*span);
            match did_you_mean(name, scope.keys().map(|s| s.as_str())) {
                Some(s) => e.with_help(format!("did you mean `${s}`?")),
                None => e,
            }
        }),
        Expr::StrInterp(parts, span) => {
            let mut out = String::new();
            for part in parts {
                match part {
                    InterpPart::Lit(s) => out.push_str(s),
                    InterpPart::Var(name) => match scope.get(name) {
                        Some(v) => out.push_str(&to_display_string(v)),
                        None => {
                            return Err(ShellError::new(
                                ErrorKind::Runtime,
                                format!("unknown variable `${name}`"),
                            )
                            .with_span(*span))
                        }
                    },
                }
            }
            Ok(Value::Str(out))
        }
    }
}

fn to_display_string(v: &Value) -> String {
    match v {
        Value::Str(s) => s.clone(),
        Value::Int(n) => n.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Null => String::new(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

/// Coerce a bound value to the declared shape. Barewords arrive as `Str` and
/// may convert; typed literals must already match (Int upgrades to Float).
fn coerce(value: Value, shape: Shape, at: Span, what: &str) -> Result<Value, ShellError> {
    let fail = |v: &Value| {
        Err(ShellError::new(
            ErrorKind::Type,
            format!("{what} expects {}, found {} ", shape_name(shape), v.type_name()),
        )
        .with_span(at))
    };
    match shape {
        Shape::Any => Ok(value),
        Shape::Str => match value {
            Value::Str(s) => Ok(Value::Str(s)),
            Value::Int(n) => Ok(Value::Str(n.to_string())),
            Value::Float(f) => Ok(Value::Str(f.to_string())),
            Value::Bool(b) => Ok(Value::Str(b.to_string())),
            ref v => fail(v),
        },
        Shape::Int => match value {
            Value::Int(n) => Ok(Value::Int(n)),
            Value::Str(s) => {
                let n = s.parse::<i64>().map_err(|_| {
                    ShellError::new(ErrorKind::Type, format!("{what} expects an int, found `{s}`"))
                        .with_span(at)
                })?;
                // Same 2^53 gate as the lexer — coercion must not be a
                // second door for precision-losing integers.
                if n.unsigned_abs() > crate::value::MAX_SAFE_INT as u64 {
                    return Err(ShellError::new(
                        ErrorKind::Type,
                        format!("{what}: `{s}` exceeds 2^53 and would lose precision in JavaScript"),
                    )
                    .with_span(at));
                }
                Ok(Value::Int(n))
            }
            ref v => fail(v),
        },
        Shape::Float => match value {
            Value::Float(f) => Ok(Value::Float(f)),
            Value::Int(n) => Ok(Value::Float(n as f64)),
            Value::Str(s) => s
                .parse::<f64>()
                .map(Value::Float)
                .map_err(|_| {
                    ShellError::new(ErrorKind::Type, format!("{what} expects a float, found `{s}`"))
                        .with_span(at)
                }),
            ref v => fail(v),
        },
        Shape::Bool => match value {
            Value::Bool(b) => Ok(Value::Bool(b)),
            Value::Str(s) if s == "true" => Ok(Value::Bool(true)),
            Value::Str(s) if s == "false" => Ok(Value::Bool(false)),
            ref v => fail(v),
        },
    }
}

fn shape_name(shape: Shape) -> &'static str {
    match shape {
        Shape::Any => "any value",
        Shape::Str => "a string",
        Shape::Int => "an int",
        Shape::Float => "a float",
        Shape::Bool => "a bool",
    }
}

/// Bind a parsed call against a signature: match flags (with did-you-mean),
/// let value-taking flags without `=` consume the next positional, evaluate
/// and coerce everything, and enforce arity.
///
/// `leading_words` are head barewords left over after command-name
/// resolution; they are bound (in order) before the other positionals.
pub fn bind(
    sig: &Signature,
    leading_words: &[crate::ast::Spanned<String>],
    call: &Call,
    scope: &Scope,
) -> Result<BoundCall, ShellError> {
    let mut positionals: Vec<(Value, Span)> = Vec::new();
    let mut flags: HashMap<String, Value> = HashMap::new();
    let mut closures: HashMap<String, Rc<dyn crate::callable::HostFn>> = HashMap::new();
    // Closure literals can't become Values, so they're pulled aside and
    // matched to parameter names by position after the value args settle.
    let mut positional_closures: Vec<(usize, crate::ast::Closure)> = Vec::new();

    for w in leading_words {
        // Same bareword semantics as eval_expr: bare true/false are bools.
        let v = match w.node.as_str() {
            "true" => Value::Bool(true),
            "false" => Value::Bool(false),
            _ => Value::Str(w.node.clone()),
        };
        positionals.push((v, w.span));
    }

    let mut args = call.args.iter().peekable();
    while let Some(arg) = args.next() {
        match arg {
            Arg::Positional(expr) => {
                if let Some(c) = expr.as_closure() {
                    // Occupies a positional slot so arity still lines up;
                    // the placeholder is never handed to a command.
                    positional_closures.push((positionals.len(), c.clone()));
                    positionals.push((Value::Null, expr.span()));
                } else {
                    positionals.push((eval_expr(expr, scope)?, expr.span()));
                }
            }
            Arg::Flag { name, long, span, value } => {
                let spec = find_flag(sig, name, *long, *span)?;
                let long_name = spec.long.clone();
                match spec.shape {
                    None => {
                        if value.is_some() {
                            return Err(ShellError::new(
                                ErrorKind::Binding,
                                format!("`--{long_name}` is a switch and takes no value"),
                            )
                            .with_span(*span));
                        }
                        flags.insert(long_name, Value::Bool(true));
                    }
                    Some(shape) => {
                        // A closure given to a value-taking flag (e.g.
                        // `--on {|x| …}`) is stored by flag name instead.
                        let closure_arg = match value {
                            Some(expr) => expr.as_closure().cloned(),
                            None => match args.peek() {
                                Some(Arg::Positional(expr)) => expr.as_closure().cloned(),
                                _ => None,
                            },
                        };
                        if let Some(c) = closure_arg {
                            if value.is_none() {
                                args.next();
                            }
                            closures.insert(
                                long_name.clone(),
                                crate::expr::NativeClosure::new(c, scope.clone()),
                            );
                            flags.insert(long_name, Value::Null);
                            continue;
                        }
                        let (raw, vspan) = match value {
                            Some(expr) => (eval_expr(expr, scope)?, expr.span()),
                            None => match args.peek() {
                                Some(Arg::Positional(expr)) => {
                                    let expr = expr.clone();
                                    args.next();
                                    (eval_expr(&expr, scope)?, expr.span())
                                }
                                _ => {
                                    return Err(ShellError::new(
                                        ErrorKind::Binding,
                                        format!("flag `--{long_name}` needs a value"),
                                    )
                                    .with_span(*span))
                                }
                            },
                        };
                        let coerced = coerce(raw, shape, vspan, &format!("`--{long_name}`"))?;
                        flags.insert(long_name, coerced);
                    }
                }
            }
        }
    }

    // Arity + shape check on positionals. Slots holding a closure skip
    // coercion — a closure has no Value form to coerce.
    let declared: Vec<&PosArg> = sig.required.iter().chain(&sig.optional).collect();
    let closure_slots: HashMap<usize, crate::ast::Closure> =
        positional_closures.into_iter().collect();
    let mut bound = Vec::new();
    let mut supplied = positionals.into_iter().enumerate();
    for (idx, spec) in declared.iter().enumerate() {
        match supplied.next() {
            Some((slot, _)) if closure_slots.contains_key(&slot) => {
                let c = closure_slots[&slot].clone();
                closures.insert(
                    spec.name.clone(),
                    crate::expr::NativeClosure::new(c, scope.clone()),
                );
                bound.push(Value::Null);
            }
            Some((_, (v, span))) => {
                bound.push(coerce(v, spec.shape, span, &format!("`{}`", spec.name))?)
            }
            None if idx < sig.required.len() => {
                return Err(ShellError::new(
                    ErrorKind::Binding,
                    format!("missing required argument `{}`", spec.name),
                )
                .with_span(call.words_span())
                .with_help(format!("run `{} --help` for usage", sig.name)));
            }
            None => break,
        }
    }
    let leftovers: Vec<(Value, Span)> = supplied.map(|(_, pair)| pair).collect();
    if !leftovers.is_empty() {
        match &sig.rest {
            Some(rest) => {
                for (v, span) in leftovers {
                    bound.push(coerce(v, rest.shape, span, &format!("`{}`", rest.name))?);
                }
            }
            None => {
                let span = leftovers[0].1;
                return Err(ShellError::new(
                    ErrorKind::Binding,
                    format!("`{}` takes at most {} positional arguments", sig.name, declared.len()),
                )
                .with_span(span)
                .with_help(format!("run `{} --help` for usage", sig.name)));
            }
        }
    }

    Ok(BoundCall { head_span: call.words_span(), positionals: bound, flags, closures })
}

fn find_flag<'a>(sig: &'a Signature, name: &str, long: bool, span: Span) -> Result<&'a FlagSpec, ShellError> {
    let found = if long {
        sig.flags.iter().find(|f| f.long == name)
    } else {
        let c = name.chars().next();
        sig.flags.iter().find(|f| f.short.is_some() && f.short == c)
    };
    found.ok_or_else(|| {
        let display = if long { format!("--{name}") } else { format!("-{name}") };
        let e = ShellError::new(ErrorKind::Binding, format!("unknown flag `{display}`")).with_span(span);
        match did_you_mean(name, sig.flags.iter().map(|f| f.long.as_str())) {
            Some(s) => e.with_help(format!("did you mean `--{s}`?")),
            None if sig.flags.is_empty() => e.with_help(format!("`{}` takes no flags", sig.name)),
            None => e,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::parse;

    fn sig() -> Signature {
        Signature::build("links", "List links")
            .optional_arg("pattern", Shape::Str, "substring filter")
            .flag("limit", Some('l'), Some(Shape::Int), "max results")
            .flag("all", Some('a'), None, "include empty links")
    }

    fn call(src: &str) -> Call {
        let out = parse(src);
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        out.line.pipelines[0].calls[0].clone()
    }

    fn bind_src(src: &str) -> Result<BoundCall, ShellError> {
        let c = call(src);
        bind(&sig(), &c.words[1..], &c, &Scope::new())
    }

    #[test]
    fn binds_positionals_flags_and_switches() {
        let b = bind_src("links docs --limit 5 --all").expect("bind");
        assert_eq!(b.positionals, vec![Value::Str("docs".into())]);
        assert_eq!(b.flag("limit"), Some(&Value::Int(5)));
        assert!(b.has_flag("all"));
    }

    #[test]
    fn eq_and_space_flag_values_equivalent() {
        let a = bind_src("links --limit=7").expect("bind");
        let b = bind_src("links --limit 7").expect("bind");
        assert_eq!(a.flag("limit"), b.flag("limit"));
    }

    #[test]
    fn short_flag_resolves_to_long_name() {
        let b = bind_src("links -l 3").expect("bind");
        assert_eq!(b.flag("limit"), Some(&Value::Int(3)));
    }

    #[test]
    fn unknown_flag_suggests() {
        let err = bind_src("links --limt 3").expect_err("should fail");
        assert!(err.msg.contains("unknown flag"));
        assert_eq!(err.help.as_deref(), Some("did you mean `--limit`?"));
    }

    #[test]
    fn wrong_type_points_at_value_span() {
        let err = bind_src("links --limit five").expect_err("should fail");
        assert!(err.msg.contains("expects an int"), "{}", err.msg);
        assert!(err.span.is_some());
    }

    #[test]
    fn extra_positionals_error_without_rest() {
        let err = bind_src("links a b").expect_err("should fail");
        assert!(err.msg.contains("at most"), "{}", err.msg);
    }

    #[test]
    fn missing_required_errors() {
        let s = Signature::build("get", "Get a column").required_arg("column", Shape::Str, "column name");
        let c = call("get");
        let err = bind(&s, &c.words[1..], &c, &Scope::new()).expect_err("should fail");
        assert!(err.msg.contains("missing required argument `column`"));
    }

    #[test]
    fn vars_and_interpolation() {
        let mut scope = Scope::new();
        scope.insert("name".into(), Value::Str("world".into()));
        let c = call(r#"links "hi $name" --limit $name"#);
        let err = bind(&sig(), &c.words[1..], &c, &scope).expect_err("limit should reject non-int");
        assert!(err.msg.contains("expects an int"));

        let c = call(r#"links "hi $name""#);
        let b = bind(&sig(), &c.words[1..], &c, &scope).expect("bind");
        assert_eq!(b.positionals, vec![Value::Str("hi world".into())]);
    }

    #[test]
    fn help_renders_usage() {
        let text = sig().render_help();
        assert!(text.contains("Usage:"));
        assert!(text.contains("--limit"));
        assert!(text.contains("[pattern]"));
    }
}
