//! Evaluating expressions, and wrapping a closure literal as a callable.
//!
//! This is what makes closures work identically everywhere: unlike inline
//! JavaScript, nothing here needs a host engine, so `filter {|x| $x.n > 5}`
//! behaves the same in the browser and in the native CLI — and needs no
//! `eval`, so a strict Content-Security-Policy can't switch it off.

use crate::ast::{BinOp, Closure, Expr, UnOp};
use crate::callable::{is_truthy, HostFn};
use crate::error::ShellError;
use crate::lex::InterpPart;
use crate::signature::Scope;
use crate::value::Value;
use std::rc::Rc;

/// Evaluate an expression against a scope.
pub fn eval_expr(expr: &Expr, scope: &Scope) -> Result<Value, ShellError> {
    match expr {
        Expr::Literal(v, _) => Ok(v.clone()),
        // Inside an expression a bareword is a string; `true`/`false`/`null`
        // were already folded to literals by the parser.
        Expr::Bareword(w, _) => Ok(Value::Str(w.clone())),
        Expr::Var(name, span) => scope.get(name).cloned().ok_or_else(|| {
            let e = ShellError::runtime(format!("unknown variable `${name}`")).with_span(*span);
            match crate::error::did_you_mean(name, scope.keys().map(|s| s.as_str())) {
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
                        Some(v) => out.push_str(&crate::render::plain(v)),
                        None => {
                            return Err(ShellError::runtime(format!("unknown variable `${name}`"))
                                .with_span(*span))
                        }
                    },
                }
            }
            Ok(Value::Str(out))
        }
        Expr::Field { base, path, .. } => {
            let mut current = eval_expr(base, scope)?;
            for key in path {
                // Missing fields are Null rather than an error, matching
                // selector semantics: a ragged row is "no match", not a
                // dead pipeline.
                current = match current {
                    // A real field always wins over the pseudo-field below.
                    Value::Record(ref map) if map.contains_key(key) => {
                        map.get(key).cloned().unwrap_or(Value::Null)
                    }
                    ref v => field_fallback(v, key),
                };
            }
            Ok(current)
        }
        Expr::Unary { op, operand, span } => {
            let v = eval_expr(operand, scope)?;
            match op {
                UnOp::Not => Ok(Value::Bool(!is_truthy(&v))),
                UnOp::Neg => match v {
                    Value::Int(n) => Ok(Value::Int(-n)),
                    Value::Float(f) => Ok(Value::Float(-f)),
                    other => Err(ShellError::type_error(format!(
                        "cannot negate {}",
                        other.type_name()
                    ))
                    .with_span(*span)),
                },
            }
        }
        Expr::Binary { op, lhs, rhs, span } => {
            // Short-circuit before evaluating the right side, so
            // `$x.a != null && $x.a > 5` is safe to write.
            match op {
                BinOp::And => {
                    let l = eval_expr(lhs, scope)?;
                    return if is_truthy(&l) {
                        Ok(Value::Bool(is_truthy(&eval_expr(rhs, scope)?)))
                    } else {
                        Ok(Value::Bool(false))
                    };
                }
                BinOp::Or => {
                    let l = eval_expr(lhs, scope)?;
                    return if is_truthy(&l) {
                        Ok(Value::Bool(true))
                    } else {
                        Ok(Value::Bool(is_truthy(&eval_expr(rhs, scope)?)))
                    };
                }
                _ => {}
            }
            let l = eval_expr(lhs, scope)?;
            let r = eval_expr(rhs, scope)?;
            binary(*op, l, r, *span)
        }
        Expr::Closure(_, span) => Err(ShellError::type_error(
            "a closure is not a value here",
        )
        .with_span(*span)
        .with_help("closures are only accepted where a command takes a function")),
    }
}

/// `.length` is the one pseudo-field, because it is what everyone reaches
/// for on a string or list and its absence is a silent `null` otherwise.
/// Real record fields shadow it.
fn field_fallback(value: &Value, key: &str) -> Value {
    if key != "length" {
        return Value::Null;
    }
    match value {
        Value::Str(s) => Value::Int(s.chars().count() as i64),
        Value::List(items) => Value::Int(items.len() as i64),
        Value::Record(map) => Value::Int(map.len() as i64),
        _ => Value::Null,
    }
}

fn binary(op: BinOp, l: Value, r: Value, span: crate::error::Span) -> Result<Value, ShellError> {
    use BinOp::*;
    match op {
        Eq => Ok(Value::Bool(l.loose_eq(&r))),
        Ne => Ok(Value::Bool(!l.loose_eq(&r))),
        // A missing field compares as false rather than aborting the
        // pipeline — the same "ragged rows just don't match" rule that
        // makes absent fields Null in the first place. Genuinely
        // incomparable *present* values (a string against a number) stay an
        // error, since that is an authoring mistake worth surfacing.
        Lt | Le | Gt | Ge if matches!(l, Value::Null) || matches!(r, Value::Null) => {
            Ok(Value::Bool(false))
        }
        Lt | Le | Gt | Ge => {
            let ord = l.partial_cmp_values(&r).ok_or_else(|| {
                ShellError::type_error(format!(
                    "cannot compare {} with {}",
                    l.type_name(),
                    r.type_name()
                ))
                .with_span(span)
            })?;
            Ok(Value::Bool(match op {
                Lt => ord.is_lt(),
                Le => ord.is_le(),
                Gt => ord.is_gt(),
                _ => ord.is_ge(),
            }))
        }
        // `+` concatenates when either side is a string, as in JavaScript;
        // everything else is numeric.
        Add if matches!(l, Value::Str(_)) || matches!(r, Value::Str(_)) => Ok(Value::Str(format!(
            "{}{}",
            crate::render::plain(&l),
            crate::render::plain(&r)
        ))),
        Add | Sub | Mul | Div | Rem => arithmetic(op, l, r, span),
        And | Or => unreachable!("short-circuited above"),
    }
}

fn arithmetic(op: BinOp, l: Value, r: Value, span: crate::error::Span) -> Result<Value, ShellError> {
    let nums = match (&l, &r) {
        (Value::Int(a), Value::Int(b)) => Some((*a, *b)),
        _ => None,
    };
    // Integer math stays integral (so `head $n` style uses keep working);
    // anything involving a float promotes.
    if let Some((a, b)) = nums {
        let v = match op {
            BinOp::Add => a.checked_add(b),
            BinOp::Sub => a.checked_sub(b),
            BinOp::Mul => a.checked_mul(b),
            BinOp::Div => {
                if b == 0 {
                    return Err(ShellError::runtime("division by zero").with_span(span));
                }
                a.checked_div(b)
            }
            _ => {
                if b == 0 {
                    return Err(ShellError::runtime("division by zero").with_span(span));
                }
                a.checked_rem(b)
            }
        };
        return match v {
            Some(n) => Ok(Value::Int(n)),
            None => Err(ShellError::runtime("integer overflow").with_span(span)),
        };
    }
    let to_f = |v: &Value| match v {
        Value::Int(n) => Some(*n as f64),
        Value::Float(f) => Some(*f),
        _ => None,
    };
    let (a, b) = match (to_f(&l), to_f(&r)) {
        (Some(a), Some(b)) => (a, b),
        _ => {
            return Err(ShellError::type_error(format!(
                "cannot do arithmetic on {} and {}",
                l.type_name(),
                r.type_name()
            ))
            .with_span(span))
        }
    };
    Ok(Value::Float(match op {
        BinOp::Add => a + b,
        BinOp::Sub => a - b,
        BinOp::Mul => a * b,
        BinOp::Div => a / b,
        _ => a % b,
    }))
}

/// A closure literal turned into a callable, so it drops straight into the
/// same slot as an inline JS function or a registered `@name`.
pub struct NativeClosure {
    closure: Closure,
    /// The scope the closure was written in, so it can see session vars.
    captured: Scope,
}

impl NativeClosure {
    /// Returns a trait object rather than `Self` — callers only ever want
    /// it as a `HostFn`, alongside JS functions and registered names.
    #[allow(clippy::new_ret_no_self)]
    pub fn new(closure: Closure, captured: Scope) -> Rc<dyn HostFn> {
        Rc::new(NativeClosure { closure, captured })
    }
}

impl HostFn for NativeClosure {
    fn call(&self, value: &Value) -> Result<Value, String> {
        let mut scope = self.captured.clone();
        // Extra parameters bind to null; the pipeline supplies one item.
        for (i, param) in self.closure.params.iter().enumerate() {
            scope.insert(
                param.clone(),
                if i == 0 { value.clone() } else { Value::Null },
            );
        }
        eval_expr(&self.closure.body, &scope).map_err(|e| e.msg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::parse;

    /// Parse `<line>` and pull out the closure in its first argument.
    fn closure_of(src: &str) -> Closure {
        let out = parse(src);
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        let call = &out.line.pipelines[0].calls[0];
        match &call.args[0] {
            crate::ast::Arg::Positional(e) => e.as_closure().expect("a closure").clone(),
            other => panic!("expected a positional closure, got {other:?}"),
        }
    }

    fn apply(src: &str, item: Value) -> Result<Value, String> {
        NativeClosure::new(closure_of(src), Scope::new()).call(&item)
    }

    fn record(pairs: Vec<(&str, Value)>) -> Value {
        Value::record(pairs.into_iter().map(|(k, v)| (k.to_string(), v)))
    }

    #[test]
    fn comparison_on_a_field() {
        let item = record(vec![("n", Value::Int(7))]);
        assert_eq!(apply("f {|x| $x.n > 5}", item.clone()), Ok(Value::Bool(true)));
        assert_eq!(apply("f {|x| $x.n < 5}", item), Ok(Value::Bool(false)));
    }

    #[test]
    fn precedence_and_grouping() {
        let item = Value::Null;
        // * binds tighter than +
        assert_eq!(apply("f {|x| 2 + 3 * 4}", item.clone()), Ok(Value::Int(14)));
        assert_eq!(apply("f {|x| (2 + 3) * 4}", item.clone()), Ok(Value::Int(20)));
        // comparison binds looser than arithmetic
        assert_eq!(apply("f {|x| 2 + 3 > 4}", item.clone()), Ok(Value::Bool(true)));
        // && binds tighter than ||
        assert_eq!(
            apply("f {|x| false && false || true}", item),
            Ok(Value::Bool(true))
        );
    }

    #[test]
    fn logical_operators_short_circuit() {
        // The right side would fail on a missing field if it were evaluated
        // eagerly against a non-record.
        let item = record(vec![("n", Value::Null)]);
        assert_eq!(
            apply("f {|x| $x.n != null && $x.n > 5}", item),
            Ok(Value::Bool(false))
        );
    }

    #[test]
    fn string_concat_and_numeric_add() {
        let item = record(vec![("a", Value::Str("x".into())), ("n", Value::Int(2))]);
        assert_eq!(
            apply("f {|x| $x.a + $x.n}", item.clone()),
            Ok(Value::Str("x2".into()))
        );
        assert_eq!(apply("f {|x| $x.n + 3}", item), Ok(Value::Int(5)));
    }

    #[test]
    fn unary_not_and_negate() {
        let item = record(vec![("n", Value::Int(3))]);
        assert_eq!(apply("f {|x| !$x.n}", item.clone()), Ok(Value::Bool(false)));
        assert_eq!(apply("f {|x| -$x.n}", item), Ok(Value::Int(-3)));
    }

    #[test]
    fn nested_field_paths_and_missing_fields() {
        let item = record(vec![("u", record(vec![("name", Value::Str("ada".into()))]))]);
        assert_eq!(
            apply("f {|x| $x.u.name}", item.clone()),
            Ok(Value::Str("ada".into()))
        );
        // Missing is Null, not an error.
        assert_eq!(apply("f {|x| $x.nope.deep}", item), Ok(Value::Null));
    }

    #[test]
    fn division_by_zero_and_overflow_are_errors() {
        assert!(apply("f {|x| 1 / 0}", Value::Null).is_err());
        assert!(apply("f {|x| 1 % 0}", Value::Null).is_err());
    }

    #[test]
    fn length_pseudo_field_on_strings_lists_and_records() {
        let item = record(vec![
            ("s", Value::Str("hello".into())),
            ("l", Value::List(vec![Value::Int(1), Value::Int(2)])),
        ]);
        assert_eq!(apply("f {|x| $x.s.length}", item.clone()), Ok(Value::Int(5)));
        assert_eq!(apply("f {|x| $x.l.length}", item.clone()), Ok(Value::Int(2)));
        assert_eq!(apply("f {|x| $x.s.length > 4}", item), Ok(Value::Bool(true)));
    }

    #[test]
    fn a_real_length_field_shadows_the_pseudo_field() {
        let item = record(vec![("length", Value::Str("actual".into()))]);
        assert_eq!(
            apply("f {|x| $x.length}", item),
            Ok(Value::Str("actual".into()))
        );
    }

    #[test]
    fn comparing_a_missing_field_is_false_not_an_error() {
        // A ragged row must not abort the whole pipeline.
        let item = record(vec![("other", Value::Int(1))]);
        assert_eq!(apply("f {|x| $x.nope > 5}", item.clone()), Ok(Value::Bool(false)));
        assert_eq!(apply("f {|x| $x.nope < 5}", item.clone()), Ok(Value::Bool(false)));
        // Equality still works normally against null.
        assert_eq!(apply("f {|x| $x.nope == null}", item), Ok(Value::Bool(true)));
    }

    #[test]
    fn incomparable_types_error_clearly() {
        let item = record(vec![("a", Value::Str("x".into()))]);
        let err = apply("f {|x| $x.a > 5}", item).expect_err("string vs int");
        assert!(err.contains("cannot compare"), "{err}");
    }

    #[test]
    fn parameter_name_is_arbitrary() {
        let item = record(vec![("n", Value::Int(1))]);
        assert_eq!(apply("f {|row| $row.n}", item), Ok(Value::Int(1)));
    }
}
