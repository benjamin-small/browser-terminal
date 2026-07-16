//! Built-in commands. All v1 builtins are synchronous; `Builtin` wraps a fn
//! pointer in a ready future so they satisfy the async `Command` trait.

use crate::error::{ErrorKind, ShellError};
use crate::registry::{
    ready, Command, CommandRegistry, ExecContext, LocalBoxFuture, PipelineData,
};
use crate::signature::{BoundCall, Shape, Signature};
use crate::value::Value;
use std::rc::Rc;

type RunFn = fn(ExecContext, BoundCall, PipelineData) -> Result<PipelineData, ShellError>;

struct Builtin {
    sig: Signature,
    run_fn: RunFn,
}

impl Command for Builtin {
    fn signature(&self) -> &Signature {
        &self.sig
    }

    fn run(
        &self,
        ctx: ExecContext,
        call: BoundCall,
        input: PipelineData,
    ) -> LocalBoxFuture<Result<PipelineData, ShellError>> {
        ready((self.run_fn)(ctx, call, input))
    }
}

fn cmd(sig: Signature, run_fn: RunFn) -> Rc<dyn Command> {
    Rc::new(Builtin { sig, run_fn })
}

pub fn register_all(registry: &mut CommandRegistry) {
    registry.register_builtin(cmd(
        Signature::build("echo", "Return the given values").rest_arg("values", Shape::Any, "values to return"),
        echo,
    ));
    registry.register_builtin(cmd(
        Signature::build("get", "Extract a column from a record or table")
            .required_arg("column", Shape::Str, "column/field name"),
        get,
    ));
    registry.register_builtin(cmd(
        Signature::build("where", "Filter table rows by comparing a column")
            .required_arg("column", Shape::Str, "column to test")
            .required_arg("op", Shape::Str, "eq|ne|gt|lt|ge|le|contains|starts-with|ends-with")
            .required_arg("value", Shape::Any, "value to compare against"),
        where_cmd,
    ));
    registry.register_builtin(cmd(
        Signature::build("first", "Take the first row (or first n rows)")
            .optional_arg("n", Shape::Int, "how many rows"),
        first,
    ));
    registry.register_builtin(cmd(
        Signature::build("last", "Take the last row (or last n rows)")
            .optional_arg("n", Shape::Int, "how many rows"),
        last,
    ));
    registry.register_builtin(cmd(
        Signature::build("length", "Count items in a list (or characters in a string)"),
        length,
    ));
    registry.register_builtin(cmd(
        Signature::build("sort-by", "Sort a table by a column")
            .required_arg("column", Shape::Str, "column to sort by")
            .flag("reverse", Some('r'), None, "descending order"),
        sort_by,
    ));
    registry.register_builtin(cmd(
        Signature::build("str upcase", "Uppercase the input (or given) strings")
            .rest_arg("values", Shape::Str, "strings to transform"),
        str_upcase,
    ));
    registry.register_builtin(cmd(
        Signature::build("str downcase", "Lowercase the input (or given) strings")
            .rest_arg("values", Shape::Str, "strings to transform"),
        str_downcase,
    ));
    registry.register_builtin(cmd(
        Signature::build("to json", "Serialize the input to a JSON string")
            .flag("pretty", Some('p'), None, "indent the output"),
        to_json,
    ));
    registry.register_builtin(cmd(
        Signature::build("from json", "Parse a JSON string into a value"),
        from_json,
    ));
    registry.register_builtin(cmd(
        Signature::build("table", "Force-render the input as text at the current width"),
        table,
    ));
    registry.register_builtin(cmd(
        Signature::build("help", "List commands, or show help for one")
            .rest_arg("command", Shape::Str, "command name words"),
        help,
    ));
    registry.register_builtin(cmd(
        Signature::build("history", "This shell's command history"),
        history,
    ));
    registry.register_builtin(cmd(Signature::build("clear", "Clear the screen"), clear));
}

fn type_err(cmd: &str, wanted: &str, got: &Value) -> ShellError {
    ShellError::new(
        ErrorKind::Type,
        format!("`{cmd}` expects {wanted}, found {}", got.type_name()),
    )
}

fn echo(_ctx: ExecContext, call: BoundCall, _input: PipelineData) -> Result<PipelineData, ShellError> {
    let mut values = call.positionals;
    Ok(match values.len() {
        0 => PipelineData::Empty,
        1 => PipelineData::Value(values.remove(0)),
        _ => PipelineData::Value(Value::List(values)),
    })
}

fn get(_ctx: ExecContext, call: BoundCall, input: PipelineData) -> Result<PipelineData, ShellError> {
    let column = call.positionals[0].as_str().unwrap_or_default().to_string();
    let missing = |map: &indexmap::IndexMap<String, Value>| {
        ShellError::new(ErrorKind::Runtime, format!("no column `{column}`"))
            .with_span(call.head_span)
            .with_help(format!(
                "available columns: {}",
                map.keys().cloned().collect::<Vec<_>>().join(", ")
            ))
    };
    match input.into_value() {
        Value::Record(map) => {
            let v = map.get(&column).cloned().ok_or_else(|| missing(&map))?;
            Ok(PipelineData::Value(v))
        }
        Value::List(rows) => {
            let mut out = Vec::new();
            for row in &rows {
                match row {
                    Value::Record(map) => out.push(map.get(&column).cloned().unwrap_or(Value::Null)),
                    other => return Err(type_err("get", "a record or table", other)),
                }
            }
            Ok(PipelineData::Value(Value::List(out)))
        }
        other => Err(type_err("get", "a record or table", &other)),
    }
}

fn where_cmd(_ctx: ExecContext, call: BoundCall, input: PipelineData) -> Result<PipelineData, ShellError> {
    let column = call.positionals[0].as_str().unwrap_or_default().to_string();
    let op = call.positionals[1].as_str().unwrap_or_default().to_string();
    let rhs = call.positionals[2].clone();

    const OPS: [&str; 9] = ["eq", "ne", "gt", "lt", "ge", "le", "contains", "starts-with", "ends-with"];
    if !OPS.contains(&op.as_str()) {
        return Err(ShellError::new(ErrorKind::Binding, format!("unknown operator `{op}`"))
            .with_span(call.head_span)
            .with_help(format!("valid operators: {}", OPS.join(", "))));
    }

    let rows = match input.into_value() {
        Value::List(rows) => rows,
        other => return Err(type_err("where", "a table (list of records)", &other)),
    };

    let matches = |cell: &Value| -> bool {
        match op.as_str() {
            "eq" => cell.loose_eq(&rhs),
            "ne" => !cell.loose_eq(&rhs),
            "gt" => matches!(cell.partial_cmp_values(&rhs), Some(std::cmp::Ordering::Greater)),
            "lt" => matches!(cell.partial_cmp_values(&rhs), Some(std::cmp::Ordering::Less)),
            "ge" => matches!(
                cell.partial_cmp_values(&rhs),
                Some(std::cmp::Ordering::Greater | std::cmp::Ordering::Equal)
            ),
            "le" => matches!(
                cell.partial_cmp_values(&rhs),
                Some(std::cmp::Ordering::Less | std::cmp::Ordering::Equal)
            ),
            "contains" => match (cell, &rhs) {
                (Value::Str(s), Value::Str(needle)) => s.contains(needle.as_str()),
                (Value::List(items), needle) => items.iter().any(|i| i.loose_eq(needle)),
                _ => false,
            },
            "starts-with" => match (cell, &rhs) {
                (Value::Str(s), Value::Str(p)) => s.starts_with(p.as_str()),
                _ => false,
            },
            "ends-with" => match (cell, &rhs) {
                (Value::Str(s), Value::Str(p)) => s.ends_with(p.as_str()),
                _ => false,
            },
            _ => false,
        }
    };

    let filtered: Vec<Value> = rows
        .into_iter()
        .filter(|row| match row {
            Value::Record(map) => map.get(&column).is_some_and(&matches),
            _ => false,
        })
        .collect();
    Ok(PipelineData::Value(Value::List(filtered)))
}

fn take_n(call: &BoundCall) -> Result<Option<usize>, ShellError> {
    match call.positionals.first() {
        None => Ok(None),
        Some(Value::Int(n)) if *n >= 0 => Ok(Some(*n as usize)),
        Some(Value::Int(n)) => Err(ShellError::runtime(format!("`{n}` is negative")).with_span(call.head_span)),
        Some(other) => Err(type_err("first/last", "an int", other)),
    }
}

fn first(_ctx: ExecContext, call: BoundCall, input: PipelineData) -> Result<PipelineData, ShellError> {
    let n = take_n(&call)?;
    match input.into_value() {
        Value::List(items) => Ok(PipelineData::Value(match n {
            None => items.into_iter().next().unwrap_or(Value::Null),
            Some(n) => Value::List(items.into_iter().take(n).collect()),
        })),
        other => Err(type_err("first", "a list", &other)),
    }
}

fn last(_ctx: ExecContext, call: BoundCall, input: PipelineData) -> Result<PipelineData, ShellError> {
    let n = take_n(&call)?;
    match input.into_value() {
        Value::List(items) => Ok(PipelineData::Value(match n {
            None => items.into_iter().next_back().unwrap_or(Value::Null),
            Some(n) => {
                let skip = items.len().saturating_sub(n);
                Value::List(items.into_iter().skip(skip).collect())
            }
        })),
        other => Err(type_err("last", "a list", &other)),
    }
}

fn length(_ctx: ExecContext, _call: BoundCall, input: PipelineData) -> Result<PipelineData, ShellError> {
    match input.into_value() {
        Value::List(items) => Ok(PipelineData::Value(Value::Int(items.len() as i64))),
        Value::Str(s) => Ok(PipelineData::Value(Value::Int(s.chars().count() as i64))),
        other => Err(type_err("length", "a list or string", &other)),
    }
}

fn sort_by(_ctx: ExecContext, call: BoundCall, input: PipelineData) -> Result<PipelineData, ShellError> {
    let column = call.positionals[0].as_str().unwrap_or_default().to_string();
    let reverse = call.has_flag("reverse");
    let mut rows = match input.into_value() {
        Value::List(rows) => rows,
        other => return Err(type_err("sort-by", "a table (list of records)", &other)),
    };
    // Missing/incomparable cells sort last, stably.
    rows.sort_by(|a, b| {
        let cell = |v: &Value| match v {
            Value::Record(map) => map.get(&column).cloned(),
            _ => None,
        };
        match (cell(a), cell(b)) {
            (Some(ca), Some(cb)) => ca
                .partial_cmp_values(&cb)
                .unwrap_or(std::cmp::Ordering::Equal),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        }
    });
    if reverse {
        rows.reverse();
    }
    Ok(PipelineData::Value(Value::List(rows)))
}

fn map_strings(
    cmd_name: &str,
    call: BoundCall,
    input: PipelineData,
    f: fn(&str) -> String,
) -> Result<PipelineData, ShellError> {
    let source = if call.positionals.is_empty() {
        input.into_value()
    } else if call.positionals.len() == 1 {
        call.positionals.into_iter().next().unwrap_or(Value::Null)
    } else {
        Value::List(call.positionals)
    };
    match source {
        Value::Str(s) => Ok(PipelineData::Value(Value::Str(f(&s)))),
        Value::List(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in &items {
                match item {
                    Value::Str(s) => out.push(Value::Str(f(s))),
                    other => return Err(type_err(cmd_name, "strings", other)),
                }
            }
            Ok(PipelineData::Value(Value::List(out)))
        }
        other => Err(type_err(cmd_name, "a string or list of strings", &other)),
    }
}

fn str_upcase(_ctx: ExecContext, call: BoundCall, input: PipelineData) -> Result<PipelineData, ShellError> {
    map_strings("str upcase", call, input, |s| s.to_uppercase())
}

fn str_downcase(_ctx: ExecContext, call: BoundCall, input: PipelineData) -> Result<PipelineData, ShellError> {
    map_strings("str downcase", call, input, |s| s.to_lowercase())
}

fn to_json(_ctx: ExecContext, call: BoundCall, input: PipelineData) -> Result<PipelineData, ShellError> {
    let value = input.into_value();
    let json = if call.has_flag("pretty") {
        serde_json::to_string_pretty(&value)
    } else {
        serde_json::to_string(&value)
    }
    .map_err(|e| ShellError::runtime(format!("cannot serialize to JSON: {e}")))?;
    Ok(PipelineData::Value(Value::Str(json)))
}

fn from_json(_ctx: ExecContext, call: BoundCall, input: PipelineData) -> Result<PipelineData, ShellError> {
    match input.into_value() {
        Value::Str(s) => {
            let v: Value = serde_json::from_str(&s)
                .map_err(|e| ShellError::runtime(format!("invalid JSON: {e}")).with_span(call.head_span))?;
            Ok(PipelineData::Value(v))
        }
        other => Err(type_err("from json", "a JSON string", &other)),
    }
}

fn table(ctx: ExecContext, _call: BoundCall, input: PipelineData) -> Result<PipelineData, ShellError> {
    let rendered = crate::render::render(&input.into_value(), ctx.width);
    Ok(PipelineData::Value(Value::Str(rendered.trim_end_matches('\n').to_string())))
}

fn help(ctx: ExecContext, call: BoundCall, _input: PipelineData) -> Result<PipelineData, ShellError> {
    if call.positionals.is_empty() {
        let rows: Vec<Value> = ctx
            .host
            .help_overview()
            .into_iter()
            .map(|(name, summary)| {
                Value::record([
                    ("command".to_string(), Value::Str(name)),
                    ("summary".to_string(), Value::Str(summary)),
                ])
            })
            .collect();
        return Ok(PipelineData::Value(Value::List(rows)));
    }
    let name = call
        .positionals
        .iter()
        .filter_map(|v| v.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    match ctx.host.help_for(&name) {
        Some(text) => Ok(PipelineData::Value(Value::Str(text))),
        None => Err(ShellError::new(ErrorKind::UnknownCommand, format!("no help for `{name}`"))
            .with_span(call.head_span)
            .with_help("run `help` to list commands")),
    }
}

fn history(ctx: ExecContext, _call: BoundCall, _input: PipelineData) -> Result<PipelineData, ShellError> {
    Ok(PipelineData::Value(Value::List(
        ctx.host.history().into_iter().map(Value::Str).collect(),
    )))
}

fn clear(ctx: ExecContext, _call: BoundCall, _input: PipelineData) -> Result<PipelineData, ShellError> {
    ctx.host.request_clear();
    Ok(PipelineData::Empty)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::{block_on, eval_line};
    use crate::parse::parse;
    use crate::registry::HostHooks;
    use crate::signature::Scope;

    struct TestHost;
    impl HostHooks for TestHost {
        fn emit_line(&self, _line: &str) {}
        fn history(&self) -> Vec<String> {
            vec!["echo 1".into(), "help".into()]
        }
        fn help_overview(&self) -> Vec<(String, String)> {
            vec![("echo".into(), "Return the given values".into())]
        }
        fn help_for(&self, name: &str) -> Option<String> {
            (name == "echo").then(|| "echo help text".to_string())
        }
    }

    fn eval(src: &str) -> Result<Value, ShellError> {
        let mut registry = CommandRegistry::new();
        register_all(&mut registry);
        let ctx = ExecContext { host: Rc::new(TestHost), width: 80 };
        let out = parse(src);
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        let mut results = block_on(eval_line(&out.line, &registry, &ctx, &Scope::new()))?;
        Ok(results.pop().map(PipelineData::into_value).unwrap_or(Value::Null))
    }

    fn table_json() -> &'static str {
        r#"'[{"text":"Rust","href":"a"},{"text":"","href":"b"},{"text":"WASM","href":"c"}]'"#
    }

    #[test]
    fn flagship_pipeline_works() {
        let v = eval(&format!("echo {} | from json | where text ne '' | first 5", table_json()))
            .expect("eval");
        match v {
            Value::List(rows) => {
                assert_eq!(rows.len(), 2);
            }
            other => panic!("expected table, got {other:?}"),
        }
    }

    #[test]
    fn where_gt_on_numbers() {
        let v = eval(r#"echo '[{"n":1},{"n":5},{"n":10}]' | from json | where n gt 4 | length"#)
            .expect("eval");
        assert_eq!(v, Value::Int(2));
    }

    #[test]
    fn where_bad_op_lists_valid_ops() {
        let err = eval(r#"echo '[]' | from json | where n above 4"#).expect_err("bad op");
        assert!(err.msg.contains("unknown operator `above`"));
        assert!(err.help.expect("help").contains("contains"));
    }

    #[test]
    fn get_extracts_column() {
        let v = eval(&format!("echo {} | from json | get text", table_json())).expect("eval");
        assert_eq!(
            v,
            Value::List(vec![
                Value::Str("Rust".into()),
                Value::Str("".into()),
                Value::Str("WASM".into())
            ])
        );
    }

    #[test]
    fn get_missing_column_lists_available() {
        let err = eval(r#"echo '{"a":1}' | from json | get b"#).expect_err("missing");
        assert!(err.help.expect("help").contains("a"));
    }

    #[test]
    fn sort_by_orders_and_reverses() {
        let v = eval(r#"echo '[{"n":5},{"n":1},{"n":10}]' | from json | sort-by n --reverse | get n"#)
            .expect("eval");
        assert_eq!(v, Value::List(vec![Value::Int(10), Value::Int(5), Value::Int(1)]));
    }

    #[test]
    fn first_last_without_n_return_single() {
        let v = eval(r#"echo '[1,2,3]' | from json | first"#).expect("eval");
        assert_eq!(v, Value::Int(1));
        let v = eval(r#"echo '[1,2,3]' | from json | last"#).expect("eval");
        assert_eq!(v, Value::Int(3));
    }

    #[test]
    fn str_case_on_input_and_args() {
        assert_eq!(eval("echo abc | str upcase").expect("eval"), Value::Str("ABC".into()));
        assert_eq!(eval("str downcase HI").expect("eval"), Value::Str("hi".into()));
    }

    #[test]
    fn json_round_trip() {
        let v = eval(r#"echo '{"a":1}' | from json | to json"#).expect("eval");
        assert_eq!(v, Value::Str(r#"{"a":1}"#.into()));
    }

    #[test]
    fn help_overview_is_a_table() {
        let v = eval("help").expect("eval");
        assert!(v.is_table());
    }

    #[test]
    fn help_for_command_via_host() {
        let v = eval("help echo").expect("eval");
        assert_eq!(v, Value::Str("echo help text".into()));
    }

    #[test]
    fn history_comes_from_host() {
        let v = eval("history").expect("eval");
        assert_eq!(
            v,
            Value::List(vec![Value::Str("echo 1".into()), Value::Str("help".into())])
        );
    }

    #[test]
    fn echo_multiple_makes_list() {
        let v = eval("echo a b").expect("eval");
        assert_eq!(v, Value::List(vec![Value::Str("a".into()), Value::Str("b".into())]));
    }
}
