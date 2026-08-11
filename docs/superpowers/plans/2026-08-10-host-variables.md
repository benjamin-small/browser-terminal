# Host-Injected Shell Variables Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let a host page inject typed values as shell `$variables`, so a registered command can be invoked as `rtce evaluate --game $game` instead of having application state serialized into the command text.

**Architecture:** One engine-wide `host_vars: Scope` on `Engine`, merged *underneath* each session's own (currently always empty) `vars` at `scope_for_pane` — the single place a scope is ever built, and already shared by interactive panes, `run()`, argument binding, and string interpolation. Five methods on `BrowserTerminal` set and read it; a `vars` builtin lists what is visible through a new `HostHooks` method.

**Tech Stack:** Rust (`bterm-core`, `bterm-wasm`), `wasm-bindgen`, TypeScript, Vite, Playwright.

## Global Constraints

- The spec is `docs/superpowers/specs/2026-08-10-host-variables-design.md`. Read it if a decision here seems arbitrary; it records why.
- **Engine access rule:** engine state is reached only inside a synchronous `access.with(|e| …)` / `WasmAccess.with(|e| …)` closure. No borrow may cross an `.await`, and **no JS call may happen inside one**. Violating this is a `RefCell` panic, and with `panic = "abort"` that is a dead terminal, not an exception.
- **No `unwrap` / `expect` in non-test code.** Clippy denies `unwrap_used` on core and wasm.
- Session values override host values. Never the reverse.
- Name validation is defined once, in Rust, reusing the lexer's own `is_var_char`. Never reimplement the rule in TypeScript.
- `Scope` is `std::collections::HashMap<String, Value>` (`crates/bterm-core/src/signature.rs:242`).
- Run `cargo test --workspace` and `cargo clippy --workspace --all-targets` before every commit. Baseline is **237 native tests, 19 wasm boundary tests, 16 Playwright tests**, all passing.
- Do not weaken an existing test to make a new one pass. If an existing test fails, that is a finding — report it.

---

## File Structure

| File | Responsibility |
| --- | --- |
| `crates/bterm-core/src/lex.rs` | `is_valid_var_name` — the single definition of a legal variable name |
| `crates/bterm-core/src/engine.rs` | `host_vars` field + accessors; the merge in `scope_for_pane`; `EngineHost::visible_vars` |
| `crates/bterm-core/src/registry.rs` | `HostHooks::visible_vars` with an empty default |
| `crates/bterm-core/src/builtins/mod.rs` | the `vars` builtin |
| `crates/bterm-wasm/src/lib.rs` | five `BtermCore` methods |
| `packages/browser-terminal/src/index.ts` | five `BrowserTerminal` methods |
| `packages/browser-terminal/README.md` | host-state example |
| `packages/demo/src/main.ts` | demo injection |
| `packages/demo/tests/smoke.spec.ts` | browser proof |

---

## Task 1: `is_valid_var_name`

**Files:**
- Modify: `crates/bterm-core/src/lex.rs` (near `is_var_char`, around line 107)

**Interfaces:**
- Consumes: nothing.
- Produces: `pub fn is_valid_var_name(name: &str) -> bool` in `crate::lex`.

Context: `lex.rs` already has `fn is_var_char(c: char) -> bool { c.is_alphanumeric() || c == '_' }` (private, line 107). The lexer builds a `$name` token by taking chars while `is_var_char` holds, and errors if the result is empty. This task exposes exactly that rule so setters cannot drift from it.

**Why this is its own task:** Rust's `is_alphanumeric` is Unicode-aware, so `$café` and `$1` are legal names. A hand-written `/^[A-Za-z_]\w*$/` at the TypeScript boundary would reject names the lexer accepts, letting a host set a variable it cannot reference. The accept-cases below are the point of the test.

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)] mod tests` block at the bottom of `crates/bterm-core/src/lex.rs`:

```rust
    #[test]
    fn valid_var_names_match_what_the_lexer_accepts() {
        // Accepted — these are not exotic, they are what `is_var_char`
        // already allows. Rust's `is_alphanumeric` is Unicode-aware, and
        // digits are legal including in leading position.
        for name in ["game", "_x", "a1", "A_B_2", "café", "日本", "1"] {
            assert!(is_valid_var_name(name), "should accept `{name}`");
        }
        // Rejected — every one of these would fail to lex as `$name`.
        for name in ["", " ", "a b", "a-b", "a.b", "a$b", "a\n"] {
            assert!(!is_valid_var_name(name), "should reject `{name}`");
        }
    }

    #[test]
    fn a_valid_name_round_trips_through_the_lexer() {
        // The guarantee that matters: anything `is_valid_var_name` accepts
        // must actually lex as a variable token. Without this the two rules
        // can drift apart silently.
        for name in ["game", "_x", "café", "1"] {
            let tokens = lex(&format!("${name}")).expect("should lex");
            assert!(
                matches!(&tokens[0].kind, TokenKind::Var(v) if v == name),
                "`${name}` did not lex as a Var token: {:?}",
                tokens[0].kind
            );
        }
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cargo test -p bterm-core --lib lex::tests::valid_var_names
```

Expected: FAIL — `cannot find function 'is_valid_var_name' in this scope`.

- [ ] **Step 3: Implement**

In `crates/bterm-core/src/lex.rs`, directly below `is_var_char` (line ~109):

```rust
/// Is `name` usable as `$name`?
///
/// The single definition of that rule. Callers that validate a variable
/// name — the host-variable setters in particular — must use this rather
/// than writing their own pattern: `is_var_char` accepts anything Unicode
/// considers alphanumeric, so `café` and `1` are legal names, and an ASCII
/// regex would reject names the lexer is perfectly happy with.
pub fn is_valid_var_name(name: &str) -> bool {
    !name.is_empty() && name.chars().all(is_var_char)
}
```

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cargo test -p bterm-core --lib lex::tests
```

Expected: PASS.

- [ ] **Step 5: Full suite and clippy**

```bash
cargo test --workspace && cargo clippy --workspace --all-targets
```

Expected: 239 passed (237 baseline + 2), clippy clean.

- [ ] **Step 6: Commit**

```bash
git add crates/bterm-core/src/lex.rs
git commit -m "Expose the lexer's own rule for a legal variable name

Callers that validate a name must not restate the rule. is_var_char
accepts anything Unicode calls alphanumeric, so \$café and \$1 are legal;
an ASCII pattern elsewhere would reject names the lexer accepts, letting
a host set a variable it could never reference."
```

---

## Task 2: `Engine.host_vars` and the merge

**Files:**
- Modify: `crates/bterm-core/src/engine.rs` — struct (line ~29), `Engine::new` (line ~69), accessors (near `set_matcher`, line ~85), `scope_for_pane` (line ~600)

**Interfaces:**
- Consumes: `crate::lex::is_valid_var_name` from Task 1.
- Produces, all on `Engine`:
  - `pub fn set_host_var(&mut self, name: &str, value: Value) -> Result<(), ShellError>`
  - `pub fn unset_host_var(&mut self, name: &str) -> bool`
  - `pub fn host_var(&self, name: &str) -> Option<&Value>`
  - `pub fn host_vars(&self) -> &Scope`

Context: `scope_for_pane` (line ~600) is the only place a `Scope` is constructed, and both `execute_line` (line ~773) and `eval_to_value` (line ~826) call it. Merging here covers panes, `run()`, positional args, flag values, and `"…$name…"` interpolation at once.

- [ ] **Step 1: Write the failing tests**

Add to the `#[cfg(test)] mod tests` block in `crates/bterm-core/src/engine.rs`. These use the existing test helpers in that module — find how neighbouring tests construct an engine and feed a line (search for `fn feed_line` or similar existing helpers) and follow that shape. If no helper exists that submits a line and returns pane output, use the same construction the nearby `did you mean` test at line ~946 uses.

```rust
    #[test]
    fn a_host_variable_resolves_in_a_command() {
        let engine = TestEngine::new();
        engine.with(|e| {
            e.set_host_var("greeting", Value::Str("hello".into())).expect("valid name");
        });
        let out = engine.run_line("echo $greeting");
        assert_eq!(out, Value::Str("hello".into()));
    }

    #[test]
    fn a_session_variable_overrides_a_host_variable() {
        let engine = TestEngine::new();
        engine.with(|e| {
            e.set_host_var("x", Value::Str("host".into())).expect("valid name");
            let sid = e.mux.active_session();
            if let Some(s) = e.mux.sessions.get_mut(&sid) {
                s.vars.insert("x".into(), Value::Str("session".into()));
            }
        });
        assert_eq!(engine.run_line("echo $x"), Value::Str("session".into()));
    }

    #[test]
    fn setting_a_host_variable_again_replaces_it() {
        let engine = TestEngine::new();
        engine.with(|e| {
            e.set_host_var("x", Value::Int(1)).expect("valid");
            e.set_host_var("x", Value::Int(2)).expect("valid");
        });
        assert_eq!(engine.run_line("echo $x"), Value::Int(2));
    }

    #[test]
    fn unsetting_restores_the_unknown_variable_error() {
        let engine = TestEngine::new();
        engine.with(|e| {
            e.set_host_var("x", Value::Int(1)).expect("valid");
            assert!(e.unset_host_var("x"), "was set, so removal reports true");
            assert!(!e.unset_host_var("x"), "already gone, so a second removal reports false");
        });
        let err = engine.run_line_err("echo $x");
        assert!(err.msg.contains("unknown variable `$x`"), "{}", err.msg);
    }

    #[test]
    fn an_invalid_name_is_rejected_rather_than_stored() {
        let engine = TestEngine::new();
        engine.with(|e| {
            let err = e.set_host_var("a b", Value::Int(1)).expect_err("space is not a var char");
            assert!(err.msg.contains("a b"), "the error should name the offender: {}", err.msg);
            assert!(e.host_vars().is_empty(), "nothing should have been stored");
        });
    }

    #[test]
    fn interpolation_sees_host_variables() {
        let engine = TestEngine::new();
        engine.with(|e| {
            e.set_host_var("name", Value::Str("world".into())).expect("valid");
        });
        assert_eq!(engine.run_line(r#"echo "hello-$name""#), Value::Str("hello-world".into()));
    }
```

**If `TestEngine`, `run_line`, or `run_line_err` do not exist**, write the smallest helpers that do this against the real `Engine` and put them in the test module — do not change production code to make testing easier, and do not skip a test because a helper is missing. Report what you added.

- [ ] **Step 2: Run to verify they fail**

```bash
cargo test -p bterm-core --lib engine::tests::a_host_variable_resolves
```

Expected: FAIL — no method `set_host_var`.

- [ ] **Step 3: Add the field**

In the `Engine` struct (line ~29), after `fn_compiler`:

```rust
    /// Supplied by the host: values injected as `$name`, visible to every
    /// session. Sessions keep their own `vars`, which win on collision —
    /// see `scope_for_pane`.
    host_vars: Scope,
```

In `Engine::new` (line ~72), add to the struct literal:

```rust
            host_vars: Scope::new(),
```

Add `use crate::signature::Scope;` to the file's imports if it is not already there.

- [ ] **Step 4: Add the accessors**

Next to `set_matcher` (line ~85):

```rust
    /// Inject a value the shell will resolve as `$name`.
    ///
    /// Rejects a name the lexer could not parse, rather than storing
    /// something unreachable: `is_valid_var_name` is the same rule the
    /// lexer applies, so anything accepted here is referenceable.
    pub fn set_host_var(&mut self, name: &str, value: Value) -> Result<(), ShellError> {
        if !crate::lex::is_valid_var_name(name) {
            return Err(ShellError::runtime(format!(
                "`{name}` is not a valid variable name: use letters, digits and `_`"
            )));
        }
        self.host_vars.insert(name.to_string(), value);
        Ok(())
    }

    /// Remove an injected value. Returns whether it was set, so a host can
    /// tell a real removal from a no-op without a second call.
    pub fn unset_host_var(&mut self, name: &str) -> bool {
        self.host_vars.remove(name).is_some()
    }

    pub fn host_var(&self, name: &str) -> Option<&Value> {
        self.host_vars.get(name)
    }

    /// The host layer alone. `scope_for_pane` is what merges it with a
    /// session's own variables.
    pub fn host_vars(&self) -> &Scope {
        &self.host_vars
    }
```

- [ ] **Step 5: Merge in `scope_for_pane`**

Replace the body of `scope_for_pane` (line ~600):

```rust
/// The variables a pipeline in `pane` can see.
///
/// The only place a `Scope` is built, which is why one merge here covers
/// interactive panes, `run()`, positional arguments, flag values, and
/// `"…$name…"` interpolation.
///
/// Host underneath, session on top: a session's own value wins. That map is
/// always empty today, so the `extend` is a no-op — but writing the
/// precedence now means shell-level assignment lands later without anyone
/// rereading this function.
///
/// Returns an owned clone, so a pipeline holds the values as they were when
/// its line started. A `set_host_var` mid-run is invisible to it and takes
/// effect on the next line. That is deliberate: under live lookup a single
/// pipeline could bind one name to two values in two stages.
fn scope_for_pane<A: EngineAccess>(access: &A, pane: u32) -> crate::signature::Scope {
    access.with(|e| {
        let mut scope = e.host_vars.clone();
        if let Some(session) = e
            .mux
            .session_of_pane(pane)
            .and_then(|sid| e.mux.sessions.get(&sid))
        {
            scope.extend(session.vars.clone());
        }
        scope
    })
}
```

- [ ] **Step 6: Run to verify they pass**

```bash
cargo test -p bterm-core --lib engine::tests
```

Expected: PASS.

- [ ] **Step 7: Add the snapshot test**

This one pins the freshness guarantee. It must fail if the implementation ever switches to live lookup.

```rust
    #[test]
    fn a_pipeline_keeps_the_values_its_line_started_with() {
        // The scope is cloned per line, so changing a variable while a
        // pipeline runs cannot change what that pipeline sees. Asserted on
        // the scope itself rather than through a timing-dependent command,
        // so it cannot go flaky.
        let engine = TestEngine::new();
        engine.with(|e| {
            e.set_host_var("x", Value::Int(1)).expect("valid");
        });
        let taken = scope_for_pane(&engine.access(), 0);
        engine.with(|e| {
            e.set_host_var("x", Value::Int(2)).expect("valid");
        });
        assert_eq!(
            taken.get("x"),
            Some(&Value::Int(1)),
            "an already-taken scope must not see a later write"
        );
        assert_eq!(
            scope_for_pane(&engine.access(), 0).get("x"),
            Some(&Value::Int(2)),
            "but the next line does"
        );
    }
```

- [ ] **Step 8: Full suite and clippy**

```bash
cargo test --workspace && cargo clippy --workspace --all-targets
```

Expected: 246 passed (239 + 7), clippy clean.

- [ ] **Step 9: Commit**

```bash
git add crates/bterm-core/src/engine.rs
git commit -m "Merge a host-supplied scope beneath every session

Engine gains host_vars beside matcher and fn_compiler, the two other
pieces of host-supplied state. scope_for_pane is the only place a Scope
is ever built and both execute_line and eval_to_value already call it, so
one merge covers panes, run(), argument binding and interpolation.

Host underneath, session on top. Session maps are always empty today so
the extend is a no-op, but the precedence is now written down and shell
assignment lands later without revisiting this function.

The clone that was already there becomes a documented guarantee: a
pipeline keeps the values its line started with."
```

---

## Task 3: `HostHooks::visible_vars` and the `vars` builtin

**Files:**
- Modify: `crates/bterm-core/src/registry.rs` (the `HostHooks` trait, around line 69)
- Modify: `crates/bterm-core/src/engine.rs` (`impl HostHooks for EngineHost`, line ~445)
- Modify: `crates/bterm-core/src/builtins/mod.rs` (registration ~line 162, implementation near `history` ~line 711, tests)

**Interfaces:**
- Consumes: `Engine::host_vars` and `scope_for_pane` from Task 2.
- Produces: `HostHooks::visible_vars(&self) -> Vec<(String, Value)>`, and a `vars` builtin emitting `List<Record{name, value}>` sorted by name.

Context: `history` is the model — a builtin that reads host state through `HostHooks` and returns a `Value` (`builtins/mod.rs:711`). `EngineHost` already carries `pane` (`engine.rs:440`), the same way `history()` reads that pane's editor.

Named `visible_vars`, not `host_vars`: `Engine::host_vars` returns the host layer alone, this returns the merged view. Two methods a keystroke apart returning different things is how things get mis-wired.

- [ ] **Step 1: Write the failing tests**

In `crates/bterm-core/src/builtins/mod.rs`, extend `TestHost` (line ~739) with:

```rust
        fn visible_vars(&self) -> Vec<(String, Value)> {
            vec![
                ("zebra".into(), Value::Int(2)),
                ("alpha".into(), Value::Str("first".into())),
            ]
        }
```

and add these tests:

```rust
    #[test]
    fn vars_lists_injected_variables_sorted_by_name() {
        let v = eval("vars").expect("eval");
        // Sorted, so the output is deterministic for a reader and for this
        // assertion. TestHost deliberately supplies them out of order.
        assert_eq!(
            v,
            Value::List(vec![
                Value::Record(
                    [("name".to_string(), Value::Str("alpha".into())),
                     ("value".to_string(), Value::Str("first".into()))]
                        .into_iter()
                        .collect()
                ),
                Value::Record(
                    [("name".to_string(), Value::Str("zebra".into())),
                     ("value".to_string(), Value::Int(2))]
                        .into_iter()
                        .collect()
                ),
            ])
        );
    }

    #[test]
    fn vars_pipes_like_any_other_table() {
        // The reason values are emitted whole rather than pre-truncated:
        // display shrinks a wide cell, the pipe keeps everything.
        let v = eval("vars | grep alpha | get value").expect("eval");
        assert_eq!(v, Value::Str("first".into()));
    }

    #[test]
    fn vars_with_nothing_injected_is_an_empty_table() {
        // Empty list, not an error and not Null — so `vars | length` is 0.
        // Same rule as an emptied stream in stream::collect.
        struct Bare;
        impl HostHooks for Bare {}
        let mut registry = CommandRegistry::new();
        register_all(&mut registry);
        let ctx = ExecContext {
            host: Rc::new(Bare),
            sink: Rc::new(crate::sink::NullSink),
            width: 80,
            pane: 0,
            run_id: 0,
        };
        let out = parse("vars | length");
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        let (mut results, error) = block_on(eval_line(&out.line, &registry, &ctx, &Scope::new()));
        assert!(error.is_none(), "{error:?}");
        assert_eq!(results.pop().map(|r| r.into_value()), Some(Value::Int(0)));
    }
```

- [ ] **Step 2: Run to verify they fail**

```bash
cargo test -p bterm-core --lib builtins::tests::vars
```

Expected: FAIL — `unknown command 'vars'`, and `visible_vars` is not a member of `HostHooks`.

- [ ] **Step 3: Add the hook**

In `crates/bterm-core/src/registry.rs`, inside `trait HostHooks`, next to `history`:

```rust
    /// Variables visible to a pipeline here, as `$name` would resolve them:
    /// the host's injected values with any session-local ones layered on
    /// top. Named for the question it answers — `Engine::host_vars` returns
    /// only the host layer.
    fn visible_vars(&self) -> Vec<(String, Value)> {
        Vec::new()
    }
```

The native CLI inherits this empty default, so `vars` there prints an empty table. That is correct, not a gap.

- [ ] **Step 4: Implement it on `EngineHost`**

In `crates/bterm-core/src/engine.rs`, inside `impl<A: EngineAccess> HostHooks for EngineHost<A>` (line ~445), beside `history`:

```rust
    fn visible_vars(&self) -> Vec<(String, Value)> {
        scope_for_pane(&self.access, self.pane).into_iter().collect()
    }
```

- [ ] **Step 5: Implement the builtin**

In `crates/bterm-core/src/builtins/mod.rs`, next to `history` (line ~711):

```rust
fn vars(ctx: ExecContext, _call: BoundCall, _input: PipelineData) -> Result<PipelineData, ShellError> {
    let mut pairs = ctx.host.visible_vars();
    // Sorted so the table is stable between runs: a HashMap iterates in an
    // arbitrary order, which would make the output shuffle and any test
    // asserting on it flaky.
    pairs.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(PipelineData::Value(Value::List(
        pairs
            .into_iter()
            .map(|(name, value)| {
                Value::Record(
                    [("name".to_string(), Value::Str(name)), ("value".to_string(), value)]
                        .into_iter()
                        .collect(),
                )
            })
            .collect(),
    )))
}
```

Register it next to `history` (line ~162):

```rust
    registry.register_builtin(cmd(
        Signature::build("vars", "List variables the host injected"),
        vars,
    ));
```

- [ ] **Step 6: Run to verify they pass**

```bash
cargo test -p bterm-core --lib builtins::tests::vars
```

Expected: PASS. If the `Value::Record` construction does not compile, check whether `Record` wraps an `IndexMap` rather than a `HashMap` and adjust the collect — do not change the assertion.

- [ ] **Step 7: Full suite and clippy**

```bash
cargo test --workspace && cargo clippy --workspace --all-targets
```

Expected: 249 passed (246 + 3), clippy clean.

- [ ] **Step 8: Commit**

```bash
git add crates/bterm-core/src/registry.rs crates/bterm-core/src/engine.rs crates/bterm-core/src/builtins/mod.rs
git commit -m "Add a vars builtin listing what the shell can see

Reached through HostHooks, the existing seam for host services a command
may touch, rather than by adding a scope to ExecContext -- that would
hand every command the variables whether it needs them or not.

visible_vars returns the merged view, so vars answers 'what would \$name
resolve to here', while Engine::host_vars stays the host layer alone.

Values are emitted whole and the renderer truncates wide cells, so
\`vars | get value\` returns the full document. Sorted by name, because a
HashMap iterates arbitrarily and the output would otherwise shuffle."
```

---

## Task 4: The wasm boundary

**Files:**
- Modify: `crates/bterm-wasm/src/lib.rs` (`impl BtermCore`, next to `register_fn` at line ~366)
- Test: `crates/bterm-wasm/tests/boundary.rs`

**Interfaces:**
- Consumes: `Engine::{set_host_var, unset_host_var, host_var, host_vars}` from Task 2.
- Produces, on `BtermCore`:
  - `set_variable(&self, name: &str, value: JsValue) -> Result<(), JsValue>`
  - `set_variables(&self, values: JsValue) -> Result<(), JsValue>`
  - `unset_variable(&self, name: &str) -> bool`
  - `get_variable(&self, name: &str) -> JsValue` — `undefined` when unset
  - `variables(&self) -> JsValue` — a plain object

Context: every export takes `&self` (never `&mut self`) — that is what makes the `RefCell` reentrancy panic unreachable by construction. `js_to_value` and `value_to_js` already exist in `crates/bterm-wasm/src/convert.rs` and are the same path command arguments take.

**Watch the engine rule:** `js_to_value` may call into JS. Do every conversion *before* opening a `WasmAccess.with` borrow, never inside one.

- [ ] **Step 1: Write the failing tests**

Add to `crates/bterm-wasm/tests/boundary.rs`, following the file's existing style for constructing a `BtermCore`:

```rust
#[wasm_bindgen_test]
fn a_host_variable_reaches_a_command_as_an_argument() {
    let core = new_core();
    core.set_variable("greeting", JsValue::from_str("hello")).expect("valid name");
    let out = run_sync(&core, "echo $greeting");
    assert_eq!(out.as_string().as_deref(), Some("hello"));
}

#[wasm_bindgen_test]
fn typed_values_survive_the_round_trip() {
    let core = new_core();
    // A record, a list, an int and a null: the shapes a host actually
    // injects. Text would round-trip trivially; these are what would break
    // if the boundary reached for source text instead of Values.
    let obj = js_sys::Object::new();
    js_sys::Reflect::set(&obj, &"k".into(), &JsValue::from_f64(1.0)).expect("set");
    core.set_variable("rec", obj.into()).expect("valid");
    core.set_variable("num", JsValue::from_f64(42.0)).expect("valid");
    core.set_variable("nothing", JsValue::NULL).expect("valid");

    let rec = core.get_variable("rec");
    assert!(rec.is_object(), "a record must come back an object, got {rec:?}");
    assert_eq!(core.get_variable("num").as_f64(), Some(42.0));
    assert!(core.get_variable("nothing").is_null(), "a set null stays null");
}

#[wasm_bindgen_test]
fn unset_is_undefined_and_is_not_the_same_as_null() {
    let core = new_core();
    core.set_variable("nothing", JsValue::NULL).expect("valid");
    // The distinction the host needs: `sim: null` was injected on purpose,
    // `missing` never existed. If both returned null, unsetVariable would
    // be unobservable through the getter.
    assert!(core.get_variable("nothing").is_null(), "set-to-null reads back null");
    assert!(core.get_variable("missing").is_undefined(), "never-set reads back undefined");

    assert!(core.unset_variable("nothing"), "removing a set name reports true");
    assert!(core.get_variable("nothing").is_undefined(), "and it is gone");
    assert!(!core.unset_variable("nothing"), "removing it again reports false");
}

#[wasm_bindgen_test]
fn an_invalid_name_throws_and_stores_nothing() {
    let core = new_core();
    assert!(core.set_variable("a b", JsValue::from_str("x")).is_err());
    assert!(core.get_variable("a b").is_undefined());
}

#[wasm_bindgen_test]
fn set_variables_applies_all_or_nothing() {
    let core = new_core();
    let obj = js_sys::Object::new();
    js_sys::Reflect::set(&obj, &"good".into(), &JsValue::from_str("a")).expect("set");
    js_sys::Reflect::set(&obj, &"bad name".into(), &JsValue::from_str("b")).expect("set");

    assert!(core.set_variables(obj.into()).is_err(), "one bad name fails the batch");
    // The point of validating first: a partial apply would leave the host
    // with some editors landed and some stale, which looks plausible and
    // is wrong.
    assert!(
        core.get_variable("good").is_undefined(),
        "nothing may be applied when the batch is rejected"
    );
}
```

If `new_core` / `run_sync` helpers do not exist in that file, write the smallest ones needed and say so in your report.

- [ ] **Step 2: Run to verify they fail**

```bash
just test-wasm
```

Expected: FAIL — no method `set_variable`.

- [ ] **Step 3: Implement**

In `crates/bterm-wasm/src/lib.rs`, after `unregister_fn` (line ~381):

```rust
    /// Inject a value the shell resolves as `$name`, for every session and
    /// every pane, including ones created later.
    ///
    /// Throws on a name the shell could not reference. The value crosses as
    /// a typed `Value` and is never parsed as shell source, so a string
    /// containing `; rm -rf /` is that string and cannot become syntax.
    ///
    /// Takes effect from the next command line: a pipeline already running
    /// keeps the values it started with.
    pub fn set_variable(&self, name: &str, value: JsValue) -> Result<(), JsValue> {
        // Convert before borrowing: js_to_value can reach into JS, and no
        // JS call may happen inside an engine borrow.
        let converted = convert::js_to_value(&value).map_err(|e| JsValue::from_str(&e))?;
        WasmAccess
            .with(|e| e.set_host_var(name, converted))
            .map_err(|err| JsValue::from_str(&err.msg))
    }

    /// Replace several variables at once.
    ///
    /// Every name is validated before anything is applied, so one bad name
    /// leaves the previous state untouched. A partial apply is the worst
    /// outcome for a host synchronising editor state: some values land,
    /// one stays stale, and the next command runs against a mix that looks
    /// plausible.
    pub fn set_variables(&self, values: JsValue) -> Result<(), JsValue> {
        let obj: js_sys::Object = values
            .dyn_into()
            .map_err(|_| JsValue::from_str("setVariables expects an object"))?;

        // Convert and validate everything first — still outside any borrow.
        let mut pending: Vec<(String, bterm_core::value::Value)> = Vec::new();
        for entry in js_sys::Object::entries(&obj).iter() {
            let pair: js_sys::Array = entry.into();
            let name = pair.get(0).as_string().unwrap_or_default();
            if !bterm_core::lex::is_valid_var_name(&name) {
                return Err(JsValue::from_str(&format!(
                    "`{name}` is not a valid variable name: use letters, digits and `_`"
                )));
            }
            let value = convert::js_to_value(&pair.get(1)).map_err(|e| JsValue::from_str(&e))?;
            pending.push((name, value));
        }

        WasmAccess.with(|e| {
            for (name, value) in pending {
                // Already validated above; the engine re-checks and the
                // error is unreachable, so drop it rather than unwrap.
                let _ = e.set_host_var(&name, value);
            }
        });
        Ok(())
    }

    /// Remove an injected variable. Returns whether it was set.
    pub fn unset_variable(&self, name: &str) -> bool {
        WasmAccess.with(|e| e.unset_host_var(name))
    }

    /// The value of an injected variable, or `undefined` if it is not set.
    ///
    /// `undefined` rather than null: a host can legitimately inject null,
    /// and the two must stay distinguishable.
    pub fn get_variable(&self, name: &str) -> JsValue {
        let found = WasmAccess.with(|e| e.host_var(name).cloned());
        match found {
            Some(v) => convert::value_to_js(&v),
            None => JsValue::UNDEFINED,
        }
    }

    /// Everything the host injected, as a plain object.
    ///
    /// The host layer only — the read-back of what this API set, so
    /// `setVariables(x)` then `variables()` round-trips. The shell's `vars`
    /// command shows the merged view instead, because it answers a
    /// different question: what `$name` resolves to here.
    pub fn variables(&self) -> JsValue {
        let pairs = WasmAccess.with(|e| {
            e.host_vars()
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect::<Vec<_>>()
        });
        let obj = js_sys::Object::new();
        for (name, value) in pairs {
            let _ = js_sys::Reflect::set(&obj, &JsValue::from_str(&name), &convert::value_to_js(&value));
        }
        obj.into()
    }
```

Add `use wasm_bindgen::JsCast;` to the imports if `dyn_into` does not resolve.

- [ ] **Step 4: Run to verify they pass**

```bash
just test-wasm
```

Expected: 24 passed (19 baseline + 5).

- [ ] **Step 5: Clippy for the wasm target**

```bash
cargo clippy --workspace --all-targets && cargo clippy -p bterm-wasm --target wasm32-unknown-unknown
```

Expected: clean. Confirm no `unwrap`/`expect` reached non-test code.

- [ ] **Step 6: Commit**

```bash
git add crates/bterm-wasm/src/lib.rs crates/bterm-wasm/tests/boundary.rs
git commit -m "Expose host variables across the wasm boundary

Five methods, all &self like every other export, so the RefCell
reentrancy panic stays unreachable by construction. Conversions happen
before the engine borrow opens, because js_to_value can call into JS and
no JS call may occur inside a borrow.

get_variable returns undefined for unset rather than null: a host can
inject null on purpose, and if both read back null then unsetVariable
would be unobservable. set_variables validates the whole batch before
applying any of it -- a partial apply leaves some editors landed and one
stale, which looks plausible and is wrong."
```

---

## Task 5: The TypeScript surface

**Files:**
- Modify: `packages/browser-terminal/src/index.ts` (`BrowserTerminal`, after `unregisterFn` at line ~206)
- Modify: `packages/browser-terminal/README.md`

**Interfaces:**
- Consumes: the five `BtermCore` methods from Task 4.
- Produces, on `BrowserTerminal`: `setVariable`, `setVariables`, `unsetVariable`, `getVariable`, `variables`.

Context: every public method calls `this.assertLive()` first (see `registerCommand`, line ~179). `Value` is already exported from `./types.js`.

- [ ] **Step 1: Implement**

In `packages/browser-terminal/src/index.ts`, after `unregisterFn`:

```ts
  /**
   * Inject a value the shell resolves as `$name` — how a host page passes
   * its own state to a command without serializing it into the command
   * text or inventing a filename:
   *
   * ```ts
   * bt.setVariable('game', gameDefinition);
   * await bt.run('rtce evaluate --game $game');
   * ```
   *
   * Visible to every session and pane, including ones created later, and
   * inside string interpolation (`"level-$game"`).
   *
   * Takes effect from the next command line: a pipeline already running
   * keeps the values it started with, so a long command cannot see one of
   * its arguments change underneath it.
   *
   * Throws if `name` is not usable as `$name` (letters, digits and `_`).
   * The value is stored as a typed value and never parsed as shell source.
   */
  setVariable(name: string, value: Value): void {
    this.assertLive();
    this.core.set_variable(name, value as unknown);
  }

  /**
   * Replace several variables at once. Every name is validated before any
   * value is applied, so a bad name leaves the previous state untouched —
   * a half-applied batch would leave the shell running against a mix of
   * fresh and stale state.
   */
  setVariables(values: Record<string, Value>): void {
    this.assertLive();
    this.core.set_variables(values as unknown);
  }

  /** Remove an injected variable. Returns whether it was set. */
  unsetVariable(name: string): boolean {
    this.assertLive();
    return this.core.unset_variable(name);
  }

  /**
   * The value of an injected variable, or `undefined` if it is not set.
   *
   * `undefined` rather than `null`, because `null` is itself a legal value
   * to inject — the two have to stay distinguishable.
   */
  getVariable(name: string): Value | undefined {
    this.assertLive();
    const v = this.core.get_variable(name);
    return v === undefined ? undefined : (v as Value);
  }

  /**
   * Everything injected through this API, as a plain object.
   *
   * The host layer only, so `setVariables(x)` then `variables()` round
   * trips. The shell's `vars` command answers a different question — what
   * `$name` resolves to in a given pane — and shows the merged view.
   */
  variables(): Record<string, Value> {
    this.assertLive();
    return this.core.variables() as Record<string, Value>;
  }
```

- [ ] **Step 2: Build and type-check**

```bash
just build && just typecheck
```

Expected: both clean. If the generated `bterm_wasm.d.ts` types the wasm methods as `any`, the casts above are what bridge them; if it types them precisely, drop the `as unknown` casts rather than leaving a needless one.

- [ ] **Step 3: Document it in the package README**

In `packages/browser-terminal/README.md`, after the "Writing commands" section, add:

````markdown
## Host state as shell variables

Inject application state and reference it as `$name`, instead of pasting it
into the command text:

```ts
bt.setVariables({
  game: gameDefinition,      // any Value: string, number, record, list, null
  build: currentBuild,
});

await bt.run('simulate --game $game --build $build');
```

Variables are visible in every pane and session, including ones created
afterwards, and inside interpolation (`"run-$game"`). Values cross as typed
values and are never parsed as shell source, so a string containing `;` or
`|` stays a string.

A change takes effect from the next command line — a pipeline already
running keeps the values it started with, so a long command cannot see one
of its arguments change halfway through.

```ts
bt.getVariable('game');     // the value, or undefined if unset
bt.variables();             // everything you injected
bt.unsetVariable('game');   // true if it was set
```

`undefined` from `getVariable` means "not set"; `null` means you injected
`null`. In the terminal, `vars` lists what is visible and pipes like any
other table (`vars | grep game`).
````

- [ ] **Step 4: Commit**

```bash
git add packages/browser-terminal/src/index.ts packages/browser-terminal/README.md
git commit -m "Add the host-variable API to BrowserTerminal

Five methods over the wasm surface, each asserting the instance is live
like every other public method. The README example is the one a consumer
needs: inject state, reference it as \$name, and the note that undefined
from getVariable means unset while null means you injected null."
```

---

## Task 6: Demo and browser proof

**Files:**
- Modify: `packages/demo/src/main.ts`
- Modify: `packages/demo/index.html`
- Modify: `packages/demo/tests/smoke.spec.ts`

**Interfaces:**
- Consumes: the five `BrowserTerminal` methods from Task 5, and the `vars` builtin from Task 3.
- Produces: nothing later tasks depend on.

Context: the demo uses `// #region <name>` / `// #endregion` markers that `codePanel` slices out of the page's own source. Existing regions are `links`, `slow`, `selector`, `watch`, `progress`. Panels are registered around line 146.

- [ ] **Step 1: Add the demo**

In `packages/demo/src/main.ts`, inside `main()`:

```ts
  // #region variables
  // Host state as shell variables: the page owns `$project`, the shell
  // reads it. Nothing is serialized into the command text, so the value
  // can be a whole object rather than something that survives quoting.
  bt.setVariables({
    project: { name: 'browser-terminal', language: 'Rust + TypeScript' },
    greeting: 'hello',
  });

  bt.registerCommand(
    {
      name: 'describe',
      summary: 'Describe a project record passed as a variable',
      required: [{ name: 'project', shape: 'any', desc: 'usually $project' }],
    },
    ({ positionals }) => {
      const p = positionals[0] as { name?: string; language?: string } | null;
      return p ? `${p.name} — ${p.language}` : 'nothing to describe';
    },
  );
  // #endregion
```

Add a panel next to the others: `codePanel('Host state as $variables', selfSource, 'variables'),`

- [ ] **Step 2: Show it in the Try block**

In `packages/demo/index.html`, add to the Try `<pre>`, matching the existing alignment:

```
vars               # what the host injected
describe $project  # a record, passed as a variable
```

- [ ] **Step 3: Write the browser tests**

Add to `packages/demo/tests/smoke.spec.ts`:

```ts
test('host variables reach commands, interpolation, and run()', async ({ page }) => {
  await page.goto('/');
  await waitForTerminal(page);

  const result = await page.evaluate(async () => {
    window.bt.setVariable('greeting', 'hello');
    window.bt.setVariable('n', 3);

    const described = await window.bt.run('describe $project');
    const interpolated = await window.bt.run('echo "say-$greeting"');
    const asFlag = await window.bt.run('echo $n');
    const listed = await window.bt.run('vars | grep greeting | get value');

    return {
      described: described.value,
      interpolated: interpolated.value,
      asFlag: asFlag.value,
      listed: listed.value,
      readBack: window.bt.getVariable('greeting'),
      missing: window.bt.getVariable('nope'),
      removed: window.bt.unsetVariable('greeting'),
      afterRemoval: window.bt.getVariable('greeting'),
    };
  });

  expect(result.described).toContain('browser-terminal');
  expect(result.interpolated).toBe('say-hello');
  expect(result.asFlag).toBe(3);
  expect(result.listed).toBe('hello');
  expect(result.readBack).toBe('hello');
  // undefined, not null — null is a value a host can inject on purpose.
  expect(result.missing).toBeUndefined();
  expect(result.removed).toBe(true);
  expect(result.afterRemoval).toBeUndefined();
});

test('an unset variable keeps its positioned diagnostic', async ({ page }) => {
  await page.goto('/');
  await waitForTerminal(page);

  const message = await page.evaluate(async () => {
    try {
      await window.bt.run('echo $definitely_not_set');
      return 'should have rejected';
    } catch (e) {
      return (e as Error).message;
    }
  });

  expect(message).toContain('unknown variable `$definitely_not_set`');
});

test('a running pipeline keeps the variable values its line started with', async ({ page }) => {
  await page.goto('/');
  await waitForTerminal(page);

  const result = await page.evaluate(async () => {
    window.bt.setVariable('doc', 'original');
    window.bt.registerCommand(
      { name: 'slow-echo', summary: 'echo after a delay', required: [{ name: 'text' }] },
      async ({ positionals }) => {
        await new Promise((r) => setTimeout(r, 300));
        return positionals[0];
      },
    );

    const inFlight = window.bt.run('slow-echo $doc');
    // Change it while the command is suspended. Under live lookup this
    // would win; under the snapshot rule the running line keeps 'original'.
    await new Promise((r) => setTimeout(r, 50));
    window.bt.setVariable('doc', 'changed');

    const finished = await inFlight;
    const next = await window.bt.run('slow-echo $doc');
    return { finished: finished.value, next: next.value };
  });

  expect(result.finished).toBe('original');
  expect(result.next).toBe('changed');
});
```

- [ ] **Step 4: Teeth-check the snapshot test**

This is the one test that could pass while proving nothing, so verify it can fail. Temporarily change `scope_for_pane` in `crates/bterm-core/src/engine.rs` so the scope is re-read per stage rather than cloned once — the simplest way is to have the `slow-echo` command's argument resolve late; if that is impractical, instead assert the inverse (`expect(result.finished).toBe('changed')`) and confirm *that* fails against the real implementation.

Run:

```bash
just build && npm --prefix packages/demo run build && cd packages/demo && npx playwright test -g "keeps the variable values"
```

Record both outputs (failing and passing) in your report. Restore afterwards.

- [ ] **Step 5: Full verification**

```bash
cargo test --workspace
just test-wasm
just build && just typecheck
npm --prefix packages/demo run build
cd packages/demo && npx playwright test
```

Expected: 249 native, 24 wasm, 19 Playwright (16 baseline + 3).

- [ ] **Step 6: Commit**

```bash
git add packages/demo/src/main.ts packages/demo/index.html packages/demo/tests/smoke.spec.ts
git commit -m "Demo host variables, and prove the snapshot rule in a browser

The demo injects a record and a string and reads them back through a
registered command, so the feature is visible rather than only described.

Three browser tests: variables as arguments, in interpolation and through
run(); the unknown-variable diagnostic surviving an unset; and the one
that matters most -- a command started before a setVariable finishes with
the value its line began with, while the next line sees the new one. That
last one was teeth-checked, because a snapshot test that cannot fail
documents nothing."
```

---

## Self-review notes

**Spec coverage.** Engine-wide scope with session override (Task 2); snapshot freshness, documented and tested (Task 2 step 7, Task 6 step 3); validation reusing the lexer rule (Task 1, used in Tasks 2 and 4); five API methods with `undefined`-for-unset and all-or-nothing batching (Tasks 4–5); `vars` via `HostHooks::visible_vars`, sorted, empty-as-`[]`, values whole (Task 3); README and demo (Tasks 5–6); native, boundary and browser tests throughout.

**Deliberately not included.** Injecting variables through `create()` options — the spec leaves it out, and a host can call `setVariables` immediately after `create()`. Raise it before implementing if rtce needs values live before the first prompt paints.

**Risks worth naming.**
- Task 2's tests assume test helpers whose exact names I could not verify (`TestEngine`, `run_line`). The task says to write the smallest helpers needed rather than skip a test — an implementer who silently drops a test here has removed the coverage this feature most needs.
- `Value::Record`'s inner map may be an `IndexMap` rather than a `HashMap`; Task 3 step 6 says to adjust the collect, not the assertion.
- The demo's `describe` command takes `shape: 'any'`. If binding rejects a record for an `any` positional, that is a real finding about the binder, not something to work around by changing the demo to pass a string.
