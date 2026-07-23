//! Selectors: the shared way a command says "look at *this part* of each
//! item." Backs the common `--on` parameter and `map`'s projection.
//!
//! A selector spec is one of three unambiguous forms:
//!
//! | spec | meaning |
//! |---|---|
//! | `href`, `user.name` | dotted field path into a record |
//! | `'(o) => o.id > 5'` | inline source, compiled by the host |
//! | `@byId` | a function the host registered under a name |
//!
//! Inline source is detected by `=>`, which no field path contains, and
//! named functions by a leading `@`. Everything else is a field path.
//!
//! Callables are supplied by the host — the browser compiles JavaScript,
//! while native hosts have no engine and say so. This is the same injection
//! seam as [`crate::matcher`], and it is deliberately the shape native shell
//! closures would plug into later: a closure is just a third `HostFn`.

use crate::registry::HostHooks;
use crate::value::Value;
use std::rc::Rc;

/// A callable supplied by the host, applied to one pipeline item at a time.
pub trait HostFn {
    fn call(&self, value: &Value) -> Result<Value, String>;
}

/// Compiles inline callable source. Hosts without a language engine return
/// an error naming the alternative.
pub trait FnCompiler {
    fn compile(&self, source: &str) -> Result<Rc<dyn HostFn>, String>;
    /// Shown in help and errors: `javascript`, or `none`.
    fn dialect(&self) -> &'static str;
}

/// Default: no scripting engine (the native CLI and tests).
pub struct NoFnCompiler;

impl FnCompiler for NoFnCompiler {
    fn compile(&self, _source: &str) -> Result<Rc<dyn HostFn>, String> {
        Err("inline functions need a JavaScript host; this shell is running natively".into())
    }

    fn dialect(&self) -> &'static str {
        "none"
    }
}

/// What part of an item a command should operate on.
pub enum Selector {
    /// The whole item — the default when `--on` is omitted.
    Identity,
    /// A dotted path into nested records. A missing field yields `Null`
    /// rather than erroring, so filters treat it as "no match" instead of
    /// blowing up a whole pipeline on one ragged row.
    Field(Vec<String>),
    Callable(Rc<dyn HostFn>),
}

impl std::fmt::Debug for Selector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Selector::Identity => write!(f, "Identity"),
            Selector::Field(path) => write!(f, "Field({})", path.join(".")),
            // Host callables are opaque — there's nothing to print.
            Selector::Callable(_) => write!(f, "Callable"),
        }
    }
}

impl Selector {
    /// Resolve a `--on`/projection spec against the host.
    pub fn parse(spec: &str, host: &dyn HostHooks) -> Result<Selector, String> {
        let trimmed = spec.trim();
        if trimmed.is_empty() {
            return Err("empty selector".into());
        }
        if trimmed.contains("=>") {
            return host.compile_fn(trimmed).map(Selector::Callable);
        }
        if let Some(name) = trimmed.strip_prefix('@') {
            if name.is_empty() {
                return Err("expected a function name after `@`".into());
            }
            return host.lookup_fn(name).map(Selector::Callable);
        }
        if trimmed.contains(char::is_whitespace) {
            return Err(format!(
                "`{trimmed}` is not a field path; inline functions must contain `=>`"
            ));
        }
        Ok(Selector::Field(
            trimmed.split('.').map(|s| s.to_string()).collect(),
        ))
    }

    /// Project one item.
    pub fn apply(&self, value: &Value) -> Result<Value, String> {
        match self {
            Selector::Identity => Ok(value.clone()),
            Selector::Field(path) => {
                let mut current = value;
                for key in path {
                    match current {
                        Value::Record(map) => match map.get(key) {
                            Some(v) => current = v,
                            None => return Ok(Value::Null),
                        },
                        _ => return Ok(Value::Null),
                    }
                }
                Ok(current.clone())
            }
            Selector::Callable(f) => f.call(value),
        }
    }

    /// Field names this selector reads at the top level, for "no column
    /// `x`" diagnostics. `None` when the selector isn't a field path.
    pub fn head_field(&self) -> Option<&str> {
        match self {
            Selector::Field(path) => path.first().map(|s| s.as_str()),
            _ => None,
        }
    }
}

/// Truthiness for callables used as predicates (`--match`, `where`-style
/// filtering). Mirrors JavaScript closely enough to be unsurprising, since
/// that is where these functions are authored: `false`, `null`, `0`, and
/// `""` are falsy; empty lists/records are **truthy**, matching JS objects.
pub fn is_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Int(n) => *n != 0,
        Value::Float(f) => *f != 0.0 && !f.is_nan(),
        Value::Str(s) => !s.is_empty(),
        Value::List(_) | Value::Record(_) => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::HostHooks;
    use std::cell::RefCell;

    /// Stands in for a JS host: recognizes two canned sources so selector
    /// plumbing is testable natively.
    struct FakeHost {
        calls: RefCell<usize>,
    }

    struct DoubleFn;
    impl HostFn for DoubleFn {
        fn call(&self, value: &Value) -> Result<Value, String> {
            match value {
                Value::Int(n) => Ok(Value::Int(n * 2)),
                other => Err(format!("expected int, got {}", other.type_name())),
            }
        }
    }

    impl HostHooks for FakeHost {
        fn compile_fn(&self, source: &str) -> Result<Rc<dyn HostFn>, String> {
            *self.calls.borrow_mut() += 1;
            if source.contains("* 2") {
                Ok(Rc::new(DoubleFn))
            } else {
                Err(format!("cannot compile `{source}`"))
            }
        }
        fn lookup_fn(&self, name: &str) -> Result<Rc<dyn HostFn>, String> {
            if name == "double" {
                Ok(Rc::new(DoubleFn))
            } else {
                Err(format!("no registered function `{name}`"))
            }
        }
    }

    fn host() -> FakeHost {
        FakeHost { calls: RefCell::new(0) }
    }

    fn record(pairs: Vec<(&str, Value)>) -> Value {
        Value::record(pairs.into_iter().map(|(k, v)| (k.to_string(), v)))
    }

    #[test]
    fn plain_word_is_a_field() {
        let s = Selector::parse("href", &host()).expect("parse");
        assert_eq!(s.head_field(), Some("href"));
        let v = record(vec![("href", Value::Str("x".into()))]);
        assert_eq!(s.apply(&v).expect("apply"), Value::Str("x".into()));
    }

    #[test]
    fn dotted_path_walks_nested_records() {
        let s = Selector::parse("user.name", &host()).expect("parse");
        let v = record(vec![(
            "user",
            record(vec![("name", Value::Str("ada".into()))]),
        )]);
        assert_eq!(s.apply(&v).expect("apply"), Value::Str("ada".into()));
    }

    #[test]
    fn missing_field_is_null_not_an_error() {
        let s = Selector::parse("nope.deeper", &host()).expect("parse");
        let v = record(vec![("a", Value::Int(1))]);
        assert_eq!(s.apply(&v).expect("apply"), Value::Null);
        // Scalars have no fields either — still Null, not a failure.
        assert_eq!(s.apply(&Value::Int(3)).expect("apply"), Value::Null);
    }

    #[test]
    fn arrow_source_compiles_through_the_host() {
        let h = host();
        let s = Selector::parse("(o) => o * 2", &h).expect("parse");
        assert_eq!(*h.calls.borrow(), 1, "went to the host compiler");
        assert_eq!(s.apply(&Value::Int(21)).expect("apply"), Value::Int(42));
    }

    #[test]
    fn at_prefix_looks_up_a_registered_function() {
        let s = Selector::parse("@double", &host()).expect("parse");
        assert_eq!(s.apply(&Value::Int(4)).expect("apply"), Value::Int(8));
        let err = Selector::parse("@missing", &host()).expect_err("unknown");
        assert!(err.contains("no registered function"), "{err}");
        assert!(Selector::parse("@", &host()).is_err());
    }

    #[test]
    fn native_host_rejects_inline_source_with_guidance() {
        struct Native;
        impl HostHooks for Native {}
        let err = Selector::parse("(o) => o.id", &Native).expect_err("no engine");
        assert!(err.contains("JavaScript host"), "{err}");
    }

    #[test]
    fn spaced_non_arrow_spec_is_a_clear_error() {
        // Catches an unquoted or malformed function before it silently
        // becomes a bizarre field name.
        let err = Selector::parse("o.id > 5", &host()).expect_err("not a field");
        assert!(err.contains("not a field path"), "{err}");
    }

    #[test]
    fn callable_errors_propagate() {
        let s = Selector::parse("(o) => o * 2", &host()).expect("parse");
        let err = s.apply(&Value::Str("nope".into())).expect_err("type error");
        assert!(err.contains("expected int"), "{err}");
    }

    #[test]
    fn truthiness_follows_javascript() {
        assert!(!is_truthy(&Value::Null));
        assert!(!is_truthy(&Value::Bool(false)));
        assert!(!is_truthy(&Value::Int(0)));
        assert!(!is_truthy(&Value::Str(String::new())));
        assert!(is_truthy(&Value::Int(1)));
        assert!(is_truthy(&Value::Str("x".into())));
        // Empty collections are truthy, as in JS.
        assert!(is_truthy(&Value::List(vec![])));
    }
}
