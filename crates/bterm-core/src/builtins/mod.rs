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

    // Multiplexer commands — everything the prefix keymap does is one of
    // these, so `:`-style scripting gets the full tmux surface for free.
    registry.register_builtin(cmd(
        Signature::build("mux split", "Split the active pane")
            .flag("right", Some('r'), None, "split side-by-side (default)")
            .flag("down", Some('d'), None, "split stacked"),
        mux_split,
    ));
    registry.register_builtin(cmd(
        Signature::build("mux window new", "Open a new window in this session"),
        |ctx, _c, _i| mux_do(ctx, MuxAction::WindowNew),
    ));
    registry.register_builtin(cmd(
        Signature::build("mux window next", "Focus the next window"),
        |ctx, _c, _i| mux_do(ctx, MuxAction::WindowNext),
    ));
    registry.register_builtin(cmd(
        Signature::build("mux window prev", "Focus the previous window"),
        |ctx, _c, _i| mux_do(ctx, MuxAction::WindowPrev),
    ));
    registry.register_builtin(cmd(
        Signature::build("mux kill-pane", "Close the active pane"),
        |ctx, _c, _i| mux_do(ctx, MuxAction::KillPane),
    ));
    registry.register_builtin(cmd(
        Signature::build("mux focus", "Move focus between panes")
            .required_arg("direction", Shape::Str, "next|left|right|up|down"),
        mux_focus,
    ));
    registry.register_builtin(cmd(
        Signature::build("mux zoom", "Toggle zoom on the active pane"),
        |ctx, _c, _i| mux_do(ctx, MuxAction::Zoom),
    ));
    registry.register_builtin(cmd(
        Signature::build("mux hide", "Hide the terminal panel"),
        |ctx, _c, _i| mux_do(ctx, MuxAction::Hide),
    ));
    registry.register_builtin(cmd(
        Signature::build("session new", "Fork a new shell session")
            .optional_arg("name", Shape::Str, "session name"),
        session_new,
    ));
    registry.register_builtin(cmd(
        Signature::build("session list", "List sessions"),
        |ctx, _c, _i| mux_do(ctx, MuxAction::SessionList),
    ));
    registry.register_builtin(cmd(
        Signature::build("session switch", "Switch to a session by name")
            .required_arg("name", Shape::Str, "session name"),
        session_switch,
    ));
    registry.register_builtin(cmd(
        Signature::build("session next", "Cycle to the next session"),
        |ctx, _c, _i| mux_do(ctx, MuxAction::SessionNext),
    ));
    registry.register_builtin(cmd(
        Signature::build("session prev", "Cycle to the previous session"),
        |ctx, _c, _i| mux_do(ctx, MuxAction::SessionPrev),
    ));
}

use crate::registry::MuxAction;

fn mux_do(ctx: ExecContext, action: MuxAction) -> Result<PipelineData, ShellError> {
    let value = ctx.host.mux_action(action)?;
    Ok(match value {
        Value::Null => PipelineData::Empty,
        v => PipelineData::Value(v),
    })
}

fn mux_split(ctx: ExecContext, call: BoundCall, _input: PipelineData) -> Result<PipelineData, ShellError> {
    let action = if call.has_flag("down") {
        MuxAction::SplitDown
    } else {
        MuxAction::SplitRight
    };
    mux_do(ctx, action)
}

fn mux_focus(ctx: ExecContext, call: BoundCall, _input: PipelineData) -> Result<PipelineData, ShellError> {
    let dir = call.positionals[0].as_str().unwrap_or_default().to_string();
    mux_do(ctx, MuxAction::Focus(dir))
}

fn session_new(ctx: ExecContext, call: BoundCall, _input: PipelineData) -> Result<PipelineData, ShellError> {
    let name = call
        .positionals
        .first()
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    mux_do(ctx, MuxAction::SessionNew { name })
}

fn session_switch(ctx: ExecContext, call: BoundCall, _input: PipelineData) -> Result<PipelineData, ShellError> {
    let name = call.positionals[0].as_str().unwrap_or_default().to_string();
    mux_do(ctx, MuxAction::SessionSwitch { name })
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
        // try_from, not `as`: on wasm32 (32-bit usize) an as-cast truncates
        // and `first 4294967296` would silently take 0 rows.
        Some(Value::Int(n)) if *n >= 0 => Ok(Some(usize::try_from(*n).unwrap_or(usize::MAX))),
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
    // total_cmp_values keeps the comparator a genuine total order even on
    // mixed-type columns (std's sort panics on intransitive comparators).
    // --reverse flips the value order only: missing-column rows stay last,
    // and the sort stays stable (no post-hoc rows.reverse()).
    rows.sort_by(|a, b| {
        let cell = |v: &Value| match v {
            Value::Record(map) => map.get(&column).cloned(),
            _ => None,
        };
        match (cell(a), cell(b)) {
            (Some(ca), Some(cb)) => {
                let ord = ca.total_cmp_values(&cb);
                if reverse {
                    ord.reverse()
                } else {
                    ord
                }
            }
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        }
    });
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
    if value.has_non_finite() {
        return Err(ShellError::runtime(
            "cannot serialize NaN or Infinity to JSON",
        )
        .with_span(call.head_span)
        .with_help("JSON has no representation for non-finite numbers"));
    }
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
        let ctx = ExecContext { host: Rc::new(TestHost), width: 80, pane: 0, run_id: 0 };
        let out = parse(src);
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        let (mut results, error) = block_on(eval_line(&out.line, &registry, &ctx, &Scope::new()));
        if let Some(e) = error {
            return Err(e);
        }
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

    // --- regression tests from the M2 adversarial review ---

    #[test]
    fn sort_by_mixed_type_column_never_panics() {
        // 30 rows mixing ints and strings previously violated sort_by's
        // total-order contract (std detects intransitivity and panics; under
        // panic=abort that killed the whole WASM terminal).
        let mut rows = Vec::new();
        for i in 0..30 {
            if i % 3 == 0 {
                rows.push(format!("{{\"n\":\"s{:03}\"}}", (i * 37) % 1000));
            } else {
                rows.push(format!("{{\"n\":{}}}", (i * 251) % 1000));
            }
        }
        let json = format!("[{}]", rows.join(","));
        let v = eval(&format!("echo '{json}' | from json | sort-by n | get n")).expect("no panic");
        // Deterministic: all numbers first (sorted), then all strings.
        match v {
            Value::List(items) => {
                let first_str = items.iter().position(|x| matches!(x, Value::Str(_))).expect("has strings");
                assert!(items[..first_str].iter().all(|x| matches!(x, Value::Int(_))));
                assert!(items[first_str..].iter().all(|x| matches!(x, Value::Str(_))));
            }
            other => panic!("expected list, got {other:?}"),
        }
    }

    #[test]
    fn sort_by_reverse_keeps_missing_last_and_is_stable() {
        let json = r#"[{"n":1,"tag":"a"},{"tag":"missing"},{"n":3,"tag":"b"},{"n":3,"tag":"c"}]"#;
        let v = eval(&format!("echo '{json}' | from json | sort-by n --reverse | get tag")).expect("eval");
        assert_eq!(
            v,
            Value::List(vec![
                Value::Str("b".into()), // 3 (first of the equal run — stable)
                Value::Str("c".into()), // 3
                Value::Str("a".into()), // 1
                Value::Str("missing".into()), // missing column stays last
            ])
        );
    }

    #[test]
    fn first_with_huge_n_returns_everything() {
        // 2^32: on wasm32 an `as usize` cast truncated this to 0.
        let v = eval("echo '[1,2,3]' | from json | first 4294967296 | length").expect("eval");
        assert_eq!(v, Value::Int(3));
    }

    #[test]
    fn to_json_refuses_non_finite() {
        let call = BoundCall {
            head_span: crate::error::Span::new(0, 0),
            positionals: vec![],
            flags: std::collections::HashMap::new(),
        };
        let ctx = ExecContext { host: Rc::new(TestHost), width: 80, pane: 0, run_id: 0 };
        let err = to_json(ctx, call, PipelineData::Value(Value::Float(f64::NAN)))
            .expect_err("NaN must not serialize");
        assert!(err.msg.contains("NaN"));
    }

    #[test]
    fn subcommand_typo_gets_did_you_mean() {
        let err = eval("str upcsae hi").expect_err("typo");
        assert!(err.msg.contains("unknown command `str upcsae`"), "{}", err.msg);
        assert_eq!(err.help.as_deref(), Some("did you mean `str upcase`?"));
    }

    #[test]
    fn quoted_true_stays_string_bareword_true_is_bool() {
        assert_eq!(eval("echo 'true'").expect("eval"), Value::Str("true".into()));
        assert_eq!(eval("echo true").expect("eval"), Value::Bool(true));
    }
}
