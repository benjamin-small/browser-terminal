# browser-terminal — v1 Architecture Design

> Status: approved 2026-07-16. Produced via multi-agent design workflow (4 research
> agents → 3 competing proposals → 3 judges → synthesis), then reviewed and approved.

**Vision.** A Rust library compiled to WebAssembly that gives any web page a
terminal/tmux experience — floating terminal panels docked at the bottom of the host
page, running a shell-like meta-language with structured-value pipes
(nushell/PowerShell style). Commands can be built into the Rust core or registered
from TypeScript in ~5 lines, so panels can do programmatic things with the host page.
"Forking shells" = creating sessions/panes tmux-style.

**Locked decisions.**
1. **Rendering**: xterm.js per pane; the Rust/WASM core owns sessions, the language,
   the command registry, pipes, and multiplexer state.
2. **Pipes**: structured values (typed `Value`s; tables auto-render at pipeline end).
3. **Scope**: tmux features in v1 (sessions, windows, splits, prefix keys, session
   switching, floating panels) plus a demo page.
4. **Concurrency**: single WASM instance on the main thread; async sessions via
   `wasm-bindgen-futures`; web workers deferred.

---

## 1. Workspace layout & build

```
browser-terminal/
├── Cargo.toml                    # [workspace] members = crates/*
├── justfile                      # just wasm / test / demo / pack
├── crates/
│   ├── bterm-core/               # ZERO wasm deps; tested natively with cargo test
│   │   └── src/
│   │       ├── value.rs          # Value enum, coercion
│   │       ├── lex.rs, parse.rs, ast.rs      # handwritten lexer + recursive descent, Span
│   │       ├── signature.rs      # Signature, Shape, binding, help generation
│   │       ├── registry.rs       # Command trait + CommandRegistry
│   │       ├── eval.rs           # pipeline evaluation
│   │       ├── editor.rs         # line editor: feed(bytes) -> Effects
│   │       ├── render/           # Value -> ANSI (tables etc.)
│   │       ├── mux/              # sessions/windows/panes, layout tree, keymap
│   │       ├── protocol.rs       # HostMsg / EngineEvent (serde + tsify types)
│   │       ├── error.rs          # ShellError + caret renderer
│   │       └── builtins/
│   ├── bterm-cli/                # native REPL binary over bterm-core (dev harness)
│   └── bterm-wasm/               # ONLY crate touching wasm-bindgen/js-sys
│       └── src/{lib.rs, js_command.rs, convert.rs, events.rs}
├── packages/
│   ├── browser-terminal/         # the npm package (handwritten package.json)
│   │   ├── src/{index.ts, panes.ts, panels.ts, keys.ts, events.ts}
│   │   └── dist/                 # tsc output + bterm_wasm.js + _bg.wasm + .d.ts
│   └── demo/                     # Vite app, workspace-linked
```

**Three Rust crates, not five.** `bterm-core` uses module boundaries internally
(lang/engine/mux); the compile-time boundaries that pay rent are core-vs-wasm (native
testing) and the CLI. `bterm-cli` is a ~100-line rustyline REPL over the core — the
single best dev-loop accelerator.

**Build: no wasm-pack.** `just wasm` runs
`cargo build -p bterm-wasm --release --target wasm32-unknown-unknown` →
`wasm-bindgen --target web --out-dir packages/browser-terminal/dist/wasm` →
`wasm-opt -Oz`. Release profile: `opt-level="z"`, `lto=true`, `codegen-units=1`,
`panic="abort"`. Core deps: `serde`, `indexmap`, `unicode-width` only — no logos
(handwritten lexer, §3), no miette. Wasm deps: `wasm-bindgen(-futures)`, `js-sys`,
`serde-wasm-bindgen`, `tsify`. Size budget: **≤350KB pre-gzip**, audited with
`twiggy` at milestone 7.

**Consumption story.** Default load path is
`init(new URL('./wasm/bterm_wasm_bg.wasm', import.meta.url))`, which Vite and
webpack 5 handle. Additionally, `BrowserTerminal.create({ wasmUrl?: string | URL })`
is a first-class escape hatch, and the README documents two non-bundler paths: plain
`<script type="module">` importing from a CDN (esm.sh/unpkg) with an explicit
`wasmUrl`, and a copy-the-two-files recipe. Jest/SSR consumers are told: this library
requires a browser; guard imports.

## 2. Core domain model

```rust
// value.rs
pub enum Value {
    Null, Bool(bool), Int(i64), Float(f64), Str(String),
    List(Vec<Value>),
    Record(IndexMap<String, Value>),   // ordered; tables are List<Record>
}

pub enum PipelineData { Empty, Value(Value) }   // Stream variant is a documented v2 seam
```

**Pipes are collected in v1**: every command is already async because TS commands
force it, and lazy streams don't compose with async stages without pinning machinery
nobody consumes in v1. The `PipelineData` enum exists so adding `Stream(...)` later
is an additive variant, not a trait redesign. Progressive output for long-running
commands comes from `ctx.emit_line` instead (§4). No spans on values (spans live on
AST and errors); no `Value::Error` flowing through pipes (errors abort the pipeline,
§8).

**Boundary semantics.** `Value` crosses to JS via
`serde_wasm_bindgen::Serializer::json_compatible()` — records become plain JS objects
(never `Map`), and `Int` becomes a JS number. Documented invariant: integer magnitude
beyond 2^53 loses precision in JS; the lexer rejects integer literals beyond ±2^53 so
shell-authored values can't hit it silently.

```rust
// mux/
pub struct Mux { sessions: IndexMap<SessionId, Session>, active: SessionId, next_id: u32 }
pub struct Session { id, name, windows: IndexMap<WindowId, Window>, active_window, vars: HashMap<String, Value> }
pub struct Window  { id, name, layout: LayoutNode, active_pane: PaneId }
pub enum LayoutNode { Leaf(PaneId),
                      Split { dir: Dir, children: Vec<(f32 /*fraction*/, LayoutNode)> } }
pub struct Pane { id: PaneId, editor: LineEditor, history: Vec<String>,
                  running: Option<AbortHandle>, cols: u16, rows: u16 }
```

IDs are `u32` newtypes from one monotonic counter, stable for the page lifetime.
Windows are per-session (no tmux shared-window pool). "Forking a shell" =
`session new [name]`: one window, one pane; every pane is a shell with its own
editor/history, sharing the session's variable scope and the single global
`CommandRegistry`.

```rust
// registry.rs
pub trait Command {
    fn signature(&self) -> &Signature;
    fn run(&self, ctx: ExecContext, call: BoundCall, input: PipelineData)
        -> LocalBoxFuture<'static, Result<PipelineData, ShellError>>;
}
pub struct CommandRegistry { map: HashMap<String, Rc<dyn Command>> }  // key: "str upcase"
```

`LocalBoxFuture` is a hand-rolled `Pin<Box<dyn Future>>` alias (no futures crate).
Multi-word names are the subcommand mechanism: lookup tries longest bareword prefix.
**Collision policy:** `register` on a name owned by a Rust builtin → error; on a name
owned by a previous TS registration → replace with a `console.warn` (this is
deliberately the HMR/hot-reload behavior); `unregister(name)` is exposed on the TS
wrapper and only removes TS-registered commands.

## 3. Shell language

Grammar: `;`-separated pipelines of calls; call = multi-word bareword head + args;
args = `--long[=v]`, `--long v`, `-s` flags, or expressions; expressions = `'raw'`,
`"interpolated $var"`, numbers, bools, barewords, `$var`; `#` comments.
`( ) { } && || >` lex as reserved tokens producing spanned "not yet supported"
errors.

**Lexer: handwritten (~200 lines), no logos** — the token set is small and shell
lexing is context-quirky (`-v2.1` bareword vs flag vs negative number). Parser:
recursive descent (~400 lines); every token and AST node carries
`Span { start: u32, end: u32 }`; recovery syncs on `|`/`;` for multiple diagnostics
per line. The AST is expression-based (`Expr` includes a pipeline variant) so
cell-paths and subexpressions slot in post-v1.

**`where` semantics (the flagship demo must parse).** v1 adds **no comparison
operators to the grammar**. `where` is an ordinary command with three positionals:
`where <column: str> <op: str> <value: any>`, where op is one of the barewords
`eq ne gt lt ge le contains starts-with ends-with`. The demo is therefore:

```
links | where text ne '' | first 5
```

This is parseable by the v1 grammar as written, self-documents via `help where`, and
leaves `!=`-style operators as a pure-sugar v2 addition (the reserved-token list
already fences the characters).

**Signatures** (nushell-shaped):
`Signature { name, summary, required, optional, rest, flags }`,
`Shape::{Any, Str, Int, Float, Bool}`. `bind(sig, &Call)` walks args left-to-right,
matches flags, coerces barewords against declared shapes with span-pointing errors.
The evaluator intercepts `--help` **before** `run`, and `help`/binding errors get
Levenshtein "did you mean `--depth`?" (~30 lines). `Signature` derives `Tsify` so TS
command authors supply the identical structure with compile-time types.

## 4. Execution model

**One structural rule replaces all actor machinery.** Engine state lives in
`thread_local! { static ENGINE: RefCell<Engine> }` where
`Engine { mux: Mux, registry: CommandRegistry, event_queue: VecDeque<EngineEvent> }`.
The **only** access path is:

```rust
fn with_engine<R>(f: impl FnOnce(&mut Engine) -> R) -> R
```

A synchronous closure: no borrow can cross an `.await` because no borrow ever
escapes, and no `&mut self` wasm-bindgen exports exist, so the reentrancy panic path
is unreachable by construction.

**Event reentrancy.** Code inside `with_engine` never invokes JS; it pushes
`EngineEvent`s onto `event_queue`. Every export and every async task calls
`flush_events()` *after* its `with_engine` scopes close: it drains the queue and
invokes the JS callback with no borrow held. A JS event handler that synchronously
calls back into the engine (e.g. `resize()` from inside a layout handler) therefore
cannot double-borrow. This is a documented invariant with a debug assertion.

**Sync input hot path**: `feed(pane_id, bytes)` is a plain sync export. Inside one
`with_engine` scope, the pane's `LineEditor` consumes the bytes and returns
`Effects { echo: String, submitted: Option<String> }`; echo is written to xterm **in
the same tick**, no async hop. Only a submitted line enters the async world.

**Per-submission tasks.** Each submitted line runs as its own `spawn_local` future,
so a slow `fetch` in pane A never blocks pane B. The task captures its `pane_id` at
submit time (output can never mis-route), evaluates the pipeline stage by stage:

```rust
let mut data = PipelineData::Empty;
for call in &pipeline.calls {
    let (cmd, bound) = with_engine(|e| { let c = e.registry.lookup(&call.head)?;
                                         Ok((c.clone(), bind(c.signature(), call)?)) })?;
    data = cmd.run(ctx.clone(), bound, data).await?;   // nothing borrowed across await
}
with_engine(|e| e.emit_output(pane_id, render(&data.into_value(), cols)));
flush_events();
```

**Cancellation**: the task future is wrapped in an `Abortable`; the `AbortHandle` is
stored in `Pane.running`. Ctrl-C while a pipeline runs aborts the future, prints
`^C`, clears `running`, and reprints the prompt — a hung TS command can never
permanently wedge a pane.

**TS command invocation** (`bterm-wasm/src/js_command.rs`):
`JsCommand { sig: Signature, func: js_sys::Function }` implements `Command`. `run`
serializes `{ positionals, flags }` and the input value via the json-compatible
serializer, calls `func(args, input, ctx)`, wraps the return in
`js_sys::Promise::resolve` (tolerating sync returns), awaits via `JsFuture`, maps
rejection → `ShellError` (§8), and converts the resolved value back to `Value`. The
third argument `ctx` is `{ signal: AbortSignal, emit(line: string): void }` — the
`AbortSignal` is fired when the pipeline is aborted so TS commands can cancel
in-flight `fetch`es, and `emit` is the TS face of `ctx.emit_line`, giving both
languages progressive output despite collected pipes.

## 5. Rust ↔ TS boundary

All protocol types (`HostMsg`, `EngineEvent`, `LayoutSnapshot`, `Effects`,
`Signature`, `Value` as a tagged union) live in `bterm-core/src/protocol.rs`, derive
`Serialize/Deserialize + Tsify`, and are re-exported from `index.ts` — the `.d.ts`
can never drift from Rust. Even though execution isn't an actor, the protocol
vocabulary is real: native tests feed `HostMsg`s and assert `EngineEvent` streams
(§9), and a future worker backend moves dispatch behind `postMessage` with no core
change.

WASM exports (one class; all `&self`, all delegating through
`with_engine`/`spawn_local`):

```rust
#[wasm_bindgen]
impl BtermCore {
    pub fn new(on_event: js_sys::Function) -> BtermCore;
    pub fn feed(&self, pane: u32, data: &str) -> JsValue;      // sync -> Effects
    pub fn resize(&self, pane: u32, cols: u16, rows: u16);     // sync
    pub fn dispatch(&self, msg: JsValue);                      // HostMsg: prefix keys, mux actions
    pub fn run(&self, pane: u32, line: &str) -> js_sys::Promise;  // resolves to final Value
    pub fn register_command(&self, sig: JsValue, f: js_sys::Function) -> Result<(), JsValue>;
    pub fn unregister_command(&self, name: &str);
    pub fn snapshot(&self) -> JsValue;                          // full LayoutSnapshot
    pub fn dispose(&self);
}
```

Events (one callback, tagged union): `paneOutput`, `layoutChanged` (full snapshot, TS
reconciles by pane id), `paneOpened/Closed`, `sessionClosed`, `statusLine`,
`prefixState { active }`, `title`, `bell`, `fatal`. Full snapshots, no diffs — the
tree is tiny.

TS wrapper:

```ts
export class BrowserTerminal {
  static async create(opts?: { mount?: HTMLElement; theme?: Theme;
                               prefixKey?: string; wasmUrl?: string | URL }): Promise<BrowserTerminal>;
  registerCommand(sig: CommandSpec,                             // optional fields — no mandatory []
    fn: (args: { positionals: Value[]; flags: Record<string, Value> },
         input: Value,
         ctx: { signal: AbortSignal; emit(line: string): void })
        => unknown | Promise<unknown>): void;
  unregisterCommand(name: string): void;
  run(line: string, opts?: { session?: string }): Promise<Value>;   // programmatic exec, typed result
  on<E extends EventName>(e: E, cb: (ev: EventOf<E>) => void): Unsubscribe;
  show(): void; hide(): void; toggle(): void;
  dispose(): void;
}
```

`run()` returning `Promise<Value>` is the programmatic-scripting story; plain
returned JS values auto-convert (arrays of objects → `List<Record>` → tables). The
demo money shot:

```ts
const bt = await BrowserTerminal.create();
bt.registerCommand(
  { name: 'links', summary: 'List links on the host page', flags: [{ long: 'limit', shape: 'int' }] },
  ({ flags }) => [...document.querySelectorAll('a')]
      .slice(0, Number(flags.limit ?? 100))
      .map(a => ({ text: a.textContent?.trim() ?? '', href: a.href }))
);
// in the terminal:  links --limit 20 | where text ne '' | first 5
```

**Multi-instance policy:** the WASM engine is a singleton per page. A second
`BrowserTerminal.create()` throws with a clear message ("one instance per page in v1;
call dispose() first"). Isolated engines are a v2 item.

**Teardown:** `dispose()` aborts every `Pane.running` handle, drains and drops the
event queue, detaches the JS event callback (subsequent emissions are no-ops),
disposes every xterm (addons first), removes panel DOM, and removes all listeners.
In-flight `spawn_local` tasks observe abortion at their next await; their `ctx`
handles hold a disposed flag so late `emit`s are dropped, not written to dead
terminals.

## 6. Multiplexer & UI

**v1 tmux surface:** prefix `Ctrl-B` (configurable via `prefixKey`) then `%`
split-right, `"` split-down, `c` new window, `n`/`p` window cycle, `x` kill pane,
`o`/arrows focus pane, `z` zoom pane, `s` session picker, `(`/`)` session cycle, `d`
hide panel, `:` command prompt (the shell itself — `mux split --right`,
`session new --name work`). **Everything-is-a-command:** the keymap in
`bterm-core/mux/keys.rs` maps prefix+key → command string → evaluated in the active
pane; TS only knows the prefix chord.

**Prefix capture & feedback:** `attachCustomKeyEventHandler` per xterm (keydown only;
guard `isComposing`/`keyCode===229`). On prefix: swallow,
`dispatch(HostMsg::PrefixKey)`, and show a status-bar **PREFIX** cell plus a 2px
accent outline on the focused pane, auto-cleared after 1.5s or on the next key.

**Layout ownership:** Rust owns the tree and the math —
`layout(&LayoutNode, Rect) -> Vec<(PaneId, Rect)>` in fractions, pure and
unit-tested. TS owns pixels: absolutely positioned pane divs sized from percentage
rects; `ResizeObserver` → `FitAddon.fit()` → `resize(pane, cols, rows)`. **Divider
drag**: TS captures pointer on divider elements, calls a sync
`dispatch(Action::ResizeSplit { path, fraction })`, re-reads the snapshot — Rust
stays authoritative, drags feel native.

**Floating panels** (`panels.ts`): one panel per session, bottom-docked by default,
drag by header (`transform: translate`), **edge resize**, minimize-to-pill in a
bottom dock strip, click-to-front z-order, DOM status bar
(`[session] 1:main* 2:logs | PREFIX`) with clickable window tabs. **Host
coexistence:** all panel DOM lives inside a **Shadow DOM root** on a single host
element (`z-index: 2147483000`) — no CSS bleed in either direction; the library adds
no window-level key listeners by default (the optional `` Ctrl+` `` global toggle is
opt-in via `create({ globalToggle: true })`).

**xterm lifecycle:** `@xterm/xterm` 6.x, one `Terminal` + `FitAddon` per pane,
`scrollback: 2000`, `allowTransparency`. **DOM renderer only in v1** (≤6 panes;
dodges WebGL context caps and dispose leaks — WebGL-on-focus is the first perf
follow-up). Kill order: dispose addons, then `term.dispose()`, then remove container.

## 7. Rendering pipeline & line editor

`bterm-core/src/render/`: `render(v: &Value, width: u16) -> String`, pure ANSI,
called **exactly once** per pipeline (PowerShell's Out-Default lesson — commands
never format; `table`/`to json` exist for forcing a `Str` mid-pipe). `List<Record>` →
box-drawn table (column set = union of keys in first-seen order; widths via
`unicode-width`, widest column shrunk with `…`; bold header; numbers right-aligned).
`Record` → key/value pairs; `List<scalar>` → indexed rows; scalars colored by type;
nested values compact (`[3 items]`). All output `\n`→`\r\n` converted.

**Write backpressure:** `panes.ts` writes through a small queue that awaits
`term.write` callbacks; above 256KB pending it chunks and defers via the callback
chain so a giant collected table can never hit xterm's 50MB buffer cap or freeze the
frame. (With collected pipes there is no Rust-side stream to pause, so backpressure
is purely a TS write-queue concern.)

**Line editor** (`editor.rs`, Rust, per pane): pure `feed(bytes) -> Effects`. v1 set:
printable insert, backspace/delete, ←/→, Home/End, `C-a`/`C-e`/`C-u`/`C-k`/`C-w`,
↑/↓ history (cap 200), `C-l` clear, `C-c` = cancel edit line *or* abort running
pipeline (§4), Enter → submit. Redraw: `\r\x1b[K` + prompt + buffer + cursor
reposition. Prompt `❯ ` (green/red by last exit status). `Effects` reserves a
`CompletionRequest` variant as the v2 hook. **Paste:** bracketed paste mode is
enabled on every xterm; the editor recognizes `ESC[200~…ESC[201~` and inserts the
payload literally with embedded `\r`/`\n` converted to single spaces — pasted
multi-line text can never auto-execute; only a user-typed Enter submits. No `C-r`
search, no completion in v1.

## 8. Error handling

```rust
pub struct ShellError { kind: ErrorKind, msg: String, span: Option<Span>, help: Option<String> }
```

Handwritten caret renderer (~80 lines): source line, underline at span, red label,
dim `help:` line, Levenshtein suggestions for unknown commands/flags. Parse and
runtime errors share it. Errors print red to the originating pane and abort the
pipeline.

**TS errors are first-class:** a TS command may throw/reject with anything;
`Error.message` is used, stack logged to `console.error`. Additionally, throwing (or
returning from a rejected promise) an object shaped `{ message, help?, code? }`
produces a full `ShellError` with the help line rendered — TS commands get the same
diagnostic polish as builtins. Span = the call head's span, so the user sees which
command failed. A throwing TS command never breaks the engine.

**Panic policy:** panics are bugs. `panic="abort"` in release,
`console_error_panic_hook` behind a `debug` feature. clippy `unwrap_used = deny` on
`bterm-core` and `bterm-wasm`. On abort, the TS wrapper detects the dead instance
(export throws), paints "terminal crashed — reload" into all panes, and disables
input. No auto-restart, no persistence-based recovery in v1.

## 9. Testing strategy

- **Native `cargo test -p bterm-core` (the bulk, milliseconds):** lexer token/span
  goldens; parser AST + recovery snapshots (`insta`); signature binding/coercion;
  pipeline eval with fake sync commands (10-line block-on for ready futures);
  `render` snapshots at multiple widths; layout math invariants (fractions sum to 1,
  no zero-size panes); editor byte-in/ANSI-out transcripts (incl. bracketed paste);
  **protocol tests** — instantiate a native `Engine`, feed `HostMsg` sequences,
  assert the `EngineEvent` queue for splits, session forking, kill-pane refocus,
  Ctrl-C abort. This natively verifies the tmux surface — the hardest requirement —
  without a browser.
- **`wasm-bindgen-test` (headless Chrome, ~8 tests):** Value↔JS round-trips through
  the json-compatible serializer (records arrive as plain objects); `JsCommand` async
  invocation, sync return, rejection→ShellError, rich-error object, AbortSignal
  firing; `register_command` collision behavior; event callback after `run`.
- **Playwright on the demo (thin, milestone 7):** type `links | where text ne ''` →
  assert table box-drawing; `C-b %` → two xterms; session switch swaps panel content;
  dispose leaves no listeners. The demo doubles as the manual checklist
  (`demo/TESTING.md`) during development.
- `bterm-cli` is the everyday manual harness for language/engine work.

## 10. Milestones (each independently verifiable)

1. **Toolchain skeleton** — workspace, `just wasm`, npm package with `init()`, demo
   mounts one xterm and round-trips `feed` echo through WASM. *Proves the whole
   toolchain and the sync hot path before any real code.*
2. **Language + engine (native)** — Value, lexer, parser, signatures, binding, eval,
   `ShellError` carets, builtins (`echo, get, where, head, tail, length, sort-by,
   str upcase/downcase, to/from json, table, help, history, clear`), `bterm-cli`
   REPL. *Verify: cargo test green; pipe commands in a native terminal.*
3. **REPL pane in browser** — line editor, event queue + flush, renderer wired
   through the boundary; one working structured shell pane. *Verify:
   `echo a b c | str upcase` renders; bad flag shows caret + did-you-mean.*
4. **TS commands + cancellation** — `register_command`, `JsCommand`,
   AbortSignal/ctx.emit, rich-error mapping, `run() -> Promise<Value>`. *Verify:
   wasm-bindgen tests; demo registers `links` in five lines and pipes it into
   `where`.*
5. **Multiplexer** — mux tree, layout math, `mux`/`session` command families, prefix
   keymap, per-pane lifecycle, resize plumbing, protocol tests. *Verify: `C-b %`
   splits; slow fetch in pane A while typing in pane B.*
6. **Sessions + floating panels** — session forking/picker/switching, Shadow-DOM
   panel manager (drag/edge-resize/minimize/z-order), status bar with PREFIX cell,
   divider drag. *Verify: native protocol tests + full demo checklist.*
7. **Ship** — help/error polish, `dispose()` audit, size pass (`twiggy`, ≤350KB
   pre-gzip), README with bundler + CDN recipes, `npm pack` dry-run, Playwright suite
   green, demo deployed as the acceptance artifact.

---

## Key risks & mitigations

| Risk | Mitigation |
|---|---|
| RefCell borrow across await / JS reentrancy panic | `with_engine` sync-closure is the *only* state access; no `&mut self` exports; events queued inside borrows, flushed only outside — enforced by module privacy + debug assertion |
| Keystroke echo latency | Sync `feed -> Effects`, echo written same tick; measured in milestone 1 before anything else is built |
| Collected pipes prove insufficient | `PipelineData` enum is the seam; `ctx.emit_line` covers progressive output now; migration is an additive variant + per-builtin mechanical change |
| Hung TS command wedges a pane | Per-pipeline `AbortHandle` + `AbortSignal` handed to TS callbacks; Ctrl-C always recovers the pane |
| Giant table output freezes/overflows xterm | TS write queue awaiting `term.write` callbacks with a 256KB watermark |
| Toolchain/packaging surprises late | Milestone 1 is the full build → wasm-bindgen → npm → browser round-trip |
| Host-page interference | Shadow DOM for all panel UI; no default window-level listeners; configurable prefix key; singleton policy explicit |
| WASM binary bloat | Minimal dep set, handwritten lexer, `wasm-opt -Oz`, twiggy audit gate at milestone 7 |

## Accepted-simplification register (living doc)

| Cut | Rework cost | Why accepted |
|---|---|---|
| Collected pipes (no `Stream` variant used) | Medium — touches builtins | async+lazy don't compose cheaply; browser data is small; enum seam + `emit_line` in place |
| `where` uses word operators (`ne`, `contains`) | Low — `!=` sugar is additive parsing | keeps v1 grammar tiny; demos actually parse |
| DOM renderer only | Low — additive addon swap | ≤6 panes; dodges WebGL context caps/leaks |
| No spans on Values | Medium | AST/error spans cover the real diagnostic UX |
| Full-snapshot layout events | ~Zero | tree is tiny |
| Singleton engine per page | Medium — needs instance-scoped state | no v1 use case for two terminals |

## Deliberately deferred to v2

- Streaming pipelines (`PipelineData::Stream`) and streaming TS commands;
  row-incremental table paint
- ~~Comparison-operator syntax (`!=`, `>`), closures, `&&`/`||`, cell paths~~ —
  **landed**: `{|x| …}` closures with a full expression language, working
  identically in the browser and the native CLI
- Redirection (`>`), list indexing (`$x.0`), closures as first-class values
- Tab completion (editor reserves `Effects::CompletionRequest`) and `C-r` history
  search
- Session persistence (localStorage/IndexedDB), detach/reattach, panic-state restore
- WebGL renderer on the focused pane; split animations
- Web-worker backend (protocol types already serialize)
- Multiple `BrowserTerminal` instances per page
- Status-bar format strings (`#{...}`), `bind` for user keybindings
- Playwright expansion beyond the smoke suite; typedoc-generated API docs; protocol
  versioning field on `EngineEvent`
