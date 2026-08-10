# Host-injected shell variables — design

**Status:** approved, not yet implemented
**Issue:** [#8 — Enhancement: inject host application values as shell `$variables`](https://github.com/benjamin-small/browser-terminal/issues/8)
**Date:** 2026-08-10

## Problem

The shell already evaluates `$name` — `Expr::Var`, `InterpPart::Var`, and
`Scope` all exist, and every mux session owns a `vars: Scope`. But sessions
start empty (`mux/mod.rs:156`) and no public API populates them, so a host
page can register commands and still cannot pass its own state to them:

```sh
rtce evaluate --game $game --build $build
# error: unknown variable `$game`
```

Quoting `'$game'` passes the literal seven characters, not the document.

The motivating consumer is
[rpg-theorycraft-engine](https://github.com/benjamin-small/rpg-theorycraft-engine),
whose browser tutorial has live JSON editors and no filesystem. Its native
CLI takes paths; in the browser the same command shape should take variables
holding the editor documents, so a lesson can swap `$scenario` between runs
without rewriting the command text or inventing fake filenames.

## What this is not

Shell-level assignment (`$x = 1` typed at the prompt) stays deferred. This
adds *host* injection only. The design leaves room for assignment without
revisiting the merge — see Precedence.

## Decisions

| Question | Decision |
| --- | --- |
| Scope model | One engine-wide host scope. No `{ scope: 'session' }` option in this cut. |
| Precedence | Session values override host values. |
| Freshness | Snapshot per command line. |
| Read API | `getVariable` and `variables()` in addition to the setters. |
| Shell visibility | A `vars` builtin, listing `name` and `value`. |
| Storage | A field on `Engine`, beside `matcher` and `fn_compiler`. |

### Why engine-wide rather than per-session

Splitting a pane or running `session new` must not make the host's `$game`
disappear — the host injected it to describe application state, which has
nothing to do with which pane you are looking at. A `{ scope: 'session' }`
option can be added later without a breaking change precisely because the
default is global; adding it now would mean designing collision rules for a
feature (shell assignment) that does not exist.

### Why snapshot rather than live

`scope_for_pane` already clones, so a `setVariable` during a running pipeline
is invisible to it and takes effect on the next line. That is the behaviour we
want, not merely the one we inherited: under live lookup a single pipeline
could bind `$game` to two different values in two stages, and "what did this
command actually run against?" would have no answer after the fact. A long
`rtce simulate` finishing against the document it started with is the
defensible outcome.

This is a documented guarantee with a test, not an implementation detail.

## Architecture

### The scope, and the one place it is built

`Engine` gains one field, joining the host-supplied state already there:

```rust
pub struct Engine {
    pub registry: CommandRegistry,
    pub mux: Mux,
    matcher: Rc<dyn PatternMatcher>,      // supplied by the host
    fn_compiler: Rc<dyn FnCompiler>,      // supplied by the host
    host_vars: Scope,                     // supplied by the host  <-- new
    // …
}
```

Accessed only through `set_host_var`, `unset_host_var`, `host_var`, and
`host_vars` — the field itself stays private.

`scope_for_pane` (`engine.rs:600`) is the single site where a `Scope` is ever
constructed, and both execution paths already go through it: `execute_line`
at `:773` for interactive panes and `eval_to_value` at `:826` for `run()`.
Merging there therefore covers panes, `run()`, positional arguments, flag
values, and `"…$name…"` interpolation in one change — interpolation reads the
same map (`signature.rs:271-289`).

```rust
fn scope_for_pane<A: EngineAccess>(access: &A, pane: u32) -> Scope {
    access.with(|e| {
        let mut scope = e.host_vars.clone();
        if let Some(s) = e.mux.session_of_pane(pane).and_then(|id| e.mux.sessions.get(&id)) {
            scope.extend(s.vars.clone());
        }
        scope
    })
}
```

Host underneath, session on top. The session map is always empty today so the
`extend` is a no-op — but writing the precedence now means shell assignment
later shadows a host value without anyone rereading this function.

The merge happens inside the borrow `scope_for_pane` already takes. No second
borrow, no new opportunity to violate the engine invariant.

### Name validation

`lex.rs` gains:

```rust
pub fn is_valid_var_name(name: &str) -> bool {
    !name.is_empty() && name.chars().all(is_var_char)
}
```

reusing the existing private `is_var_char` (`c.is_alphanumeric() || c == '_'`).
Every setter calls it.

This must not be reimplemented at the TypeScript boundary. Rust's
`is_alphanumeric` is Unicode-aware, so the lexer accepts names a naive
`/^[A-Za-z_]\w*$/` rejects. Verified against the CLI:

| input | lexer |
| --- | --- |
| `$café` | accepted (then "unknown variable", i.e. the *name* parsed) |
| `$1` | accepted — digits are legal, including leading |
| `$` | parse error, "expected a variable name after `$`" |

A stricter guard at the boundary would let a host set a name it cannot
reference. One rule, one definition.

### Public API

Five methods on `BtermCore` (all `&self`, per the no-`&mut`-exports
invariant), mirrored on `BrowserTerminal`:

```ts
setVariable(name: string, value: Value): void;
setVariables(values: Record<string, Value>): void;
unsetVariable(name: string): boolean;      // was it set?
getVariable(name: string): Value | undefined;
variables(): Record<string, Value>;
```

Values cross via the existing `js_to_value` / `value_to_js` — the same path
command arguments take. Nothing is re-lexed as shell source, so a value
containing `; rm -rf /` is a string containing those characters and cannot
become syntax. That property comes from the value never re-entering the
parser, not from escaping.

**`getVariable` returns `undefined` for unset, never `null`.** `Value`
includes `null`, and the issue's own example injects `simdef ?? null`. If
absent and null-valued both returned `null`, a host could not tell them apart
and `unsetVariable` would be unobservable through the getter. `undefined` is
outside `Value`, so it is unambiguous.

**`setVariables` validates every name before applying any.** A partial apply
is the worst outcome: half the editors land, one stays stale, and the next
command runs against a mix that looks plausible. One bad name means nothing
changed.

`unsetVariable` returns whether the name was set, so a host can tell a real
removal from a no-op without a second call. Unsetting an absent name is not
an error.

**`variables()` returns the host scope, not the merged one.** It is a
read-back of what the host itself injected — the counterpart to
`setVariables`, so `setVariables(x)` followed by `variables()` round-trips.
The shell's `vars` builtin is the one that shows the *merged* view, because
its job is to answer "what would `$name` resolve to here?". The two are
identical today, since session scopes are always empty; they diverge the
moment shell assignment lands, and each is right for its own audience.

All five follow the existing `assertLive()` pattern and throw after
`dispose()`. The variables need no teardown of their own: they live in
`Engine`, and `dispose_engine()` drops it. (Contrast `js_fn`, which lives in
its own `thread_local` and must be cleared explicitly — a second teardown
obligation this design deliberately avoids.)

### The `vars` builtin

Reached through `HostHooks`, not `ExecContext`. `ExecContext` carries `host`,
`sink`, `width`, `pane`, and `run_id`; adding a scope there would hand every
command the variables whether it needs them or not. `HostHooks` is already
the seam for host services a command may touch (`history()`,
`help_overview()`, `mux_action()`), and `EngineHost` already holds `pane`
(`engine.rs:440-450`), so:

```rust
fn visible_vars(&self) -> Vec<(String, Value)> { Vec::new() }   // default
```

Named `visible_vars` rather than `host_vars` deliberately: `Engine::host_vars`
is the accessor for the host layer alone, and two methods a keystroke apart
returning different things is exactly the sort of near-collision that gets
mis-wired. This one answers "what is visible from here", which is the merged
question.

The engine's implementation returns the *merged* scope for its pane, so
`vars` shows what `$name` would actually resolve to rather than only the host
layer. The native CLI inherits the empty default and prints an empty table,
which is correct rather than special-cased.

Output is a `List<Record>` of `{ name, value }`, sorted by name —
deterministic for tests, predictable for a human. An empty scope yields `[]`,
so `vars | length` answers 0 rather than erroring, consistent with the
empty-stream rule in `stream::collect`.

Full values are emitted; the table renderer truncates wide cells to the
column width with `…`. Display truncates, the pipe preserves — so
`vars | grep game | get value` returns the whole document. This is how every
other table in the shell already behaves.

## Error handling

| Situation | Behaviour |
| --- | --- |
| `setVariable('a b', …)` | Throws synchronously, naming the offending identifier. |
| `setVariables` with one bad name | Throws; no value applied. |
| `$missing` in a command | Existing positioned `unknown variable` diagnostic, unchanged. |
| `unsetVariable('never-set')` | Returns `false`. Not an error. |
| Any method after `dispose()` | Throws, per `assertLive()`. |
| `vars` with nothing injected | Empty table. |

Unsetting restores the `unknown variable` diagnostic exactly as before — the
error path is untouched by this work.

## Testing

**Native** (`cargo test`) — the merge and validation:

- host-only resolution; session overriding host on a name collision
- replacement and removal; `unset` of an absent name returns `false`
- empty scope resolves nothing and leaves the diagnostic intact
- `is_valid_var_name` accepts `café` and `1`, rejects `""`, `"a b"`, `"a-b"`
- snapshot: the scope handed to a pipeline does not change under it

**Boundary** (`just test-wasm`) — what native cannot reach:

- a record, a list, a `null`, and an int survive the round trip with types
- an invalid name throws across the boundary
- `getVariable` distinguishes unset (`undefined`) from set-to-null

**Browser** (Playwright) — the layer that settles it:

- a registered command invoked as `probe --game $game` receives the value as
  a real argument
- `run()` sees the same variables as an interactive pane
- `"prefix-$game"` interpolates
- `vars | grep …` pipes
- **the snapshot guarantee**: set a variable, start a slow command, change the
  variable mid-flight, assert the command finishes with the original value

That last test must be teeth-checked by making the lookup live and confirming
it fails. A test that passes under both behaviours documents nothing, which
is the failure mode this project has hit repeatedly — most recently a
block-mode test whose writes contained no delimiter, and before that three
tests that asserted a singleton-collapse bug as intended behaviour.

## Documentation

- `packages/browser-terminal/README.md`: a host-state injection example in
  the API section, and the snapshot rule stated in prose.
- The vanilla demo injects a variable and uses it, so the feature is visible
  rather than only described.
- `vars` gets help text like every other builtin.

## Acceptance criteria

- [ ] Set, replace, and unset a shell variable from TypeScript.
- [ ] Exported `Value` shapes preserved across wasm in both directions.
- [ ] Injected variables work as positional and flag arguments to registered
      commands, and inside string interpolation.
- [ ] They work through `run()` as well as interactive panes.
- [ ] Visible in every session and pane, including ones created afterwards.
- [ ] Unknown and unset variables keep the current positioned diagnostic.
- [ ] Snapshot-per-line documented and tested, teeth-checked.
- [ ] `vars` lists merged variables sorted by name; empty scope gives `[]`.
- [ ] README and demo carry a host-state example.
- [ ] Native, boundary, and browser tests as above.

## Files

| Path | Change |
| --- | --- |
| `crates/bterm-core/src/engine.rs` | `host_vars` field, accessors, merge in `scope_for_pane`, `EngineHost::visible_vars` |
| `crates/bterm-core/src/lex.rs` | `pub fn is_valid_var_name` |
| `crates/bterm-core/src/registry.rs` | `HostHooks::visible_vars` with an empty default |
| `crates/bterm-core/src/builtins/mod.rs` | the `vars` builtin |
| `crates/bterm-wasm/src/lib.rs` | five `BtermCore` methods |
| `packages/browser-terminal/src/index.ts` | five `BrowserTerminal` methods |
| `packages/browser-terminal/README.md` | host-state example |
| `packages/demo/src/main.ts` | demo injection |
| `packages/demo/tests/smoke.spec.ts` | browser tests |
