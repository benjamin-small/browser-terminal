# Session-Scoped Variables Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let a host page inject variables scoped to one session, addressed by explicit id, shadowing the engine-wide host layer — and make that shadowing visible in the shell.

**Architecture:** `Session.vars` already exists and `scope_for_pane` already layers it over `host_vars` with session winning; that `extend` has been a no-op only because nothing could populate it. This adds `Engine` accessors keyed by `SessionId`, an optional `opts` argument on the five public methods selecting a layer, and a `scope` column on `vars`.

**Tech Stack:** Rust (`bterm-core`, `bterm-wasm`), `wasm-bindgen`, TypeScript, Vite, Playwright.

## Global Constraints

- Spec: `docs/superpowers/specs/2026-08-10-host-variables-design.md`, section **Increment 2 — session scope**. Read it if a decision here looks arbitrary.
- **Engine access rule:** engine state is reached only inside a synchronous `access.with(|e| …)` / `WasmAccess.with(|e| …)` closure. No borrow crosses an `.await`, and **no JS call happens inside one** — `js_to_value`, `value_to_js` and `js_sys::Reflect::*` are JS calls. A violation is a `RefCell` double-borrow, and with `panic = "abort"` that kills the module. Convert first, then borrow.
- **Sessions are addressed by explicit id, never the implicit active session.** An unknown id is an error, not a no-op.
- **Reads return one layer, never a merged view.** The merged question is what the shell's `vars` answers.
- Omitting `opts` means the host layer, so every existing call site keeps working unchanged.
- Name validation reuses `crate::lex::is_valid_var_name` — never a hand-written pattern. It is Unicode-aware: `café` and `1` are legal.
- No `unwrap`/`expect` in non-test code. Clippy denies `unwrap_used` on core and wasm.
- Baseline: **250 native, 28 wasm, 19 Playwright**, clippy clean on `--workspace --all-targets` and `-p bterm-wasm --target wasm32-unknown-unknown`.
- Do not weaken an existing test. If one breaks, that is a finding — report it.
- Stage only the files a task names. Never `git add -A`.

---

## File Structure

| File | Responsibility |
| --- | --- |
| `crates/bterm-core/src/engine.rs` | session-var accessors keyed by `SessionId`; `EngineHost::visible_vars` labelling origin |
| `crates/bterm-core/src/registry.rs` | `VarOrigin`, `VisibleVar`, the changed `HostHooks::visible_vars` |
| `crates/bterm-core/src/builtins/mod.rs` | the `scope` column on `vars` |
| `crates/bterm-wasm/src/lib.rs` | `opts` parsing + the five methods |
| `packages/browser-terminal/src/index.ts` | `VarScope` type + `opts` on five methods |
| `packages/browser-terminal/README.md` | session-scope example |
| `packages/demo/src/main.ts`, `index.html` | demo |
| `packages/demo/tests/smoke.spec.ts` | browser proof |

---

## Task 1: Session-var accessors on `Engine`

**Files:**
- Modify: `crates/bterm-core/src/engine.rs` — next to `set_host_var` (line ~115)

**Interfaces:**
- Consumes: `crate::lex::is_valid_var_name`; `Session.vars` (`mux/mod.rs:76`); `Mux.sessions: IndexMap<SessionId, Session>`.
- Produces, on `Engine`:
  - `pub fn set_session_var(&mut self, session: SessionId, name: &str, value: Value) -> Result<(), ShellError>`
  - `pub fn unset_session_var(&mut self, session: SessionId, name: &str) -> Result<bool, ShellError>`
  - `pub fn session_var(&self, session: SessionId, name: &str) -> Result<Option<&Value>, ShellError>`
  - `pub fn session_vars(&self, session: SessionId) -> Result<&Scope, ShellError>`

Note every one returns `Result`, including the reads: an unknown id is an error in all four, so a host cannot mistake "session gone" for "name not set". `SessionId` is `u32` (`mux/mod.rs:14`).

- [ ] **Step 1: Write the failing tests**

Add to the `#[cfg(test)] mod tests` block in `crates/bterm-core/src/engine.rs`. Use the module's existing helpers — `engine()` returns `Rc<RefCell<Engine>>` (which *is* the `EngineAccess`), `active_pane(&access)`, and `run_line(&access, line)` added in the previous increment.

```rust
    #[test]
    fn a_session_variable_resolves_in_that_session() {
        let access = engine();
        let sid = access.with(|e| e.mux.active_session);
        access.with(|e| {
            e.set_session_var(sid, "scratch", Value::Str("mine".into())).expect("valid");
        });
        assert_eq!(run_line(&access, "echo $scratch").expect("resolves"), Value::Str("mine".into()));
    }

    #[test]
    fn a_session_variable_shadows_the_host_one_without_destroying_it() {
        let access = engine();
        let sid = access.with(|e| e.mux.active_session);
        access.with(|e| {
            e.set_host_var("x", Value::Str("host".into())).expect("valid");
            e.set_session_var(sid, "x", Value::Str("session".into())).expect("valid");
        });
        assert_eq!(run_line(&access, "echo $x").expect("resolves"), Value::Str("session".into()));
        // The host layer is untouched — shadowing hides a value, it does not
        // overwrite one. Reading the layer directly is how a host tells the
        // difference, since a read never merges.
        access.with(|e| {
            assert_eq!(e.host_var("x"), Some(&Value::Str("host".into())));
            assert_eq!(e.session_var(sid, "x").expect("live session"), Some(&Value::Str("session".into())));
        });
    }

    #[test]
    fn unsetting_a_session_variable_reveals_the_host_one_again() {
        let access = engine();
        let sid = access.with(|e| e.mux.active_session);
        access.with(|e| {
            e.set_host_var("x", Value::Str("host".into())).expect("valid");
            e.set_session_var(sid, "x", Value::Str("session".into())).expect("valid");
            assert!(e.unset_session_var(sid, "x").expect("live session"));
            assert!(!e.unset_session_var(sid, "x").expect("live session"), "already gone");
        });
        assert_eq!(run_line(&access, "echo $x").expect("resolves"), Value::Str("host".into()));
    }

    #[test]
    fn an_unknown_session_id_errors_rather_than_silently_doing_nothing() {
        let access = engine();
        access.with(|e| {
            let missing: crate::mux::SessionId = 9999;
            let err = e
                .set_session_var(missing, "x", Value::Int(1))
                .expect_err("no such session");
            assert!(err.msg.contains("9999"), "the error should name the id: {}", err.msg);
            assert!(e.unset_session_var(missing, "x").is_err());
            assert!(e.session_var(missing, "x").is_err());
            assert!(e.session_vars(missing).is_err());
        });
    }

    #[test]
    fn an_invalid_session_variable_name_is_rejected_by_the_lexer_rule() {
        let access = engine();
        let sid = access.with(|e| e.mux.active_session);
        access.with(|e| {
            assert!(e.set_session_var(sid, "a b", Value::Int(1)).is_err());
            // Names only the real rule allows, so an ASCII-only pattern
            // creeping in here is caught. `is_alphanumeric` is Unicode-aware
            // and there is no leading-character restriction.
            e.set_session_var(sid, "café", Value::Int(7)).expect("unicode letters are var chars");
            e.set_session_var(sid, "1", Value::Int(8)).expect("a leading digit is legal");
        });
        assert_eq!(run_line(&access, "echo $café").expect("resolves"), Value::Int(7));
        assert_eq!(run_line(&access, "echo $1").expect("resolves"), Value::Int(8));
    }
```

- [ ] **Step 2: Run to verify they fail**

```bash
cargo test -p bterm-core --lib engine::tests::a_session_variable
```

Expected: FAIL — no method `set_session_var`.

- [ ] **Step 3: Implement**

In `crates/bterm-core/src/engine.rs`, after `host_vars()` (line ~139):

```rust
    /// Inject a value visible only in `session`, shadowing any host value of
    /// the same name.
    ///
    /// The id comes from the host's snapshot rather than being implied by
    /// whichever session is active: an ambient target would land the value
    /// somewhere else entirely if the user switched sessions between the
    /// host's read and its write, and nothing would say so.
    pub fn set_session_var(
        &mut self,
        session: crate::mux::SessionId,
        name: &str,
        value: Value,
    ) -> Result<(), ShellError> {
        if !crate::lex::is_valid_var_name(name) {
            return Err(ShellError::runtime(format!(
                "`{name}` is not a valid variable name: use letters, digits and `_`"
            )));
        }
        let s = self
            .mux
            .sessions
            .get_mut(&session)
            .ok_or_else(|| ShellError::runtime(format!("no session with id {session}")))?;
        s.vars.insert(name.to_string(), value);
        Ok(())
    }

    /// Remove a session-scoped value, revealing any host value it shadowed.
    /// `Ok(false)` means the session exists but the name was not set;
    /// `Err` means the session does not exist. Keeping those distinct is
    /// the point — a host must not read "session gone" as "already unset".
    pub fn unset_session_var(
        &mut self,
        session: crate::mux::SessionId,
        name: &str,
    ) -> Result<bool, ShellError> {
        let s = self
            .mux
            .sessions
            .get_mut(&session)
            .ok_or_else(|| ShellError::runtime(format!("no session with id {session}")))?;
        Ok(s.vars.remove(name).is_some())
    }

    /// One session's own value, ignoring the host layer. Reads name a layer
    /// and never merge, so this answers "did I set it here?" rather than
    /// "what would `$name` be?" — the latter is what the shell's `vars` is for.
    pub fn session_var(
        &self,
        session: crate::mux::SessionId,
        name: &str,
    ) -> Result<Option<&Value>, ShellError> {
        Ok(self.session_vars(session)?.get(name))
    }

    /// One session's own layer, ignoring the host one.
    pub fn session_vars(&self, session: crate::mux::SessionId) -> Result<&Scope, ShellError> {
        self.mux
            .sessions
            .get(&session)
            .map(|s| &s.vars)
            .ok_or_else(|| ShellError::runtime(format!("no session with id {session}")))
    }
```

- [ ] **Step 4: Run to verify they pass**

```bash
cargo test -p bterm-core --lib engine::tests
```

- [ ] **Step 5: Teeth-check the shadowing test**

Reverse the merge in `scope_for_pane` (build from `session.vars` and extend with `host_vars`) and confirm `a_session_variable_shadows_the_host_one_without_destroying_it` fails on its `run_line` assertion. Restore. Report both outputs verbatim. If it does not fail, the test is not pinning precedence and you should say so.

- [ ] **Step 6: Full suite and clippy**

```bash
cargo test --workspace && cargo clippy --workspace --all-targets
```

Expected: 255 passed (250 + 5), clippy clean.

- [ ] **Step 7: Commit**

```bash
git add crates/bterm-core/src/engine.rs
git commit -m "Address session variables by explicit id

Session.vars has existed since the mux landed and scope_for_pane has
layered it over the host map since the host scope did; the extend was a
no-op only because nothing could populate it. These four accessors are
what populate it.

The id is the host's, from its snapshot, rather than whichever session
happens to be active: an ambient target lands the value somewhere else
entirely if the user switches sessions between the host's read and its
write, and nothing would say so.

Every accessor returns Result, reads included, so a host cannot mistake a
dead session for an unset name."
```

---

## Task 2: Origin-labelled `visible_vars` and the `scope` column

**Files:**
- Modify: `crates/bterm-core/src/registry.rs` (`HostHooks`, line ~78)
- Modify: `crates/bterm-core/src/engine.rs` (`EngineHost::visible_vars`, line ~489)
- Modify: `crates/bterm-core/src/builtins/mod.rs` (`vars`, line ~721; `TestHost`, line ~769; tests)

**Interfaces:**
- Consumes: `Engine::host_vars`, `Engine::session_vars` from Task 1; `Mux::session_of_pane`.
- Produces, in `crate::registry`:
  - `pub enum VarOrigin { Host, Session }`
  - `pub struct VisibleVar { pub name: String, pub origin: VarOrigin, pub value: Value }`
  - `fn visible_vars(&self) -> Vec<VisibleVar>` replacing the `Vec<(String, Value)>` form.

**One semantic decision to implement exactly.** A host value shadowed by a session value of the same name produces **one row, labelled `session`** — not two rows. `vars` answers "what would `$name` resolve to here", so a name appears once. A host that needs the hidden value reads it through its own layer with `getVariable`.

This is a breaking change to a public trait on `bterm-core`. No out-of-tree implementors exist today; the in-tree ones are `EngineHost`, the CLI's host (which inherits the default), and test doubles.

- [ ] **Step 1: Write the failing tests**

In `crates/bterm-core/src/builtins/mod.rs`, replace `TestHost`'s `visible_vars` with:

```rust
        fn visible_vars(&self) -> Vec<crate::registry::VisibleVar> {
            use crate::registry::{VarOrigin, VisibleVar};
            // Deliberately out of order, so the sort in `vars` is doing real
            // work, and deliberately mixed-origin so the label is too.
            vec![
                VisibleVar {
                    name: "zebra".into(),
                    origin: VarOrigin::Session,
                    value: Value::Int(2),
                },
                VisibleVar {
                    name: "alpha".into(),
                    origin: VarOrigin::Host,
                    value: Value::Str("first".into()),
                },
            ]
        }
```

Update `vars_lists_injected_variables_sorted_by_name` to the new shape and add a label test:

```rust
    #[test]
    fn vars_lists_injected_variables_sorted_by_name() {
        let v = eval("vars").expect("eval");
        assert_eq!(
            v,
            Value::List(vec![
                Value::record([
                    ("name".to_string(), Value::Str("alpha".into())),
                    ("scope".to_string(), Value::Str("host".into())),
                    ("value".to_string(), Value::Str("first".into())),
                ]),
                Value::record([
                    ("name".to_string(), Value::Str("zebra".into())),
                    ("scope".to_string(), Value::Str("session".into())),
                    ("value".to_string(), Value::Int(2)),
                ]),
            ])
        );
    }

    #[test]
    fn vars_labels_where_each_value_came_from() {
        // The column exists so a shadowed value is visible at the moment
        // someone asks why `$x` is not what they set. Filtering on it is
        // the point, so it has to pipe.
        let v = eval("vars | filter {|r| $r.scope == 'session'} | get name").expect("eval");
        assert_eq!(v, Value::Str("zebra".into()));
    }
```

The existing `vars_pipes_like_any_other_table` and `vars_with_nothing_injected_is_an_empty_table` stay as they are — sorting and the empty-`[]` rule are unchanged by this task, and they still pin both.

- [ ] **Step 2: Run to verify they fail**

```bash
cargo test -p bterm-core --lib builtins::tests::vars
```

Expected: FAIL — `VisibleVar` not found.

- [ ] **Step 3: Add the types and change the trait**

In `crates/bterm-core/src/registry.rs`, above `trait HostHooks`:

```rust
/// Which layer a visible variable came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VarOrigin {
    /// Injected engine-wide by the host; visible in every session.
    Host,
    /// Injected into one session, shadowing any host value of that name.
    Session,
}

impl VarOrigin {
    /// The label the `vars` table shows.
    pub fn as_str(self) -> &'static str {
        match self {
            VarOrigin::Host => "host",
            VarOrigin::Session => "session",
        }
    }
}

/// One row of what `$name` would resolve to here.
#[derive(Clone, Debug, PartialEq)]
pub struct VisibleVar {
    pub name: String,
    pub origin: VarOrigin,
    pub value: Value,
}
```

and replace the trait method:

```rust
    /// Variables visible to a pipeline here, as `$name` would resolve them:
    /// the host's injected values with any session-local ones layered on
    /// top, each labelled with where it came from. A shadowed host value is
    /// absent rather than listed twice — a name resolves to one thing.
    /// Named for the question it answers; `Engine::host_vars` returns only
    /// the host layer.
    fn visible_vars(&self) -> Vec<VisibleVar> {
        Vec::new()
    }
```

- [ ] **Step 4: Implement it on `EngineHost`**

Replace `EngineHost::visible_vars` in `crates/bterm-core/src/engine.rs`:

```rust
    fn visible_vars(&self) -> Vec<crate::registry::VisibleVar> {
        use crate::registry::{VarOrigin, VisibleVar};
        self.access.with(|e| {
            let session_vars = e
                .mux
                .session_of_pane(self.pane)
                .and_then(|sid| e.mux.sessions.get(&sid))
                .map(|s| s.vars.clone())
                .unwrap_or_default();
            // A shadowed host entry is dropped rather than listed twice:
            // `vars` answers what a name resolves to, and a name resolves
            // to one value.
            let mut out: Vec<VisibleVar> = e
                .host_vars()
                .iter()
                .filter(|(name, _)| !session_vars.contains_key(*name))
                .map(|(name, value)| VisibleVar {
                    name: name.clone(),
                    origin: VarOrigin::Host,
                    value: value.clone(),
                })
                .collect();
            out.extend(session_vars.into_iter().map(|(name, value)| VisibleVar {
                name,
                origin: VarOrigin::Session,
                value,
            }));
            out
        })
    }
```

Note this no longer calls `scope_for_pane`, because it needs the layers apart. The merge rule is duplicated here in the sense that session wins — keep the `filter` and the ordering exactly as written so the two cannot disagree.

- [ ] **Step 5: Add the column**

Replace `vars` in `crates/bterm-core/src/builtins/mod.rs`:

```rust
fn vars(ctx: ExecContext, _call: BoundCall, _input: PipelineData) -> Result<PipelineData, ShellError> {
    let mut rows = ctx.host.visible_vars();
    // Sorted so the table is stable between runs: the underlying maps
    // iterate in an arbitrary order, which would make the output shuffle
    // and any test asserting on it flaky.
    rows.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(PipelineData::Value(Value::List(
        rows.into_iter()
            .map(|row| {
                Value::record([
                    ("name".to_string(), Value::Str(row.name)),
                    ("scope".to_string(), Value::Str(row.origin.as_str().to_string())),
                    ("value".to_string(), row.value),
                ])
            })
            .collect(),
    )))
}
```

- [ ] **Step 6: Run to verify they pass**

```bash
cargo test -p bterm-core --lib builtins::tests::vars
```

- [ ] **Step 7: Add an engine-level test for the shadow-once rule**

The `TestHost` tests above use a hand-built list, so they cannot catch a wrong `EngineHost` implementation. Add to `engine.rs`'s tests:

```rust
    #[test]
    fn vars_shows_a_shadowed_name_once_labelled_session() {
        let access = engine();
        let sid = access.with(|e| e.mux.active_session);
        access.with(|e| {
            e.set_host_var("shared", Value::Str("host".into())).expect("valid");
            e.set_host_var("only_host", Value::Int(1)).expect("valid");
            e.set_session_var(sid, "shared", Value::Str("session".into())).expect("valid");
        });
        // Two names, not three: the shadowed host entry is hidden, not
        // duplicated. `map` keeps every stage non-singleton.
        assert_eq!(run_line(&access, "vars | length").expect("resolves"), Value::Int(2));
        assert_eq!(
            run_line(&access, "vars | filter {|r| $r.name == 'shared'} | map {|r| $r.scope}")
                .expect("resolves"),
            Value::Str("session".into())
        );
        assert_eq!(
            run_line(&access, "vars | filter {|r| $r.name == 'shared'} | map {|r| $r.value}")
                .expect("resolves"),
            Value::Str("session".into())
        );
    }
```

- [ ] **Step 8: Teeth-check the shadow filter**

Remove the `.filter(…)` from `EngineHost::visible_vars` and confirm `vars_shows_a_shadowed_name_once_labelled_session` fails on the `length` assertion (3, not 2). Restore. Report both outputs verbatim.

- [ ] **Step 9: Full suite and clippy**

```bash
cargo test --workspace && cargo clippy --workspace --all-targets
```

Expected: 257 passed (255 + 2), clippy clean.

- [ ] **Step 10: Commit**

```bash
git add crates/bterm-core/src/registry.rs crates/bterm-core/src/engine.rs crates/bterm-core/src/builtins/mod.rs
git commit -m "Label where each visible variable came from

Without a scope column a session value shadowing a host one is a single
indistinguishable row, and 'why is \$game not what I set' has no answer
from inside the shell -- which is exactly where it gets asked.

A shadowed host entry is hidden rather than listed twice, because vars
answers what a name resolves to and a name resolves to one value. The
hidden value is still readable through its own layer, since reads name a
layer and never merge.

visible_vars changes shape, so the shipped test asserting the two-field
record is updated. That is a shape assertion rather than a behavioural
guarantee; sorting and the empty-table rule stay pinned by their own
tests."
```

---

## Task 3: `opts` at the wasm boundary

**Files:**
- Modify: `crates/bterm-wasm/src/lib.rs` (the five methods, ~line 390 onward)
- Test: `crates/bterm-wasm/tests/boundary.rs`

**Interfaces:**
- Consumes: Task 1's `Engine` session accessors.
- Produces: a private `enum VarTarget { Host, Session(u32) }` and `fn var_target(opts: &JsValue) -> Result<VarTarget, JsValue>`; the five methods each taking a trailing `opts: JsValue`.

**Watch the borrow rule.** `Reflect::get` on `opts` is a JS call — parse the target *before* opening any `WasmAccess.with`.

- [ ] **Step 1: Write the failing tests**

Add to `crates/bterm-wasm/tests/boundary.rs`, following the file's existing style (`make_core()`, async tests, `core.dispose()` at the end, `Reflect` already imported).

```rust
#[wasm_bindgen_test]
async fn omitting_opts_still_means_the_host_layer() {
    // Every call site written before session scope existed must keep
    // working, which is why host is the default rather than an explicit
    // choice the caller has to make.
    let core = make_core();
    core.set_variable("g", JsValue::from_str("host"), JsValue::UNDEFINED).expect("valid");
    assert_eq!(
        core.get_variable("g", JsValue::UNDEFINED).as_string().as_deref(),
        Some("host")
    );
    core.dispose();
}

#[wasm_bindgen_test]
async fn a_session_scoped_value_shadows_the_host_one_in_that_session() {
    let core = make_core();
    let snap = core.snapshot();
    let sessions = Reflect::get(&snap, &"sessions".into()).expect("sessions");
    let first = js_sys::Array::from(&sessions).get(0);
    let sid = Reflect::get(&first, &"id".into()).expect("id").as_f64().expect("number");

    let opts = js_sys::Object::new();
    Reflect::set(&opts, &"scope".into(), &"session".into()).expect("set");
    Reflect::set(&opts, &"session".into(), &JsValue::from_f64(sid)).expect("set");

    core.set_variable("x", JsValue::from_str("host"), JsValue::UNDEFINED).expect("valid");
    core.set_variable("x", JsValue::from_str("session"), opts.clone().into()).expect("valid");

    // Reads name a layer and never merge, so each returns its own.
    assert_eq!(
        core.get_variable("x", JsValue::UNDEFINED).as_string().as_deref(),
        Some("host")
    );
    assert_eq!(
        core.get_variable("x", opts.clone().into()).as_string().as_deref(),
        Some("session")
    );
    // What actually resolves is the session one.
    assert_eq!(run_value(&core, "echo $x").await.as_string().as_deref(), Some("session"));
    core.dispose();
}

#[wasm_bindgen_test]
async fn an_unknown_session_id_throws_rather_than_aborting() {
    let core = make_core();
    let opts = js_sys::Object::new();
    Reflect::set(&opts, &"scope".into(), &"session".into()).expect("set");
    Reflect::set(&opts, &"session".into(), &JsValue::from_f64(9999.0)).expect("set");

    assert!(core.set_variable("x", JsValue::from_str("v"), opts.clone().into()).is_err());
    assert!(core.get_variable("x", opts.clone().into()).is_undefined());
    core.dispose();
}

#[wasm_bindgen_test]
async fn a_malformed_scope_option_is_rejected() {
    let core = make_core();

    // `scope: 'session'` with no id has no target at all.
    let no_id = js_sys::Object::new();
    Reflect::set(&no_id, &"scope".into(), &"session".into()).expect("set");
    assert!(core.set_variable("x", JsValue::from_f64(1.0), no_id.into()).is_err());

    // An unrecognised scope is a typo, not a silent fallback to host —
    // falling back would put the value somewhere the caller did not ask for.
    let bogus = js_sys::Object::new();
    Reflect::set(&bogus, &"scope".into(), &"sesion".into()).expect("set");
    assert!(core.set_variable("x", JsValue::from_f64(1.0), bogus.into()).is_err());
    assert!(core.get_variable("x", JsValue::UNDEFINED).is_undefined(), "nothing stored");
    core.dispose();
}
```

Also update every existing call to the five methods in that file to pass `JsValue::UNDEFINED` as the new trailing argument.

- [ ] **Step 2: Run to verify they fail**

```bash
just test-wasm
```

Expected: FAIL — arity mismatch on the five methods.

- [ ] **Step 3: Implement the parser**

In `crates/bterm-wasm/src/lib.rs`, above `impl BtermCore`:

```rust
/// Which layer a variable call targets.
enum VarTarget {
    Host,
    Session(u32),
}

/// Read the optional `{ scope, session }` argument.
///
/// Absent means host, so every call written before session scope existed
/// keeps working. An unrecognised `scope` is an error rather than a
/// fallback to host: a typo that silently wrote to the wrong layer would
/// be found later, by someone reading a value that was never set where
/// they looked.
///
/// Pure JS work — call it before opening an engine borrow.
fn var_target(opts: &JsValue) -> Result<VarTarget, JsValue> {
    if opts.is_undefined() || opts.is_null() {
        return Ok(VarTarget::Host);
    }
    let scope = js_sys::Reflect::get(opts, &JsValue::from_str("scope"))
        .ok()
        .and_then(|v| v.as_string());
    match scope.as_deref() {
        None | Some("host") => Ok(VarTarget::Host),
        Some("session") => {
            let id = js_sys::Reflect::get(opts, &JsValue::from_str("session"))
                .ok()
                .and_then(|v| v.as_f64())
                .ok_or_else(|| {
                    JsValue::from_str("{ scope: 'session' } needs a `session` id from snapshot.sessions")
                })?;
            Ok(VarTarget::Session(id as u32))
        }
        Some(other) => Err(JsValue::from_str(&format!(
            "unknown scope `{other}`: expected 'host' or 'session'"
        ))),
    }
}
```

- [ ] **Step 4: Thread it through the five methods**

Each gains a trailing `opts: JsValue`, parses it first, then branches. `set_variable` becomes:

```rust
    pub fn set_variable(&self, name: &str, value: JsValue, opts: JsValue) -> Result<(), JsValue> {
        if !engine_alive() {
            return Err(JsValue::from_str("browser-terminal: engine is disposed"));
        }
        // Both conversions are JS work and happen before the borrow opens.
        let target = var_target(&opts)?;
        let converted = convert::js_to_value(&value).map_err(|e| JsValue::from_str(&e))?;
        WasmAccess
            .with(|e| match target {
                VarTarget::Host => e.set_host_var(name, converted),
                VarTarget::Session(id) => e.set_session_var(id, name, converted),
            })
            .map_err(|err| JsValue::from_str(&err.msg))
    }
```

Apply the same shape to the other four:

- `set_variables(values, opts)` — parse the target once, keep the existing validate-everything-then-apply structure, and dispatch per entry inside the single borrow.
- `unset_variable(name, opts) -> bool` — `VarTarget::Session(id)` uses `unset_session_var(id, name).unwrap_or(false)`, so an unknown id reports `false` rather than throwing; note this in a comment, since it is the one place a bad id is not an error and the reason is that the signature has nowhere to put one.
- `get_variable(name, opts) -> JsValue` — `undefined` for unset **and** for an unknown session id.
- `variables(opts) -> JsValue` — an unknown id returns `undefined`; a live id returns that session's own layer, sorted, same as the host path.

Keep every `Reflect::set` and `value_to_js` outside the borrow, as the existing code does.

- [ ] **Step 5: Run to verify they pass**

```bash
just test-wasm
```

Expected: 32 passed (28 + 4).

- [ ] **Step 6: Teeth-check the unknown-scope rejection**

Change the `Some(other)` arm to `Ok(VarTarget::Host)` — the silent fallback the comment argues against — and confirm `a_malformed_scope_option_is_rejected` fails. Restore. Report both outputs verbatim.

- [ ] **Step 7: Clippy for both targets**

```bash
cargo clippy --workspace --all-targets && cargo clippy -p bterm-wasm --target wasm32-unknown-unknown
```

- [ ] **Step 8: Commit**

```bash
git add crates/bterm-wasm/src/lib.rs crates/bterm-wasm/tests/boundary.rs
git commit -m "Select a variable layer at the wasm boundary

An optional trailing { scope, session } on all five methods. Absent means
host, so every call written before session scope existed keeps working.

An unrecognised scope is an error rather than a fallback to host: a typo
that silently wrote to the wrong layer would surface much later, as
someone reading a value that was never set where they looked. The target
is parsed before the engine borrow opens, because Reflect::get is a JS
call and no JS call may happen inside one."
```

---

## Task 4: `opts` in TypeScript, and the README

**Files:**
- Modify: `packages/browser-terminal/src/index.ts`
- Modify: `packages/browser-terminal/src/types.ts` (export `VarScope`)
- Modify: `packages/browser-terminal/README.md`

**Interfaces:**
- Consumes: Task 3's five wasm methods.
- Produces: `VarScope` exported from the package; `opts?: VarScope` on the five `BrowserTerminal` methods.

- [ ] **Step 1: Add the type**

In `packages/browser-terminal/src/types.ts`:

```ts
/**
 * Which layer a variable call targets.
 *
 * Omitted means the host layer — engine-wide, visible in every session
 * including ones created later. `{ scope: 'session', session }` targets one
 * session's own layer, which shadows the host one for that session only.
 *
 * The id comes from `bt.snapshot.sessions`, never from "whichever session
 * is active": an ambient target would land the value somewhere else if the
 * user switched sessions between your read and your write.
 */
export type VarScope = { scope?: 'host' } | { scope: 'session'; session: number };
```

and re-export it from `index.ts`'s `export type { … } from './types.js'` block.

- [ ] **Step 2: Thread it through**

Each of the five gains `opts?: VarScope` and forwards it. For example:

```ts
  setVariable(name: string, value: Value, opts?: VarScope): void {
    this.assertLive();
    this.core.set_variable(name, value, opts);
  }
```

`getVariable` and `variables` forward identically. Keep the existing return-side casts; the wasm params are `any`, so no argument casts are needed.

Import `VarScope` into `index.ts`'s `import type { … } from './types.js'` list — note the previous increment hit exactly this: `Value` appeared only in the re-export block, which creates no local binding, and the build failed until it was added to the import.

- [ ] **Step 3: Build and type-check**

```bash
just build && just typecheck
```

- [ ] **Step 4: Document it**

In `packages/browser-terminal/README.md`, at the end of the "Host state as shell variables" section:

````markdown
### Scoping a variable to one session

By default a variable is engine-wide. To scope one to a single session,
pass an id from `bt.snapshot.sessions`:

```ts
const [first] = bt.snapshot!.sessions;
bt.setVariable('scratch', 'local', { scope: 'session', session: first.id });
```

A session value shadows a host value of the same name, in that session
only. Reads name a layer rather than merging, so both remain readable:

```ts
bt.getVariable('scratch');                                        // the host value
bt.getVariable('scratch', { scope: 'session', session: first.id }); // that session's
```

For "what would `$scratch` actually be here?", the shell's `vars` shows
the resolved view with a `scope` column saying where each value came from:
`vars | filter {|v| $v.scope == 'session'}`.

Sessions are addressed by explicit id rather than "whichever is active",
because a user can switch sessions between your read and your write. An
unknown id throws.
````

Also add `VarScope` to the exported-types list if the README enumerates them.

- [ ] **Step 5: Verify the README against the code**

Check each claim rather than trusting the prose: that `snapshot.sessions` really carries `id`, that an unknown id really throws from `setVariable` (Task 3 makes it throw there but *not* from `unsetVariable`, which returns `false` — do not claim otherwise), and that the `vars` filter example really works. Report anything that does not hold.

- [ ] **Step 6: Commit**

```bash
git add packages/browser-terminal/src/index.ts packages/browser-terminal/src/types.ts packages/browser-terminal/README.md
git commit -m "Expose variable scoping in TypeScript

VarScope makes the layer explicit at the call site, and omitting it keeps
every existing call meaning what it meant. The README says plainly that
the session id comes from a snapshot rather than being implied, and that
reads return one layer while the shell's vars is what resolves."
```

---

## Task 5: Demo and browser proof

**Files:**
- Modify: `packages/demo/src/main.ts`, `packages/demo/index.html`, `packages/demo/tests/smoke.spec.ts`

**Interfaces:**
- Consumes: Task 4's TypeScript surface; the `vars` `scope` column from Task 2.

- [ ] **Step 1: Extend the demo**

In the existing `// #region variables` block in `packages/demo/src/main.ts`, after the `setVariables` call:

```ts
  // A session-scoped value: same name, different answer depending on which
  // session you are in. `session new` in the terminal gives you a second
  // one where `$scope_demo` reads "host" instead.
  const firstSession = bt.snapshot?.sessions[0];
  if (firstSession) {
    bt.setVariable('scope_demo', 'session', {
      scope: 'session',
      session: firstSession.id,
    });
  }
  bt.setVariable('scope_demo', 'host');
```

- [ ] **Step 2: Show it in the Try block**

In `packages/demo/index.html`, add to the Try `<pre>`:

```
vars               # name, scope, value — scope shows shadowing
echo $scope_demo   # 'session' here; 'host' after `session new`
```

- [ ] **Step 3: Write the browser test**

This is the test that would catch an implicit-active-session implementation regressing in, so it must involve two real sessions.

```ts
test('a session-scoped variable resolves per session', async ({ page }) => {
  await page.goto('/');
  await waitForTerminal(page);

  const result = await page.evaluate(async () => {
    const bt = window.bt;
    bt.setVariable('which', 'host');

    const firstId = bt.snapshot!.sessions[0].id;
    bt.setVariable('which', 'first-session', { scope: 'session', session: firstId });
    const inFirst = (await bt.run('echo $which')).value;

    // A second session has no value of its own, so it falls through to host.
    await bt.run('session new');
    const secondId = bt.snapshot!.sessions.find((s) => s.id !== firstId)!.id;
    const inSecond = (await bt.run('echo $which')).value;

    bt.setVariable('which', 'second-session', { scope: 'session', session: secondId });
    const inSecondAfter = (await bt.run('echo $which')).value;

    return {
      inFirst,
      inSecond,
      inSecondAfter,
      hostStillReadable: bt.getVariable('which'),
      firstStillItsOwn: bt.getVariable('which', { scope: 'session', session: firstId }),
    };
  });

  expect(result.inFirst).toBe('first-session');
  // The proof that scoping is real: the same name, the same instant, a
  // different answer because a different session is active.
  expect(result.inSecond).toBe('host');
  expect(result.inSecondAfter).toBe('second-session');
  // Shadowing hides, it does not overwrite.
  expect(result.hostStillReadable).toBe('host');
  expect(result.firstStillItsOwn).toBe('first-session');
});

test('vars labels which layer each value came from', async ({ page }) => {
  await page.goto('/');
  await waitForTerminal(page);

  const rows = await page.evaluate(async () => {
    const bt = window.bt;
    bt.setVariable('only_host', 1);
    bt.setVariable('shadowed', 'from-host');
    const sid = bt.snapshot!.sessions[0].id;
    bt.setVariable('shadowed', 'from-session', { scope: 'session', session: sid });

    // `||`, not `or` — the grammar has the C-style operators (`lex.rs`
    // Op::OrOr); `or` is a parse error. Verified against the CLI.
    return (await bt.run("vars | filter {|r| $r.name == 'only_host' || $r.name == 'shadowed'}"))
      .value as Array<{ name: string; scope: string; value: unknown }>;
  });

  const byName = Object.fromEntries(rows.map((r) => [r.name, r]));
  expect(byName.only_host.scope).toBe('host');
  // One row, labelled session — not two rows for the shadowed name.
  expect(byName.shadowed.scope).toBe('session');
  expect(byName.shadowed.value).toBe('from-session');
  expect(rows.filter((r) => r.name === 'shadowed')).toHaveLength(1);
});
```

The `||` above is deliberate and verified — the grammar has `Op::OrOr` and `Op::AndAnd`, and the word form `or` is a parse error (`expected `}` to close the closure, found `or``).

- [ ] **Step 4: Teeth-check the per-session test**

Make `set_variable`'s `VarTarget::Session(_)` arm write to the host layer instead — the "scoping does nothing" implementation. Confirm `a session-scoped variable resolves per session` fails on `inSecond`, which would then read `first-session` rather than `host`. Restore. Report both outputs verbatim.

- [ ] **Step 5: Full verification**

```bash
cargo test --workspace
just test-wasm
just build && just typecheck
npm --prefix packages/demo run build
cd packages/demo && npx playwright test
```

Expected: 257 native, 32 wasm, 21 Playwright (19 + 2).

- [ ] **Step 6: Commit**

```bash
git add packages/demo/src/main.ts packages/demo/index.html packages/demo/tests/smoke.spec.ts
git commit -m "Demo session-scoped variables, and prove they are per-session

The browser test creates a real second session and asserts the same name
gives a different answer in each -- the check that would fail if scoping
silently wrote everything to the host layer, which is the shape a wrong
implementation takes.

A second test pins the vars scope column, including that a shadowed name
appears once rather than twice."
```

---

## Self-review notes

**Spec coverage.** Addressing by explicit id with an erroring unknown id (Task 1); reads returning one layer (Tasks 1, 3); the `opts` argument defaulting to host (Tasks 3, 4); `vars` gaining `scope` with shadow-once semantics (Task 2); killing a session dropping its variables (inherent — `Session` owns the map, noted in Task 1's commit); native, boundary and browser tests throughout.

**Deliberately not included.** Shell-level assignment (`$x = 1`) stays deferred, as in increment 1. A `resolved: true` read form was considered and rejected in the spec.

**Risks worth naming.**
- Task 2 changes a public trait method's signature. In-tree implementors are `EngineHost`, test doubles, and the CLI host (which inherits the default and needs no change). If the compiler finds another, that is a finding, not a nuisance.
- `EngineHost::visible_vars` no longer calls `scope_for_pane`, so the session-wins rule now exists in two places. The plan pins both with tests (`a_session_variable_shadows_the_host_one_without_destroying_it` for the merge, `vars_shows_a_shadowed_name_once_labelled_session` for the label), but a future change to one must update the other.
- `unset_variable` returns `false` for an unknown session id rather than throwing, because `-> bool` has nowhere to put an error. That is an inconsistency with the other four and is called out in Task 3's comment and Task 4's README check. If it bothers you later, the fix is `-> Result<bool, JsValue>`, which is a breaking change.
- Task 5's `filter` closure uses `||`. This was checked against the CLI while writing the plan rather than assumed: `or` is a parse error, `||` is `Op::OrOr` and works.
