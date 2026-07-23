//! The engine: the mux tree (sessions → windows → panes), the command
//! registry, and the outgoing event queue.
//!
//! Concurrency contract (enforced structurally in the wasm layer): all
//! engine access goes through `EngineAccess::with`, a synchronous closure —
//! no borrow can cross an await. Code holding the engine never invokes the
//! host callback; events are queued and flushed after the closure returns.
//! `execute_line` is the one shared async path, used verbatim by native
//! protocol tests and the browser.

use crate::editor::Effects;
use crate::error::ShellError;
use crate::eval::{eval_line, CommandSource};
use crate::mux::{keys, layout_window, Dir, FocusDir, Mux, PaneShell, Rect};
use crate::parse::parse;
use crate::protocol::{EngineEvent, HostMsg, LayoutSnapshot, PaneInfo, SessionInfo, WindowInfo};
use crate::callable::{FnCompiler, NoFnCompiler};
use crate::matcher::{PatternMatcher, SubstringMatcher};
use crate::registry::{Command, CommandRegistry, ExecContext, HostHooks, MuxAction, PipelineData};
use crate::render::render;
use crate::value::Value;
use std::collections::VecDeque;
use std::rc::Rc;

pub struct Engine {
    pub registry: CommandRegistry,
    pub mux: Mux,
    /// Supplied by the host: JS `RegExp` in the browser, substring natively.
    matcher: Rc<dyn PatternMatcher>,
    /// Supplied by the host: JavaScript in the browser, absent natively.
    fn_compiler: Rc<dyn FnCompiler>,
    prefix_armed: bool,
    events: VecDeque<EngineEvent>,
}

/// What `handle_msg` wants the host to do after the borrow closes.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct MsgResult {
    /// Evaluate this command line in this pane (prefix keymap hit).
    pub run: Option<(u32, String)>,
    /// Abort any in-flight tasks for these panes (they closed).
    pub closed_panes: Vec<u32>,
}

impl Default for Engine {
    fn default() -> Self {
        Self::new()
    }
}

impl Engine {
    pub fn new() -> Self {
        let mut registry = CommandRegistry::new();
        crate::builtins::register_all(&mut registry);
        Engine {
            registry,
            mux: Mux::new(),
            matcher: Rc::new(SubstringMatcher),
            fn_compiler: Rc::new(NoFnCompiler),
            prefix_armed: false,
            events: VecDeque::new(),
        }
    }

    /// Install the host's pattern engine (the browser passes a JS
    /// `RegExp`-backed matcher).
    pub fn set_matcher(&mut self, matcher: Rc<dyn PatternMatcher>) {
        self.matcher = matcher;
    }

    /// Cloned out under a short borrow so compilation — which may call into
    /// JS — happens with no engine borrow held.
    pub fn matcher(&self) -> Rc<dyn PatternMatcher> {
        self.matcher.clone()
    }

    /// Install the host's scripting engine for inline callables.
    pub fn set_fn_compiler(&mut self, compiler: Rc<dyn FnCompiler>) {
        self.fn_compiler = compiler;
    }

    pub fn fn_compiler(&self) -> Rc<dyn FnCompiler> {
        self.fn_compiler.clone()
    }

    pub fn pane(&self, id: u32) -> Option<&PaneShell> {
        self.mux.pane(id)
    }

    pub fn pane_mut(&mut self, id: u32) -> Option<&mut PaneShell> {
        self.mux.pane_mut(id)
    }

    /// Sync input hot path: feed raw input to the pane's editor.
    pub fn feed(&mut self, pane: u32, data: &str) -> Effects {
        match self.mux.pane_mut(pane) {
            Some(p) => p.editor.feed(data),
            None => Effects::default(),
        }
    }

    pub fn resize(&mut self, pane: u32, cols: u16, rows: u16) {
        if let Some(p) = self.mux.pane_mut(pane) {
            p.cols = cols;
            p.rows = rows;
        }
    }

    /// Queue pane output. `text` uses `\n` endings (or raw control
    /// sequences); it is CRLF-converted here, the single choke point.
    pub fn emit_output(&mut self, pane: u32, text: &str) {
        self.emit(EngineEvent::PaneOutput { pane, data: crlf(text) });
    }

    pub fn emit(&mut self, event: EngineEvent) {
        self.events.push_back(event);
    }

    pub fn drain_events(&mut self) -> Vec<EngineEvent> {
        self.events.drain(..).collect()
    }

    /// Fresh prompt line for a pane (used after output settles).
    pub fn prompt_line(&self, pane: u32) -> String {
        self.mux
            .pane(pane)
            .map(|p| p.editor.prompt_line())
            .unwrap_or_default()
    }

    pub fn snapshot(&self) -> LayoutSnapshot {
        let session = self.mux.active_session();
        let window = self.mux.active_window();
        LayoutSnapshot {
            sessions: self
                .mux
                .sessions
                .values()
                .map(|s| SessionInfo {
                    id: s.id,
                    name: s.name.clone(),
                    active: s.id == self.mux.active_session,
                })
                .collect(),
            windows: session
                .windows
                .values()
                .map(|w| WindowInfo {
                    id: w.id,
                    name: w.name.clone(),
                    active: w.id == session.active_window,
                })
                .collect(),
            panes: layout_window(window, Rect::FULL)
                .into_iter()
                .map(|(pane, rect)| PaneInfo { pane, rect, active: pane == window.active_pane })
                .collect(),
            dividers: crate::mux::dividers(window, Rect::FULL),
            active_pane: window.active_pane,
            zoomed: window.zoomed,
        }
    }

    /// Host control messages (prefix chord, clicks, divider drags).
    pub fn handle_msg(&mut self, msg: HostMsg) -> MsgResult {
        match msg {
            HostMsg::PrefixKey => {
                self.prefix_armed = true;
                self.emit(EngineEvent::PrefixState { active: true });
                MsgResult::default()
            }
            HostMsg::Key { key } => {
                let was_armed = self.prefix_armed;
                self.prefix_armed = false;
                self.emit(EngineEvent::PrefixState { active: false });
                if was_armed {
                    if let Some(cmd) = keys::keymap(&key) {
                        return MsgResult {
                            run: Some((self.mux.active_pane(), cmd.to_string())),
                            ..Default::default()
                        };
                    }
                }
                MsgResult::default()
            }
            HostMsg::FocusPane { pane } => {
                let outcome = self.mux.focus_pane(pane);
                self.apply_outcome(&outcome);
                MsgResult { closed_panes: outcome.closed_panes, ..Default::default() }
            }
            HostMsg::FocusWindow { window } => {
                let outcome = self.mux.focus_window(window);
                self.apply_outcome(&outcome);
                MsgResult::default()
            }
            HostMsg::FocusSession { session } => {
                let outcome = self.mux.focus_session(session);
                self.apply_outcome(&outcome);
                MsgResult::default()
            }
            HostMsg::ResizeSplit { path, fraction } => {
                let outcome = self.mux.resize_split(&path, fraction);
                self.apply_outcome(&outcome);
                MsgResult::default()
            }
        }
    }

    /// Apply a mux mutation from a shell command (`mux …` / `session …`).
    /// Returns the command's value plus the pane ids whose tasks must be
    /// aborted by the host.
    pub fn mux_apply(&mut self, action: MuxAction) -> (Result<Value, ShellError>, Vec<u32>) {
        use MuxAction::*;
        let (value, outcome) = match action {
            SplitRight => {
                let (_, o) = self.mux.split(Dir::Row);
                (Ok(Value::Null), o)
            }
            SplitDown => {
                let (_, o) = self.mux.split(Dir::Col);
                (Ok(Value::Null), o)
            }
            WindowNew => {
                let (_, o) = self.mux.new_window();
                (Ok(Value::Null), o)
            }
            WindowNext => (Ok(Value::Null), self.mux.cycle_window(true)),
            WindowPrev => (Ok(Value::Null), self.mux.cycle_window(false)),
            KillPane => {
                let o = self.mux.kill_active_pane();
                (Ok(Value::Null), o)
            }
            Focus(dir) => {
                let dir = match dir.as_str() {
                    "next" => FocusDir::Next,
                    "left" => FocusDir::Left,
                    "right" => FocusDir::Right,
                    "up" => FocusDir::Up,
                    "down" => FocusDir::Down,
                    other => {
                        return (
                            Err(ShellError::runtime(format!("unknown focus direction `{other}`"))
                                .with_help("use next, left, right, up or down")),
                            Vec::new(),
                        )
                    }
                };
                (Ok(Value::Null), self.mux.focus(dir))
            }
            Zoom => (Ok(Value::Null), self.mux.toggle_zoom()),
            Hide => {
                self.emit(EngineEvent::HidePanel);
                (Ok(Value::Null), Default::default())
            }
            SessionNew { name } => {
                let (_, o) = self.mux.new_session(name);
                (Ok(Value::Null), o)
            }
            SessionNext => (Ok(Value::Null), self.mux.cycle_session(true)),
            SessionPrev => (Ok(Value::Null), self.mux.cycle_session(false)),
            SessionSwitch { name } => match self.mux.switch_session(&name) {
                Ok(o) => (Ok(Value::Null), o),
                Err(msg) => {
                    return (
                        Err(ShellError::runtime(msg)
                            .with_help("run `session list` to see sessions")),
                        Vec::new(),
                    )
                }
            },
            SessionList => {
                let rows: Vec<Value> = self
                    .mux
                    .sessions
                    .values()
                    .map(|s| {
                        Value::record([
                            ("name".to_string(), Value::Str(s.name.clone())),
                            ("windows".to_string(), Value::Int(s.windows.len() as i64)),
                            (
                                "active".to_string(),
                                Value::Bool(s.id == self.mux.active_session),
                            ),
                        ])
                    })
                    .collect();
                (Ok(Value::List(rows)), Default::default())
            }
        };
        let closed = outcome.closed_panes.clone();
        self.apply_outcome(&outcome);
        (value, closed)
    }

    fn apply_outcome(&mut self, outcome: &crate::mux::MuxOutcome) {
        for pane in &outcome.opened_panes {
            self.emit(EngineEvent::PaneOpened { pane: *pane });
            // The new xterm needs a prompt to be usable.
            let prompt = self.prompt_line(*pane);
            self.emit(EngineEvent::PaneOutput { pane: *pane, data: prompt });
        }
        for pane in &outcome.closed_panes {
            self.emit(EngineEvent::PaneClosed { pane: *pane });
        }
        for session in &outcome.closed_sessions {
            self.emit(EngineEvent::SessionClosed { session: *session });
        }
        if outcome.layout_changed {
            let snapshot = self.snapshot();
            self.emit(EngineEvent::LayoutChanged { snapshot });
        }
    }
}

/// Normalize then convert to CRLF. Lone `\r` (cursor-to-column-0 control)
/// is preserved.
pub fn crlf(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\n', "\r\n")
}

/// How async tasks reach the engine: a synchronous scoped borrow. The wasm
/// layer implements this over `thread_local!` + flush-after-scope; native
/// tests use `Rc<RefCell<Engine>>`.
pub trait EngineAccess: Clone + 'static {
    fn with<R>(&self, f: impl FnOnce(&mut Engine) -> R) -> R;
    /// Called after a `with` scope that queued events; the wasm layer
    /// flushes the queue to the JS callback here (no borrow held).
    fn events_ready(&self);
    /// Panes closed by a mux mutation — the wasm layer aborts their
    /// in-flight tasks here (JS AbortControllers fire outside any borrow).
    fn panes_closed(&self, _panes: &[u32]) {}
    /// Resolve `@name` against the host's registered-function table, which
    /// lives outside the engine (the wasm layer owns the JS handles).
    fn lookup_fn(&self, name: &str) -> Result<Rc<dyn crate::callable::HostFn>, String> {
        Err(format!(
            "no registered function `{name}`; this host cannot register functions"
        ))
    }
}

impl EngineAccess for Rc<std::cell::RefCell<Engine>> {
    fn with<R>(&self, f: impl FnOnce(&mut Engine) -> R) -> R {
        f(&mut self.borrow_mut())
    }

    fn events_ready(&self) {
        // Native harnesses drain the queue explicitly.
    }
}

/// Command lookup for `eval` that clones the Rc out under a short borrow.
struct EngineCommands<A: EngineAccess>(A);

impl<A: EngineAccess> CommandSource for EngineCommands<A> {
    fn lookup(&self, words: &[String]) -> Option<(Rc<dyn Command>, usize)> {
        self.0.with(|e| e.registry.lookup(words))
    }

    fn group_help(&self, words: &[String]) -> Option<String> {
        self.0.with(|e| e.registry.group_help(words))
    }

    fn unknown_command_error(&self, words: &[crate::ast::Spanned<String>]) -> ShellError {
        self.0.with(|e| e.registry.unknown_command_error(words))
    }
}

/// Host hooks for commands running inside a pane.
struct EngineHost<A: EngineAccess> {
    access: A,
    pane: u32,
}

impl<A: EngineAccess> HostHooks for EngineHost<A> {
    fn history(&self) -> Vec<String> {
        self.access
            .with(|e| e.pane(self.pane).map(|p| p.editor.history().to_vec()))
            .unwrap_or_default()
    }

    fn request_clear(&self) {
        self.access.with(|e| e.emit_output(self.pane, "\x1b[2J\x1b[H"));
        self.access.events_ready();
    }

    fn help_overview(&self) -> Vec<(String, String)> {
        self.access.with(|e| {
            e.registry
                .names()
                .into_iter()
                .filter_map(|name| {
                    e.registry
                        .get(&name)
                        .map(|cmd| (name, cmd.signature().summary.clone()))
                })
                .collect()
        })
    }

    fn help_for(&self, name: &str) -> Option<String> {
        self.access
            .with(|e| e.registry.get(name).map(|cmd| cmd.signature().render_help()))
    }

    fn mux_action(&self, action: MuxAction) -> Result<Value, ShellError> {
        let (result, closed) = self.access.with(|e| e.mux_apply(action));
        if !closed.is_empty() {
            self.access.panes_closed(&closed);
        }
        self.access.events_ready();
        result
    }

    fn compile_pattern(
        &self,
        pattern: &str,
        case_insensitive: bool,
    ) -> Result<Box<dyn crate::matcher::Pattern>, String> {
        // Borrow ends with `with`; compiling may call into JS.
        let matcher = self.access.with(|e| e.matcher());
        matcher.compile(pattern, case_insensitive)
    }

    fn pattern_dialect(&self) -> &'static str {
        self.access.with(|e| e.matcher().dialect())
    }

    fn compile_fn(&self, source: &str) -> Result<Rc<dyn crate::callable::HostFn>, String> {
        // Borrow ends with `with`; compiling may call into JS.
        let compiler = self.access.with(|e| e.fn_compiler());
        compiler.compile(source)
    }

    fn lookup_fn(&self, name: &str) -> Result<Rc<dyn crate::callable::HostFn>, String> {
        self.access.lookup_fn(name)
    }
}

fn make_ctx<A: EngineAccess>(
    access: &A,
    pane: u32,
    run_id: u64,
    sink: Rc<dyn crate::sink::Sink>,
) -> ExecContext {
    let cols = access.with(|e| e.pane(pane).map(|p| p.cols).unwrap_or(80));
    ExecContext {
        host: Rc::new(EngineHost { access: access.clone(), pane }),
        sink,
        width: cols,
        pane,
        run_id,
    }
}

fn scope_for_pane<A: EngineAccess>(access: &A, pane: u32) -> crate::signature::Scope {
    access.with(|e| {
        e.mux
            .session_of_pane(pane)
            .and_then(|sid| e.mux.sessions.get(&sid))
            .map(|s| s.vars.clone())
            .unwrap_or_default()
    })
}

/// Evaluate one submitted line in a pane: parse → eval → render → prompt.
/// The single shared execution path for native tests and the browser.
pub async fn execute_line<A: EngineAccess>(access: A, pane: u32, line: String, run_id: u64) {
    let parsed = parse(&line);
    if !parsed.errors.is_empty() {
        access.with(|e| {
            for err in &parsed.errors {
                e.emit_output(pane, &err.render(&line));
            }
            finish_pane(e, pane, false);
        });
        access.events_ready();
        return;
    }

    // Task 4 replaces this with a real PaneSink.
    let ctx = make_ctx(&access, pane, run_id, Rc::new(crate::sink::NullSink));
    let cols = ctx.width;
    let scope = scope_for_pane(&access, pane);
    let source = EngineCommands(access.clone());

    let (results, error) = eval_line(&parsed.line, &source, &ctx, &scope).await;

    access.with(|e| {
        // Completed pipelines render even when a later one failed.
        for data in &results {
            match data {
                PipelineData::Value(v) => {
                    let rendered = render(v, cols);
                    e.emit_output(pane, &rendered);
                }
                // Already formatted by us — printed as-is, escapes intact.
                PipelineData::Rendered(s) => e.emit_output(pane, &format!("{s}\n")),
                PipelineData::Empty => {}
            }
        }
        match &error {
            Some(err) => {
                e.emit_output(pane, &err.render(&line));
                finish_pane(e, pane, false);
            }
            None => finish_pane(e, pane, true),
        }
    });
    access.events_ready();
}

/// Programmatic execution (the TS `run()` API): parse and evaluate a line,
/// returning the final pipeline's value without touching the pane's prompt
/// or rendering anything. Commands' `ctx.emit` output still reaches the
/// pane.
pub async fn eval_to_value<A: EngineAccess>(
    access: A,
    pane: u32,
    line: String,
    run_id: u64,
) -> Result<Value, ShellError> {
    let parsed = parse(&line);
    if let Some(err) = parsed.errors.into_iter().next() {
        return Err(err);
    }
    // Task 7 replaces this with a capturing sink.
    let ctx = make_ctx(&access, pane, run_id, Rc::new(crate::sink::NullSink));
    let scope = scope_for_pane(&access, pane);
    let source = EngineCommands(access.clone());
    let (results, error) = eval_line(&parsed.line, &source, &ctx, &scope).await;
    if let Some(err) = error {
        return Err(err);
    }
    Ok(results
        .into_iter()
        .last()
        .map(PipelineData::into_value)
        .unwrap_or(Value::Null))
}

/// Mark the pane idle, color the prompt by status, and print it. A pane
/// that was closed mid-run (kill-pane while a task was in flight) simply
/// produces no prompt.
fn finish_pane(e: &mut Engine, pane: u32, ok: bool) {
    if let Some(p) = e.pane_mut(pane) {
        p.running = false;
        p.editor.set_last_status(ok);
    }
    let prompt = e.prompt_line(pane);
    if !prompt.is_empty() {
        e.emit(EngineEvent::PaneOutput { pane, data: prompt });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::block_on;
    use std::cell::RefCell;

    fn engine() -> Rc<RefCell<Engine>> {
        Rc::new(RefCell::new(Engine::new()))
    }

    fn active_pane(access: &Rc<RefCell<Engine>>) -> u32 {
        access.with(|e| e.mux.active_pane())
    }

    fn feed_and_run(access: &Rc<RefCell<Engine>>, input: &str) -> Vec<EngineEvent> {
        let pane = active_pane(access);
        let fx = access.with(|e| e.feed(pane, input));
        for line in fx.submitted {
            block_on(execute_line(access.clone(), pane, line, 0));
        }
        access.with(|e| e.drain_events())
    }

    fn output_text(events: &[EngineEvent]) -> String {
        events
            .iter()
            .map(|ev| match ev {
                EngineEvent::PaneOutput { data, .. } => data.clone(),
                _ => String::new(),
            })
            .collect()
    }

    #[test]
    fn pipeline_renders_to_pane_events() {
        let access = engine();
        let events = feed_and_run(&access, "echo a b c | str upcase\r");
        let out = output_text(&events);
        assert!(out.contains("A"), "output: {out:?}");
        assert!(out.contains("\r\n"), "CRLF conversion applied");
        assert!(out.contains("❯"), "prompt reprinted after output");
    }

    #[test]
    fn parse_error_renders_caret_and_red_prompt() {
        let access = engine();
        let events = feed_and_run(&access, "echo &\r");
        let out = output_text(&events);
        assert!(out.contains("not supported yet"));
        assert!(out.contains("^"));
        assert!(out.contains("\x1b[31m❯"), "red prompt after failure");
    }

    #[test]
    fn unknown_flag_shows_did_you_mean() {
        let access = engine();
        let events = feed_and_run(&access, "sort-by n --reverze\r");
        let out = output_text(&events);
        assert!(out.contains("unknown flag"), "output: {out:?}");
        assert!(out.contains("did you mean `--reverse`?"));
    }

    #[test]
    fn history_builtin_sees_editor_history() {
        let access = engine();
        feed_and_run(&access, "echo one\r");
        let events = feed_and_run(&access, "history\r");
        let out = output_text(&events);
        assert!(out.contains("echo one"), "output: {out:?}");
    }

    // --- protocol tests: HostMsg sequences → EngineEvent stream ---

    #[test]
    fn prefix_percent_splits_and_snapshots() {
        let access = engine();
        let first = active_pane(&access);

        let result = access.with(|e| {
            e.handle_msg(HostMsg::PrefixKey);
            e.handle_msg(HostMsg::Key { key: "%".to_string() })
        });
        let (pane, cmd) = result.run.expect("keymap resolves");
        assert_eq!(pane, first);
        assert_eq!(cmd, "mux split --right");
        block_on(execute_line(access.clone(), pane, cmd, 0));

        let events = access.with(|e| e.drain_events());
        assert!(events.iter().any(|e| matches!(e, EngineEvent::PrefixState { active: true })));
        assert!(events.iter().any(|e| matches!(e, EngineEvent::PaneOpened { .. })));
        let snapshot = events
            .iter()
            .rev()
            .find_map(|e| match e {
                EngineEvent::LayoutChanged { snapshot } => Some(snapshot.clone()),
                _ => None,
            })
            .expect("layout event");
        assert_eq!(snapshot.panes.len(), 2);
        assert!((snapshot.panes[0].rect.w - 0.5).abs() < 1e-4);
        assert_ne!(snapshot.active_pane, first, "new pane focused");
    }

    #[test]
    fn kill_pane_refocuses_and_closes() {
        let access = engine();
        let first = active_pane(&access);
        block_on(execute_line(access.clone(), first, "mux split --right".into(), 0));
        let second = active_pane(&access);
        block_on(execute_line(access.clone(), second, "mux kill-pane".into(), 0));

        let events = access.with(|e| e.drain_events());
        assert!(events
            .iter()
            .any(|e| matches!(e, EngineEvent::PaneClosed { pane } if *pane == second)));
        assert_eq!(active_pane(&access), first);
        let snapshot = access.with(|e| e.snapshot());
        assert_eq!(snapshot.panes.len(), 1);
    }

    #[test]
    fn session_fork_switch_via_commands() {
        let access = engine();
        let pane_main = active_pane(&access);
        block_on(execute_line(access.clone(), pane_main, "session new work".into(), 0));
        let pane_work = active_pane(&access);
        assert_ne!(pane_main, pane_work);

        let snapshot = access.with(|e| e.snapshot());
        assert_eq!(snapshot.sessions.len(), 2);
        assert!(snapshot.sessions.iter().any(|s| s.name == "work" && s.active));

        block_on(execute_line(access.clone(), pane_work, "session switch main".into(), 0));
        assert_eq!(active_pane(&access), pane_main);

        // Each pane keeps its own shell: histories are separate.
        feed_and_run(&access, "echo in-main\r");
        let hist_main = access.with(|e| e.pane(pane_main).map(|p| p.editor.history().to_vec()));
        let hist_work = access.with(|e| e.pane(pane_work).map(|p| p.editor.history().to_vec()));
        assert!(hist_main.unwrap_or_default().contains(&"echo in-main".to_string()));
        assert!(hist_work.unwrap_or_default().is_empty());
    }

    #[test]
    fn session_list_prints_table() {
        let access = engine();
        let events = feed_and_run(&access, "session list\r");
        let out = output_text(&events);
        assert!(out.contains("main"), "output: {out:?}");
        assert!(out.contains("windows"), "table header: {out:?}");
    }

    #[test]
    fn unarmed_key_does_nothing() {
        let access = engine();
        let result = access.with(|e| e.handle_msg(HostMsg::Key { key: "%".to_string() }));
        assert_eq!(result.run, None, "no keymap without prefix");
    }

    #[test]
    fn focus_pane_msg_switches_focus() {
        let access = engine();
        let first = active_pane(&access);
        block_on(execute_line(access.clone(), first, "mux split --right".into(), 0));
        let second = active_pane(&access);
        assert_ne!(first, second);
        access.with(|e| e.handle_msg(HostMsg::FocusPane { pane: first }));
        assert_eq!(active_pane(&access), first);
    }

    #[test]
    fn resize_split_msg_updates_snapshot() {
        let access = engine();
        let first = active_pane(&access);
        block_on(execute_line(access.clone(), first, "mux split --right".into(), 0));
        access.with(|e| {
            e.handle_msg(HostMsg::ResizeSplit { path: vec![0], fraction: 0.7 })
        });
        let snapshot = access.with(|e| e.snapshot());
        assert!((snapshot.panes[0].rect.w - 0.7).abs() < 1e-4, "{:?}", snapshot.panes);
    }

    #[test]
    fn hide_command_emits_event() {
        let access = engine();
        let events = feed_and_run(&access, "mux hide\r");
        assert!(events.iter().any(|e| matches!(e, EngineEvent::HidePanel)));
    }

    #[test]
    fn mux_zoom_roundtrip_via_prefix() {
        let access = engine();
        let pane = active_pane(&access);
        block_on(execute_line(access.clone(), pane, "mux split --down".into(), 0));
        block_on(execute_line(access.clone(), active_pane(&access), "mux zoom".into(), 0));
        let snapshot = access.with(|e| e.snapshot());
        assert_eq!(snapshot.panes.len(), 1, "zoomed pane fills the window");
        assert!(snapshot.zoomed.is_some());
    }
}
