//! Built-in commands. All v1 builtins are synchronous; `Builtin` wraps a fn
//! pointer in a ready future so they satisfy the async `Command` trait.

use crate::error::{ErrorKind, ShellError};
use crate::registry::{
    ready, Command, CommandRegistry, ExecContext, LocalBoxFuture, PipelineData,
};
use crate::callable::{is_truthy, Selector};
use crate::render::plain;
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
        Signature::build("grep", "Filter rows or lines matching a pattern")
            .required_arg("pattern", Shape::Str, "regex in the browser, substring in the CLI")
            .on_selector("match against this field, path, {|o| …} closure, or @name")
            .flag("ignore-case", Some('i'), None, "case-insensitive match")
            .flag("invert", Some('v'), None, "keep non-matching rows instead"),
        grep,
    ));
    registry.register_builtin(cmd(
        Signature::build("map", "Project each item through a closure or field")
            .required_arg("selector", Shape::Str, "{|o| …} closure, field, dotted path, '(o) => …', or @name"),
        map,
    ));
    registry.register_builtin(cmd(
        Signature::build("filter", "Keep items whose predicate is truthy")
            .required_arg("predicate", Shape::Str, "{|o| …} closure, '(o) => …', or @name")
            .flag("invert", Some('v'), None, "keep items that return false instead"),
        filter,
    ));
    registry.register_builtin(cmd(
        Signature::build("head", "Take the first row (or first n rows)")
            .optional_arg("n", Shape::Int, "how many rows"),
        head,
    ));
    registry.register_builtin(cmd(
        Signature::build("tail", "Take the last row (or last n rows)")
            .optional_arg("n", Shape::Int, "how many rows"),
        tail,
    ));
    registry.register_builtin(cmd(
        Signature::build("length", "Count items in a list (or characters in a string)"),
        length,
    ));
    registry.register_builtin(cmd(
        Signature::build("sort-by", "Sort a table by a column or computed key")
            .optional_arg("column", Shape::Str, "column to sort by (shorthand for --on)")
            .on_selector("sort by this field, path, or {|o| …} computed key")
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

// `where` was retired once closures and `grep --on` landed: its comparison
// operators are `filter {|o| $o.col > 5}` and its text operators
// (contains/starts-with/ends-with) are `grep`'s job. Two orthogonal tools —
// text search and arbitrary predicate — instead of a third that reinvented a
// mini operator language.

/// Resolve the common `--on` parameter into a [`Selector`], turning a host
/// resolution failure into a spanned shell error.
fn on_selector(ctx: &ExecContext, call: &BoundCall) -> Result<Option<Selector>, ShellError> {
    // A closure literal — `--on {|x| …}` — needs no host at all.
    if let Some(f) = call.closure("on") {
        return Ok(Some(Selector::Callable(f)));
    }
    let Some(spec) = call.flag("on").and_then(|v| v.as_str()) else {
        return Ok(None);
    };
    Selector::parse(spec, ctx.host.as_ref())
        .map(Some)
        .map_err(|msg| {
            ShellError::new(ErrorKind::Binding, format!("`--on {spec}`: {msg}"))
                .with_span(call.head_span)
        })
}

/// Resolve a positional selector argument: a closure literal if one was
/// given, otherwise the string form routed through the host.
fn positional_selector(
    ctx: &ExecContext,
    call: &BoundCall,
    param: &str,
) -> Result<Selector, ShellError> {
    if let Some(f) = call.closure(param) {
        return Ok(Selector::Callable(f));
    }
    let spec = call.positionals[0].as_str().unwrap_or_default();
    Selector::parse(spec, ctx.host.as_ref()).map_err(|msg| {
        ShellError::new(ErrorKind::Binding, format!("`{spec}`: {msg}")).with_span(call.head_span)
    })
}

/// Apply a selector, converting its failure into a spanned shell error.
fn project(selector: &Selector, item: &Value, span: crate::error::Span) -> Result<Value, ShellError> {
    selector
        .apply(item)
        .map_err(|msg| ShellError::runtime(msg).with_span(span))
}

/// If a field selector names a column no row has, that's a typo — say so
/// rather than silently returning nothing. Callables get no such check;
/// computing `null` is legitimate for them.
fn check_field_exists(
    selector: &Selector,
    items: &[Value],
    span: crate::error::Span,
) -> Result<(), ShellError> {
    let Some(field) = selector.head_field() else {
        return Ok(());
    };
    if items.is_empty() {
        return Ok(());
    }
    let mut seen: Vec<String> = Vec::new();
    for item in items {
        if let Value::Record(map) = item {
            for k in map.keys() {
                if !seen.contains(k) {
                    seen.push(k.clone());
                }
            }
        }
    }
    if seen.iter().any(|k| k == field) {
        return Ok(());
    }
    let help = if seen.is_empty() {
        "`--on <field>` applies to tables (lists of records)".to_string()
    } else {
        format!("available columns: {}", seen.join(", "))
    };
    Err(
        ShellError::new(ErrorKind::Runtime, format!("no column `{field}`"))
            .with_span(span)
            .with_help(help),
    )
}

/// `map` projects each item; `filter` keeps items whose predicate is truthy.
/// Together with `--on` these are the composable half of the story: `--on`
/// changes what a command *looks at* while keeping the row, `map` changes
/// what flows downstream.
fn map(ctx: ExecContext, call: BoundCall, input: PipelineData) -> Result<PipelineData, ShellError> {
    let selector = positional_selector(&ctx, &call, "selector")?;
    match input.into_value() {
        Value::List(items) => {
            check_field_exists(&selector, &items, call.head_span)?;
            let mapped = items
                .iter()
                .map(|item| project(&selector, item, call.head_span))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(PipelineData::Value(Value::List(mapped)))
        }
        // A single value maps to a single value — no need to wrap it first.
        other => Ok(PipelineData::Value(project(&selector, &other, call.head_span)?)),
    }
}

fn filter(ctx: ExecContext, call: BoundCall, input: PipelineData) -> Result<PipelineData, ShellError> {
    let selector = positional_selector(&ctx, &call, "predicate")?;
    let invert = call.has_flag("invert");
    match input.into_value() {
        Value::List(items) => {
            let mut kept = Vec::new();
            for item in items {
                let verdict = is_truthy(&project(&selector, &item, call.head_span)?);
                if verdict != invert {
                    kept.push(item);
                }
            }
            Ok(PipelineData::Value(Value::List(kept)))
        }
        other => Err(type_err("filter", "a list", &other)),
    }
}

/// `grep` searches the text a value *displays as* — the same strings the
/// table renderer shows — so what you see is what you match. A `List` filters
/// its items (every cell of a row, or just the `--on` projection); a
/// multi-line `Str` filters its lines, the way real grep does.
fn grep(ctx: ExecContext, call: BoundCall, input: PipelineData) -> Result<PipelineData, ShellError> {
    let pattern_src = call.positionals[0].as_str().unwrap_or_default().to_string();
    let case_insensitive = call.has_flag("ignore-case");
    let invert = call.has_flag("invert");
    let selector = on_selector(&ctx, &call)?;

    let pattern = ctx
        .host
        .compile_pattern(&pattern_src, case_insensitive)
        .map_err(|msg| {
            ShellError::new(
                ErrorKind::Binding,
                format!("invalid {} pattern `{pattern_src}`: {msg}", ctx.host.pattern_dialect()),
            )
            .with_span(call.head_span)
        })?;
    // XOR with `invert` in one place so every branch honors -v.
    let keep = |text: &str| pattern.is_match(text) != invert;

    match input.into_value() {
        Value::Str(text) => {
            let lines: Vec<Value> = text
                .lines()
                .filter(|line| keep(line))
                .map(|line| Value::Str(line.to_string()))
                .collect();
            Ok(PipelineData::Value(Value::List(lines)))
        }
        Value::List(items) => {
            if let Some(sel) = &selector {
                // Same courtesy as `get`: an unknown column is a typo, not an
                // empty result set.
                check_field_exists(sel, &items, call.head_span)?;
            }
            let mut kept = Vec::new();
            for item in items {
                // With `--on`, test the projection; without it, a row matches
                // if any cell does. `keep` folds in `invert`, so the
                // any-cell branch tests raw and inverts once.
                let hit = match (&selector, &item) {
                    (Some(sel), _) => keep(&plain(&project(sel, &item, call.head_span)?)),
                    (None, Value::Record(map)) => {
                        map.values().any(|v| pattern.is_match(&plain(v))) != invert
                    }
                    (None, scalar) => keep(&plain(scalar)),
                };
                if hit {
                    kept.push(item);
                }
            }
            Ok(PipelineData::Value(Value::List(kept)))
        }
        other => Err(type_err("grep", "a list or string", &other)),
    }
}

fn take_n(call: &BoundCall) -> Result<Option<usize>, ShellError> {
    match call.positionals.first() {
        None => Ok(None),
        // try_from, not `as`: on wasm32 (32-bit usize) an as-cast truncates
        // and `head 4294967296` would silently take 0 rows.
        Some(Value::Int(n)) if *n >= 0 => Ok(Some(usize::try_from(*n).unwrap_or(usize::MAX))),
        Some(Value::Int(n)) => Err(ShellError::runtime(format!("`{n}` is negative")).with_span(call.head_span)),
        Some(other) => Err(type_err("head/tail", "an int", other)),
    }
}

fn head(_ctx: ExecContext, call: BoundCall, input: PipelineData) -> Result<PipelineData, ShellError> {
    let n = take_n(&call)?;
    match input.into_value() {
        Value::List(items) => Ok(PipelineData::Value(match n {
            None => items.into_iter().next().unwrap_or(Value::Null),
            Some(n) => Value::List(items.into_iter().take(n).collect()),
        })),
        other => Err(type_err("head", "a list", &other)),
    }
}

fn tail(_ctx: ExecContext, call: BoundCall, input: PipelineData) -> Result<PipelineData, ShellError> {
    let n = take_n(&call)?;
    match input.into_value() {
        Value::List(items) => Ok(PipelineData::Value(match n {
            None => items.into_iter().next_back().unwrap_or(Value::Null),
            Some(n) => {
                let skip = items.len().saturating_sub(n);
                Value::List(items.into_iter().skip(skip).collect())
            }
        })),
        other => Err(type_err("tail", "a list", &other)),
    }
}

fn length(_ctx: ExecContext, _call: BoundCall, input: PipelineData) -> Result<PipelineData, ShellError> {
    match input.into_value() {
        Value::List(items) => Ok(PipelineData::Value(Value::Int(items.len() as i64))),
        Value::Str(s) => Ok(PipelineData::Value(Value::Int(s.chars().count() as i64))),
        other => Err(type_err("length", "a list or string", &other)),
    }
}

fn sort_by(ctx: ExecContext, call: BoundCall, input: PipelineData) -> Result<PipelineData, ShellError> {
    // `sort-by n` is shorthand for `sort-by --on n`; --on wins if both are
    // given, and it additionally allows computed keys.
    let selector = match on_selector(&ctx, &call)? {
        Some(sel) => sel,
        None if call.closure("column").is_some() => {
            Selector::Callable(call.closure("column").expect("checked"))
        }
        None => match call.positionals.first().and_then(|v| v.as_str()) {
            Some(col) => Selector::parse(col, ctx.host.as_ref()).map_err(|msg| {
                ShellError::new(ErrorKind::Binding, format!("`{col}`: {msg}"))
                    .with_span(call.head_span)
            })?,
            None => {
                return Err(ShellError::new(
                    ErrorKind::Binding,
                    "`sort-by` needs a column or `--on`",
                )
                .with_span(call.head_span)
                .with_help("try `sort-by name` or `sort-by --on {|o| $o.a + $o.b}`"))
            }
        },
    };
    let reverse = call.has_flag("reverse");
    let mut rows = match input.into_value() {
        Value::List(rows) => rows,
        other => return Err(type_err("sort-by", "a table (list of records)", &other)),
    };
    check_field_exists(&selector, &rows, call.head_span)?;

    // Keys are computed once up front rather than inside the comparator:
    // a callable key must not be invoked O(n log n) times, and projection
    // can fail, which a comparator cannot report.
    let mut keyed: Vec<(Option<Value>, Value)> = Vec::with_capacity(rows.len());
    for row in rows.drain(..) {
        let key = project(&selector, &row, call.head_span)?;
        // Missing fields sort last, as before.
        let key = if matches!(key, Value::Null) { None } else { Some(key) };
        keyed.push((key, row));
    }
    keyed.sort_by(|(a, _), (b, _)| match (a, b) {
        (Some(ka), Some(kb)) => {
            let ord = ka.total_cmp_values(kb);
            if reverse {
                ord.reverse()
            } else {
                ord
            }
        }
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });
    Ok(PipelineData::Value(Value::List(
        keyed.into_iter().map(|(_, row)| row).collect(),
    )))
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
    Ok(PipelineData::Rendered(rendered.trim_end_matches('\n').to_string()))
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
        Some(text) => Ok(PipelineData::Rendered(text)),
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
        let ctx = ExecContext {
            host: Rc::new(TestHost),
            sink: Rc::new(crate::sink::NullSink),
            width: 80,
            pane: 0,
            run_id: 0,
        };
        let out = parse(src);
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        let (mut results, error) = block_on(eval_line(&out.line, &registry, &ctx, &Scope::new()));
        if let Some(e) = error {
            return Err(e);
        }
        Ok(results.pop().map(PipelineData::into_value).unwrap_or(Value::Null))
    }

    /// Like `eval`, but returns parse errors as `Err` instead of asserting
    /// — for cases where a parse failure is the expected outcome.
    fn eval_any(src: &str) -> Result<Value, ShellError> {
        let mut registry = CommandRegistry::new();
        register_all(&mut registry);
        let ctx = ExecContext {
            host: Rc::new(TestHost),
            sink: Rc::new(crate::sink::NullSink),
            width: 80,
            pane: 0,
            run_id: 0,
        };
        let out = parse(src);
        if let Some(e) = out.errors.into_iter().next() {
            return Err(e);
        }
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
        // `where` retired: the flagship filter is now a closure predicate.
        let v = eval(&format!(
            "echo {} | from json | filter {{|o| $o.text != ''}} | head 5",
            table_json()
        ))
        .expect("eval");
        match v {
            Value::List(rows) => {
                assert_eq!(rows.len(), 2);
            }
            other => panic!("expected table, got {other:?}"),
        }
    }

    #[test]
    fn where_is_gone() {
        // Retired in favor of `filter` / `grep`; make sure it stays gone.
        let err = eval(r#"echo '[]' | from json | where n gt 4"#).expect_err("removed");
        assert!(err.msg.contains("unknown command `where`"), "{}", err.msg);
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
    fn head_tail_without_n_return_single() {
        let v = eval(r#"echo '[1,2,3]' | from json | head"#).expect("eval");
        assert_eq!(v, Value::Int(1));
        let v = eval(r#"echo '[1,2,3]' | from json | tail"#).expect("eval");
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
    fn head_with_huge_n_returns_everything() {
        // 2^32: on wasm32 an `as usize` cast truncated this to 0.
        let v = eval("echo '[1,2,3]' | from json | head 4294967296 | length").expect("eval");
        assert_eq!(v, Value::Int(3));
    }

    #[test]
    fn to_json_refuses_non_finite() {
        let call = BoundCall {
            head_span: crate::error::Span::new(0, 0),
            positionals: vec![],
            flags: std::collections::HashMap::new(),
            closures: std::collections::HashMap::new(),
        };
        let ctx = ExecContext {
            host: Rc::new(TestHost),
            sink: Rc::new(crate::sink::NullSink),
            width: 80,
            pane: 0,
            run_id: 0,
        };
        let err = to_json(ctx, call, PipelineData::Value(Value::Float(f64::NAN)))
            .expect_err("NaN must not serialize");
        assert!(err.msg.contains("NaN"));
    }

    /// Writes to both diagnostic channels so the wiring from `ExecContext`
    /// through `eval_call` into a command can be asserted, not assumed.
    struct Noisy;
    impl Command for Noisy {
        fn signature(&self) -> &Signature {
            static SIG: std::sync::OnceLock<Signature> = std::sync::OnceLock::new();
            SIG.get_or_init(|| Signature::build("noisy", "writes to log and err"))
        }
        fn run(
            &self,
            ctx: ExecContext,
            _call: BoundCall,
            _input: PipelineData,
        ) -> LocalBoxFuture<Result<PipelineData, ShellError>> {
            ctx.sink.write(crate::sink::Record::log("progress"));
            ctx.sink.write(crate::sink::Record::err("warning"));
            ready(Ok(PipelineData::Value(Value::Int(1))))
        }
    }

    #[test]
    fn a_command_writes_diagnostics_to_the_context_sink() {
        let mut registry = CommandRegistry::new();
        registry.register_builtin(Rc::new(Noisy));

        let sink = Rc::new(crate::sink::CollectingSink::new());
        let ctx = ExecContext {
            host: Rc::new(TestHost),
            sink: sink.clone(),
            width: 80,
            pane: 0,
            run_id: 0,
        };
        let out = parse("noisy");
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        let (mut results, error) = block_on(eval_line(&out.line, &registry, &ctx, &Scope::new()));
        assert!(error.is_none(), "{:?}", error);

        assert_eq!(sink.log_lines(), vec!["progress"]);
        assert_eq!(sink.err_lines(), vec!["warning"]);
        // Diagnostics must not leak into the data channel — that is the
        // whole point of having a separate sink.
        assert_eq!(
            results.pop().map(PipelineData::into_value),
            Some(Value::Int(1))
        );
    }

    #[test]
    fn subcommand_typo_gets_did_you_mean() {
        // Since `str` resolves as a group, the diagnostic can name it rather
        // than reporting the whole phrase as one unknown command.
        let err = eval("str upcsae hi").expect_err("typo");
        assert!(err.msg.contains("`str` has no subcommand `upcsae`"), "{}", err.msg);
        assert_eq!(err.help.as_deref(), Some("did you mean `str upcase`?"));
    }

    // --- grep (native host: substring dialect) ---

    #[test]
    fn grep_filters_table_rows_across_all_columns() {
        // "Rust" appears in text; "webassembly.org" only in href — both hit.
        let v = eval(&format!("echo {} | from json | grep Rust | length", table_json()))
            .expect("eval");
        assert_eq!(v, Value::Int(1));
        let v = eval(r#"echo '[{"t":"a","href":"x.org"},{"t":"b","href":"y.com"}]' | from json | grep .org | get t"#)
            .expect("eval");
        assert_eq!(v, Value::List(vec![Value::Str("a".into())]));
    }

    #[test]
    fn grep_ignore_case_and_invert() {
        let json = r#"'[{"n":"Rust"},{"n":"wasm"}]'"#;
        let v = eval(&format!("echo {json} | from json | grep rust -i | length")).expect("eval");
        assert_eq!(v, Value::Int(1));
        let v = eval(&format!("echo {json} | from json | grep Rust -v | get n")).expect("eval");
        assert_eq!(v, Value::List(vec![Value::Str("wasm".into())]));
    }

    #[test]
    fn grep_on_restricts_the_search_but_keeps_whole_rows() {
        let json = r#"'[{"t":"rust","href":"a"},{"t":"b","href":"rust"}]'"#;
        // Unrestricted matches both rows; --on t matches only the first —
        // and the surviving row keeps every column, which is the whole point
        // of --on over piping through `get`.
        let v = eval(&format!("echo {json} | from json | grep rust | length")).expect("eval");
        assert_eq!(v, Value::Int(2));
        let v = eval(&format!("echo {json} | from json | grep rust --on t | get href"))
            .expect("eval");
        assert_eq!(v, Value::List(vec![Value::Str("a".into())]));
    }

    #[test]
    fn grep_on_unknown_column_errors_with_available_columns() {
        let err = eval(r#"echo '[{"a":1}]' | from json | grep x --on nope"#)
            .expect_err("unknown column");
        assert!(err.msg.contains("no column `nope`"), "{}", err.msg);
        assert!(err.help.expect("help").contains('a'));
    }

    #[test]
    fn on_is_rejected_by_commands_that_do_not_declare_it() {
        // The point of declaring `--on` per command rather than injecting it
        // everywhere: a meaningless use is an error, not a silent no-op.
        let err = eval("echo '[1,2]' | from json | length --on foo").expect_err("unknown flag");
        assert!(err.msg.contains("unknown flag"), "{}", err.msg);
    }

    #[test]
    fn grep_on_dotted_path_reaches_nested_records() {
        let json = r#"'[{"u":{"name":"ada"}},{"u":{"name":"bob"}}]'"#;
        let v = eval(&format!("echo {json} | from json | grep ada --on u.name | length"))
            .expect("eval");
        assert_eq!(v, Value::Int(1));
    }

    #[test]
    fn sort_by_on_is_shorthand_compatible() {
        let json = r#"'[{"n":3},{"n":1},{"n":2}]'"#;
        // Positional and --on forms agree.
        let a = eval(&format!("echo {json} | from json | sort-by n | get n")).expect("eval");
        let b = eval(&format!("echo {json} | from json | sort-by --on n | get n")).expect("eval");
        assert_eq!(a, b);
        assert_eq!(a, Value::List(vec![Value::Int(1), Value::Int(2), Value::Int(3)]));
    }

    #[test]
    fn sort_by_without_column_or_on_explains_itself() {
        let err = eval(r#"echo '[{"n":1}]' | from json | sort-by"#).expect_err("needs a key");
        assert!(err.msg.contains("needs a column or `--on`"), "{}", err.msg);
        assert!(err.help.expect("help").contains("--on"));
    }

    #[test]
    fn map_projects_a_field_and_a_dotted_path() {
        let json = r#"'[{"u":{"name":"ada"},"id":1},{"u":{"name":"bob"},"id":2}]'"#;
        let v = eval(&format!("echo {json} | from json | map u.name")).expect("eval");
        assert_eq!(
            v,
            Value::List(vec![Value::Str("ada".into()), Value::Str("bob".into())])
        );
        let v = eval(&format!("echo {json} | from json | map id")).expect("eval");
        assert_eq!(v, Value::List(vec![Value::Int(1), Value::Int(2)]));
    }

    #[test]
    fn map_unknown_field_errors_rather_than_yielding_nulls() {
        let err = eval(r#"echo '[{"a":1}]' | from json | map nope"#).expect_err("typo");
        assert!(err.msg.contains("no column `nope`"), "{}", err.msg);
    }

    #[test]
    fn inline_functions_need_a_js_host_natively() {
        // The native test host has no scripting engine; the error must say
        // so rather than mangling the source into a field name.
        let err = eval(r#"echo '[{"a":1}]' | from json | map '(o) => o.a'"#)
            .expect_err("no js host");
        assert!(err.msg.contains("JavaScript host"), "{}", err.msg);
    }

    #[test]
    fn grep_filters_lines_of_a_string() {
        let v = eval(r#"echo "alpha\nbeta\ngamma" | grep a | length"#).expect("eval");
        // alpha, beta, gamma all contain "a".
        assert_eq!(v, Value::Int(3));
        let v = eval(r#"echo "alpha\nbeta" | grep bet"#).expect("eval");
        assert_eq!(v, Value::List(vec![Value::Str("beta".into())]));
    }

    #[test]
    fn grep_no_matches_yields_empty_list() {
        let v = eval(r#"echo '[{"a":"x"}]' | from json | grep zzz | length"#).expect("eval");
        assert_eq!(v, Value::Int(0));
    }

    #[test]
    fn grep_scalar_list_and_type_error() {
        let v = eval(r#"echo '["one","two"]' | from json | grep tw"#).expect("eval");
        assert_eq!(v, Value::List(vec![Value::Str("two".into())]));
        let err = eval("echo 5 | grep x").expect_err("int input");
        assert!(err.msg.contains("expects a list or string"), "{}", err.msg);
    }

    #[test]
    fn grep_composes_with_the_rest_of_the_pipeline() {
        let v = eval(&format!(
            "echo {} | from json | grep -i wasm | head 1 | get text",
            table_json()
        ))
        .expect("eval");
        assert_eq!(v, Value::List(vec![Value::Str("WASM".into())]));
    }

    // --- native closures: no host engine involved ---
    //
    // The whole point of these tests is the host used by `eval()` has *no*
    // scripting engine (`compile_fn` errors). Everything below therefore
    // proves closures work identically in the CLI and the browser.

    #[test]
    fn closure_filters_without_any_host_engine() {
        let json = r#"'[{"id":1},{"id":7},{"id":9}]'"#;
        let v = eval(&format!("echo {json} | from json | filter {{|o| $o.id > 5}} | length"))
            .expect("eval");
        assert_eq!(v, Value::Int(2));
    }

    #[test]
    fn closure_maps_and_computes() {
        let json = r#"'[{"a":1,"b":2},{"a":10,"b":20}]'"#;
        let v = eval(&format!("echo {json} | from json | map {{|o| $o.a + $o.b}}")).expect("eval");
        assert_eq!(v, Value::List(vec![Value::Int(3), Value::Int(30)]));
    }

    #[test]
    fn closure_works_as_on_selector_keeping_whole_rows() {
        let json = r#"'[{"t":"rust","n":1},{"t":"wasm","n":2}]'"#;
        // Match on a computed value but keep every column.
        // `str upcase` isn't an expression — a clean parse error, not a panic.
        let v = eval_any(&format!(
            "echo {json} | from json | grep RUST --on {{|o| str upcase}} | length"
        ));
        assert!(v.is_err());

        let v = eval(&format!("echo {json} | from json | grep rust --on {{|o| $o.t}} | get n"))
            .expect("eval");
        assert_eq!(v, Value::List(vec![Value::Int(1)]));
    }

    #[test]
    fn closure_computes_a_sort_key() {
        let json = r#"'[{"a":1,"b":9},{"a":5,"b":1}]'"#;
        // Sort by a sum that exists in no column.
        let v = eval(&format!(
            "echo {json} | from json | sort-by --on {{|o| $o.a + $o.b}} | map a"
        ))
        .expect("eval");
        assert_eq!(v, Value::List(vec![Value::Int(5), Value::Int(1)]));
    }

    #[test]
    fn closure_string_predicate_and_logical_ops() {
        let json = r#"'[{"t":"rust","n":1},{"t":"wasm","n":9}]'"#;
        let v = eval(&format!(
            "echo {json} | from json | filter {{|o| $o.t == 'rust' || $o.n > 5}} | length"
        ))
        .expect("eval");
        assert_eq!(v, Value::Int(2));
    }

    #[test]
    fn closure_errors_are_spanned_not_panics() {
        // Unknown variable inside a closure body.
        let err = eval(r#"echo '[{"a":1}]' | from json | map {|o| $nope.a}"#)
            .expect_err("unknown var");
        assert!(err.msg.contains("unknown variable"), "{}", err.msg);
    }

    #[test]
    fn malformed_closures_report_clearly() {
        assert!(eval_any("echo 1 | map {|o| $o.a").is_err(), "unterminated closure");
        assert!(eval_any("echo 1 | map {$o}").is_err(), "missing parameters");
    }

    #[test]
    fn a_group_name_lists_its_subcommands() {
        // `mux` is not a command; before groups existed it was an unknown
        // command whose did-you-mean pointed at `map`.
        let out = eval("mux").expect("group help");
        let text = match out {
            Value::Str(s) => s,
            other => panic!("expected rendered help, got {other:?}"),
        };
        assert!(text.contains("`mux` is a command group"), "{text}");
        assert!(text.contains("mux split"), "{text}");
        assert!(text.contains("Split the active pane"), "{text}");
        // Groups nest: `mux window` is itself a group.
        assert!(eval("mux window").is_ok(), "nested group");
        // And --help on a group is the same page, not an arity error.
        assert_eq!(eval("mux --help").expect("group --help"), Value::Str(text));
    }

    #[test]
    fn unknown_subcommand_suggests_a_sibling_not_a_stranger() {
        let err = eval("mux windo").expect_err("bad subcommand");
        assert!(err.msg.contains("`mux` has no subcommand `windo`"), "{}", err.msg);
        assert_eq!(err.help.as_deref(), Some("did you mean `mux window`?"));

        // Nothing close: point at the group listing rather than guess.
        let err = eval("str frobnicate").expect_err("bad subcommand");
        assert_eq!(err.help.as_deref(), Some("run `str` to list its subcommands"));
    }

    #[test]
    fn a_real_command_wins_over_the_group_page() {
        // `to json` exists and `to` is a group; resolving the command must
        // still take priority over listing.
        assert_eq!(eval("echo 1 | to json").expect("eval"), Value::Str("1".into()));
    }

    #[test]
    fn quoted_true_stays_string_bareword_true_is_bool() {
        assert_eq!(eval("echo 'true'").expect("eval"), Value::Str("true".into()));
        assert_eq!(eval("echo true").expect("eval"), Value::Bool(true));
    }
}
