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
use crate::eval::{eval_line, eval_line_with, CommandSource};
use crate::mux::{keys, layout_window, Dir, FocusDir, Mux, PaneShell, Rect};
use crate::parse::parse;
use crate::protocol::{EngineEvent, HostMsg, LayoutSnapshot, PaneInfo, SessionInfo, WindowInfo};
use crate::callable::{FnCompiler, NoFnCompiler};
use crate::matcher::{PatternMatcher, SubstringMatcher};
use crate::registry::{Command, CommandRegistry, ExecContext, HostHooks, MuxAction, PipelineData};
use crate::render::render;
use crate::signature::Scope;
use crate::value::Value;
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;

/// SGR reset. Prefixed to every chunk of pane output — see `emit_output`.
const RESET: &str = "\x1b[0m";

pub struct Engine {
    pub registry: CommandRegistry,
    pub mux: Mux,
    /// Supplied by the host: JS `RegExp` in the browser, substring natively.
    matcher: Rc<dyn PatternMatcher>,
    /// Supplied by the host: JavaScript in the browser, absent natively.
    fn_compiler: Rc<dyn FnCompiler>,
    /// Supplied by the host: values injected as `$name`, visible to every
    /// session. Sessions keep their own `vars`, which win on collision —
    /// see `scope_for_pane`.
    host_vars: Scope,
    prefix_armed: bool,
    events: VecDeque<EngineEvent>,
    /// The in-flight progressive renderer for a pane, tagged with the run
    /// that owns it. The host's probe deadline reaches it through here to
    /// force an early commit — this crate has no clock, so the timing
    /// decision is the host's.
    ///
    /// A second line submitted while the first is still streaming
    /// (`feed`/`spawn_pipeline` starts every submitted line as its own
    /// task) replaces this entry for the pane, so the run id lets
    /// `commit_pending_render`/`probe_settled` tell "this is still my
    /// renderer" from "a newer run took over this pane" — otherwise an
    /// older run's deadline would silently drive the newer run's renderer
    /// and abandon its own.
    pending_render: HashMap<u32, (u64, Rc<RefCell<crate::render::stream::StreamRenderer>>)>,
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
            host_vars: Scope::new(),
            prefix_armed: false,
            events: VecDeque::new(),
            pending_render: HashMap::new(),
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
    ///
    /// Every chunk is also reset-prefixed here rather than at each call
    /// site. A command's SGR can outlive the command — an aborted body never
    /// resumes, so its trailing reset never runs — and the engine's own
    /// paints are then drawn in the command's colours. That is not only
    /// cosmetic for the prompt: `prompt_line` starts with `\r\x1b[K`, and
    /// erase-to-end-of-line fills with the *current* background, so a leaked
    /// background repaints the shell's own line. Doing it at the choke point
    /// keeps the invariant one line of code instead of a reset scattered
    /// over every emitter, and means a new emitter cannot forget it.
    pub fn emit_output(&mut self, pane: u32, text: &str) {
        if text.is_empty() {
            return;
        }
        self.emit(EngineEvent::PaneOutput { pane, data: format!("{RESET}{}", crlf(text)) });
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
            self.emit_output(*pane, &prompt);
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

    /// The host's probe deadline fired for `run_id`: commit whatever this
    /// pane's progressive renderer has buffered, so a slow source paints
    /// without waiting for more rows. `None` if there is nothing pending,
    /// if it already committed (reaching `PROBE_ROWS`, or a previous
    /// deadline), or — critically — if a *different* run has since taken
    /// over this pane: without the run-id check, an older run's timer
    /// would reach into a newer run's renderer, silently forcing its probe
    /// while leaving the older run's own renderer (now orphaned) unpainted.
    ///
    /// Returns the text to emit; the caller emits it after this borrow
    /// closes (emitting inside would nest an engine borrow).
    pub fn commit_pending_render(&mut self, pane: u32, run_id: u64) -> Option<String> {
        // Clone the Rc out and let the map borrow end here — `borrow_mut`
        // below must not run while `self` (and thus the map) is still
        // borrowed, and nothing here needs `&mut self` beyond the lookup.
        let (owner, renderer) = self.pending_render.get(&pane)?;
        if *owner != run_id {
            return None;
        }
        let renderer = renderer.clone();
        let mut renderer = renderer.borrow_mut();
        renderer.commit()
    }

    /// Whether the host's probe deadline has nothing left to do for
    /// `run_id` in this pane: no run is pending here (finished, never
    /// started, or superseded by a different run), or its renderer already
    /// committed (via the row-count probe or a previous deadline).
    /// `commit_pending_render` returning `None` can't tell "already
    /// settled" apart from "still waiting for the first row" — this can, so
    /// a slow-starting source's deadline knows whether to keep rescheduling
    /// itself. A pane now owned by a different run also reads as settled:
    /// this run has nothing left to do here either way.
    pub fn probe_settled(&self, pane: u32, run_id: u64) -> bool {
        match self.pending_render.get(&pane) {
            None => true,
            Some((owner, renderer)) => *owner != run_id || renderer.borrow().is_committed(),
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

/// Routes diagnostics to a pane, styled.
///
/// The record arrives already sanitized — `Record` sanitizes on
/// construction, since diagnostics come from a TS command and are
/// therefore page-controlled. This sink's only job is styling, applied
/// around the already-clean text, so our colour survives and the
/// command's cannot be injected.
struct PaneSink<A: EngineAccess> {
    access: A,
    pane: u32,
}

impl<A: EngineAccess> crate::sink::Sink for PaneSink<A> {
    fn write(&self, record: crate::sink::Record) {
        const RED: &str = "\x1b[31m";
        let clean = record.text();
        // Every record is reset-prefixed, raw or not -- by `emit_output`,
        // which does it for every chunk of pane output rather than only for
        // records. The allowlist permits SGR on the argument that a
        // command's styling is bounded by a reset at the command boundary --
        // and that argument is false on the abort path: `Abortable::poll`
        // returns Ready(Err(Aborted)) *before* polling the inner future
        // (abort.rs:58), so a suspended command body never resumes and a
        // trailing reset never runs. A command that writes `\x1b[8m`
        // (conceal) and then hangs would otherwise leave every later
        // command's output invisible. Resetting per chunk makes cross-command
        // SGR bleed structurally impossible instead of dependent on cleanup
        // that cancellation can skip.
        //
        // A raw write otherwise owns its formatting: no appended newline (a
        // partial write must stay partial) and no colour wrapper (the command
        // may be emitting its own SGR, and re-wrapping per write would fight
        // it).
        let line = if record.is_raw() {
            clean.to_string()
        } else {
            match record.channel() {
                crate::sink::Channel::Log => format!("{clean}\n"),
                crate::sink::Channel::Err => format!("{RED}{clean}{RESET}\n"),
            }
        };
        self.access.with(|e| e.emit_output(self.pane, &line));
        // Flush after the borrow closes, never inside `with` — flushing may
        // invoke the JS callback, and no JS call may happen while an engine
        // borrow is held. Without this, a command's ctx.emit calls all sit
        // queued and invisible until the whole pipeline resolves.
        self.access.events_ready();
    }

    /// Hand control back to the driver between paints.
    ///
    /// Without this a fast producer paints as fast as it can produce,
    /// starving every other stage in the same `drive` pass and, in a browser,
    /// the frame loop with it. One yield per painted item bounds paint rate
    /// to the driver's pace, which is what keeps the tab responsive on a
    /// 100k-row source. `drive` re-polls promptly because the self-wake sets
    /// its nudge flag.
    fn ready(&self) -> crate::registry::LocalBoxFuture<()> {
        Box::pin(async {
            let mut yielded = false;
            std::future::poll_fn(move |cx| {
                if yielded {
                    std::task::Poll::Ready(())
                } else {
                    yielded = true;
                    cx.waker().wake_by_ref();
                    std::task::Poll::Pending
                }
            })
            .await
        })
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

/// The variables a pipeline in `pane` can see.
///
/// The only place the engine derives an evaluation scope, which is why one
/// merge here covers interactive panes, `run()`, positional arguments, flag
/// values, and `"…$name…"` interpolation. (A `Scope` is a plain map and is
/// constructed elsewhere — an empty one per session, a derived one per
/// closure call — but nothing else decides what a submitted line can see.)
///
/// Host underneath, session on top: a session's own value wins. That map is
/// populated by `set_session_var`, so the `extend` is what makes a
/// session-scoped value shadow a host one — and shell-level assignment will
/// land later without anyone rereading this function.
///
/// Returns an owned clone, so a pipeline holds the values as they were when
/// its line started. A `set_host_var` mid-run is invisible to it and takes
/// effect on the next line. That is deliberate: under live lookup a single
/// pipeline could bind one name to two values in two stages.
fn scope_for_pane<A: EngineAccess>(access: &A, pane: u32) -> Scope {
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

/// Rows to buffer before committing column widths. The host may commit
/// sooner on a deadline (a later task); this is the row-count bound.
const PROBE_ROWS: usize = 50;

/// Paints a pipeline's items to a pane as they arrive.
///
/// Records feed a `StreamRenderer` (probe → commit widths → stream rows), so
/// a long or live result shows its first rows without waiting for the end.
/// Anything else (scalars, `Rendered` help text) is buffered and rendered
/// once at `finish`, exactly as before — those have no incremental form.
struct ProgressiveConsumer<A: EngineAccess> {
    access: A,
    pane: u32,
    /// Tags this run's entry in `Engine::pending_render`, so `finish` only
    /// removes it if a newer run hasn't already replaced it for this pane.
    run_id: u64,
    sink: Rc<dyn crate::sink::Sink>,
    /// Kept alongside the renderer for the mixed-stream fallback below,
    /// which renders buffered non-record items at this same width.
    width: u16,
    /// Shared with `Engine::pending_render` for the run's lifetime, so the
    /// host's probe-deadline timer can force an early commit from outside
    /// this future.
    renderer: Rc<RefCell<crate::render::stream::StreamRenderer>>,
    /// Non-record items, rendered at finish via the normal path.
    buffered: Vec<PipelineData>,
    painted: bool,
}

impl<A: EngineAccess> ProgressiveConsumer<A> {
    fn new(access: A, pane: u32, run_id: u64, sink: Rc<dyn crate::sink::Sink>, width: u16) -> Self {
        let renderer = Rc::new(RefCell::new(crate::render::stream::StreamRenderer::new(
            width,
            PROBE_ROWS,
        )));
        access.with(|e| {
            e.pending_render.insert(pane, (run_id, renderer.clone()));
        });
        ProgressiveConsumer {
            access,
            pane,
            run_id,
            sink,
            width,
            renderer,
            buffered: Vec::new(),
            painted: false,
        }
    }

    /// Paint text to the pane. The borrow closes before `events_ready`,
    /// which may call into JS.
    fn paint(&self, text: &str) {
        self.access.with(|e| e.emit_output(self.pane, text));
        self.access.events_ready();
    }
}

impl<A: EngineAccess> crate::eval::FinalConsumer for ProgressiveConsumer<A> {
    fn item(&mut self, item: PipelineData) {
        match item {
            PipelineData::Value(Value::Record(map)) => {
                self.painted = true;
                if let Some(text) = self.renderer.borrow_mut().push(Value::Record(map)) {
                    self.paint(&text);
                }
            }
            other => self.buffered.push(other),
        }
    }

    fn needs_backpressure(&self) -> bool {
        true
    }

    fn ready(&self) -> crate::registry::LocalBoxFuture<()> {
        self.sink.ready()
    }

    fn finish(&mut self) -> PipelineData {
        // Deregister before doing anything else: the run is ending, so the
        // host's probe deadline (if it still fires) must no longer reach
        // this renderer. The run-id check is what actually protects a
        // second run in this pane (`commit_pending_render`/`probe_settled`
        // check it too); `ptr_eq` is belt-and-braces on top of that.
        self.access.with(|e| {
            if let Some((owner, current)) = e.pending_render.get(&self.pane) {
                if *owner == self.run_id && Rc::ptr_eq(current, &self.renderer) {
                    e.pending_render.remove(&self.pane);
                }
            }
        });
        if self.painted {
            let tail = self.renderer.borrow_mut().finish();
            if !tail.is_empty() {
                self.paint(&tail);
            }
            // Mixed stream: records painted progressively, but non-record
            // items also arrived (pathological, but never silently dropped).
            // Ordering relative to the painted rows is lost either way, so
            // just render them now via the normal one-shot path.
            if !self.buffered.is_empty() {
                let cols = self.width;
                let items = std::mem::take(&mut self.buffered);
                for data in items {
                    match data {
                        PipelineData::Value(v) => {
                            let rendered = render(&v, cols);
                            self.paint(&rendered);
                        }
                        PipelineData::Rendered(s) => self.paint(&format!("{s}\n")),
                        PipelineData::Empty => {}
                    }
                }
            }
            // Already painted; nothing left for the caller to render.
            return PipelineData::Empty;
        }
        // No records: fall back to the old collect-then-render path.
        let items = std::mem::take(&mut self.buffered);
        let all_rendered =
            !items.is_empty() && items.iter().all(|i| matches!(i, PipelineData::Rendered(_)));
        if all_rendered {
            let joined = items
                .into_iter()
                .map(|i| match i {
                    PipelineData::Rendered(s) => s,
                    _ => String::new(),
                })
                .collect::<Vec<_>>()
                .join("\n");
            return PipelineData::Rendered(joined);
        }
        match items.len() {
            0 => PipelineData::Empty,
            1 => items.into_iter().next().unwrap_or(PipelineData::Empty),
            _ => PipelineData::Value(Value::List(
                items.into_iter().map(PipelineData::into_value).collect(),
            )),
        }
    }
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

    let sink: Rc<dyn crate::sink::Sink> =
        Rc::new(PaneSink { access: access.clone(), pane });
    let sink_for_consumer = sink.clone();
    let ctx = make_ctx(&access, pane, run_id, sink);
    let cols = ctx.width;
    let scope = scope_for_pane(&access, pane);
    let source = EngineCommands(access.clone());

    let (results, error) = {
        let access2 = access.clone();
        let sink2 = sink_for_consumer;
        let mut make = || -> Box<dyn crate::eval::FinalConsumer> {
            Box::new(ProgressiveConsumer::new(access2.clone(), pane, run_id, sink2.clone(), cols))
        };
        eval_line_with(&parsed.line, &source, &ctx, &scope, &mut make).await
    };

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
/// or rendering anything. Commands' diagnostics (`ctx.log` / `ctx.err`) go
/// to the caller-supplied sink rather than the pane, which is what lets a
/// programmatic run avoid writing on the user's terminal.
pub async fn eval_to_value<A: EngineAccess>(
    access: A,
    pane: u32,
    line: String,
    run_id: u64,
    sink: Rc<dyn crate::sink::Sink>,
) -> Result<Value, ShellError> {
    let parsed = parse(&line);
    if let Some(err) = parsed.errors.into_iter().next() {
        return Err(err);
    }
    let ctx = make_ctx(&access, pane, run_id, sink);
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
    // Through `emit_output`, not `emit`: the prompt opens with `\r\x1b[K`,
    // and erase-to-end-of-line fills with the current background. A command
    // that leaked a background colour would otherwise paint the shell's own
    // prompt line with it.
    let prompt = e.prompt_line(pane);
    e.emit_output(pane, &prompt);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::block_on;
    use crate::registry::{Command, LocalBoxFuture, PipelineData};
    use crate::signature::{BoundCall, Signature};
    use crate::sink::{Record, Sink};
    use std::cell::{Cell, RefCell};

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
    fn every_pane_chunk_is_reset_prefixed_including_the_engine_s_own_paints() {
        // A command's SGR outlives it on the abort path, so the engine's own
        // paints are drawn in whatever the command left set. For the prompt
        // that is not merely cosmetic: `prompt_line` opens with `\r\x1b[K`,
        // and erase-to-end-of-line fills with the *current* background, so a
        // leaked background repaints the shell's own line. `emit_output`
        // prefixes every chunk, which covers rendered values, `Rendered`
        // text, error renders, progressive paints and the prompt alike --
        // one choke point rather than a reset per call site.
        let access = engine();
        let events = feed_and_run(&access, "echo hi\r");
        let chunks: Vec<&String> = events
            .iter()
            .filter_map(|ev| match ev {
                EngineEvent::PaneOutput { data, .. } => Some(data),
                _ => None,
            })
            .collect();
        assert!(!chunks.is_empty(), "the run painted nothing");
        for chunk in &chunks {
            assert!(chunk.starts_with("\x1b[0m"), "unreset pane chunk: {chunk:?}");
        }
        let last = chunks.last().map(|s| s.as_str()).unwrap_or_default();
        assert!(last.contains("❯"), "the prompt is the last chunk: {last:?}");
        assert!(
            last.starts_with("\x1b[0m\r\x1b[K"),
            "the reset must precede the erase, or `\\x1b[K` fills with a leaked background: {last:?}"
        );
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

    #[test]
    fn pane_sink_strips_escapes_from_diagnostics() {
        let access = engine();
        let pane = active_pane(&access);
        let sink = PaneSink { access: access.clone(), pane };

        sink.write(Record::log("\x1b[2Jcleared"));
        sink.write(Record::err("bad\nthing"));

        let out = output_text(&access.with(|e| e.drain_events()));
        // The command's own escape is gone...
        assert!(!out.contains("[2J"), "clear-screen survived: {out:?}");
        assert!(out.contains("cleared"));
        // ...the newline is collapsed, keeping one diagnostic on one line...
        assert!(out.contains("bad thing"));
        // ...and our styling is applied around the sanitized text.
        assert!(out.contains("\x1b[31m"), "err not styled: {out:?}");
    }

    /// An `EngineAccess` that delegates to a real engine but counts
    /// `events_ready` calls, so a flush-timing bug (queued but never
    /// flushed) is visible to a test. The plain `Rc<RefCell<Engine>>`
    /// impl's `events_ready` is a documented no-op and cannot catch this.
    #[derive(Clone)]
    struct CountingAccess {
        inner: Rc<RefCell<Engine>>,
        flushes: Rc<Cell<usize>>,
    }

    impl CountingAccess {
        fn new() -> Self {
            CountingAccess { inner: Rc::new(RefCell::new(Engine::new())), flushes: Rc::new(Cell::new(0)) }
        }
    }

    impl EngineAccess for CountingAccess {
        fn with<R>(&self, f: impl FnOnce(&mut Engine) -> R) -> R {
            f(&mut self.inner.borrow_mut())
        }

        fn events_ready(&self) {
            self.flushes.set(self.flushes.get() + 1);
        }
    }

    #[test]
    fn pane_sink_flushes_after_every_write() {
        let access = CountingAccess::new();
        let pane = access.with(|e| e.mux.active_pane());
        let sink = PaneSink { access: access.clone(), pane };

        sink.write(Record::log("one"));
        assert_eq!(access.flushes.get(), 1, "write should flush immediately");
        sink.write(Record::err("two"));
        assert_eq!(access.flushes.get(), 2, "each write should flush, not just the last");
    }

    #[test]
    fn the_pane_emits_a_raw_record_verbatim() {
        // A partial write must not gain a newline, and must not be wrapped
        // in the err channel's colour -- a progress bar owns its own line.
        let access = engine();
        let pane = active_pane(&access);
        let sink = PaneSink { access: access.clone(), pane };
        sink.write(crate::sink::Record::raw_log("50%\r"));
        let out = output_text(&access.with(|e| e.drain_events()));
        assert!(out.contains("50%\r"), "raw text altered: {out:?}");
        assert!(!out.contains("50%\r\n"), "newline appended: {out:?}");

        let access2 = engine();
        let pane2 = active_pane(&access2);
        let sink2 = PaneSink { access: access2.clone(), pane: pane2 };
        sink2.write(crate::sink::Record::raw_err("oops"));
        let out2 = output_text(&access2.with(|e| e.drain_events()));
        assert!(!out2.contains("\x1b[31m"), "raw err was colour-wrapped: {out2:?}");
    }

    #[test]
    fn every_pane_record_is_reset_prefixed_so_sgr_cannot_bleed() {
        // The allowlist permits SGR only because styling is bounded. It
        // cannot be bounded by cleanup at the command boundary: `Abortable`
        // returns Ready(Err(Aborted)) without resuming a suspended body
        // (abort.rs:58), so a command that writes conceal (`\x1b[8m`) and
        // then hangs would leave later output invisible forever. Prefixing
        // every record with a reset makes that unreachable.
        let access = engine();
        let pane = active_pane(&access);
        let sink = PaneSink { access: access.clone(), pane };
        sink.write(crate::sink::Record::raw_log("\x1b[8mhidden"));
        sink.write(crate::sink::Record::log("after"));
        let out = output_text(&access.with(|e| e.drain_events()));
        // The innocent second write starts from a clean slate.
        let after = out.find("after").expect("second write painted");
        let reset_before = out[..after].rfind("\x1b[0m").expect("reset precedes it");
        assert!(reset_before < after, "no reset before the following record");
    }

    /// Writes to both diagnostic channels so the wiring from `execute_line`
    /// through `make_ctx` into a running command can be asserted end to end.
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
            _input: crate::chan::Receiver,
            _output: crate::chan::Sender,
        ) -> LocalBoxFuture<Result<(), ShellError>> {
            ctx.sink.write(Record::log("tick"));
            ctx.sink.write(Record::err("careful"));
            crate::registry::ready(Ok(()))
        }
    }

    #[test]
    fn typed_command_diagnostics_reach_the_pane() {
        let access = engine();
        access.with(|e| e.registry.register_builtin(Rc::new(Noisy)));
        let out = output_text(&feed_and_run(&access, "noisy\r"));
        assert!(out.contains("tick"), "log missing from pane: {out:?}");
        assert!(out.contains("careful"), "err missing from pane: {out:?}");
        assert!(out.contains("\x1b[31m"), "err not styled: {out:?}");
    }

    /// Takes an engine borrow on every poll and yields in between.
    ///
    /// Updated for stage 3 (streaming). This fixture still runs as its own
    /// pipeline of three `Borrower`s — it does not gain a real dependency on
    /// `map`/`filter`/`grep`/`head` — but what changed is that the overlap
    /// it exercises is no longer purely synthetic. Those four are now
    /// `StreamingBuiltin`s that consume and emit items one at a time rather
    /// than draining their whole upstream via `stream::collect` first, so a
    /// downstream streaming stage's work genuinely runs concurrently with
    /// its still-producing upstream, through the very same `drive` loop and
    /// interleaving this fixture polls through. `EngineAccess::with`'s
    /// signature already makes a borrow escaping across an await a compile
    /// error rather than a runtime one, so the only way this test could
    /// fail is a genuine overlapping borrow reached through that
    /// interleaving — and that is now a real, reachable pattern elsewhere
    /// in the codebase, not a hypothetical one. That is what makes this
    /// test falsifiable: an overlapping borrow would panic here, not just
    /// fail an assertion.
    struct Borrower(Rc<RefCell<Engine>>);
    impl Command for Borrower {
        fn signature(&self) -> &Signature {
            static SIG: std::sync::OnceLock<Signature> = std::sync::OnceLock::new();
            SIG.get_or_init(|| Signature::build("borrower", "borrows the engine"))
        }
        fn run(
            &self,
            _ctx: ExecContext,
            _call: BoundCall,
            _input: crate::chan::Receiver,
            output: crate::chan::Sender,
        ) -> LocalBoxFuture<Result<(), ShellError>> {
            let access = self.0.clone();
            Box::pin(async move {
                let mut yielded = false;
                std::future::poll_fn(|cx| {
                    // A short borrow, released before the yield — the
                    // discipline the whole design rests on.
                    let sessions = access.with(|e| e.mux.sessions.len());
                    assert!(sessions > 0);
                    if yielded {
                        std::task::Poll::Ready(())
                    } else {
                        yielded = true;
                        cx.waker().wake_by_ref();
                        std::task::Poll::Pending
                    }
                })
                .await;
                let _ = crate::stream::flatten(PipelineData::Value(Value::Int(1)), &output).await;
                Ok(())
            })
        }
    }

    #[test]
    fn concurrent_stages_never_hold_overlapping_engine_borrows() {
        let access = engine();
        access.with(|e| {
            e.registry.register_builtin(Rc::new(Borrower(access.clone())));
        });
        // Three stages, each borrowing on every poll and yielding between.
        // An overlapping borrow panics rather than failing an assertion.
        let events = feed_and_run(&access, "borrower | borrower | borrower\r");
        let out = output_text(&events);
        assert!(out.contains('1'), "pipeline produced no output: {out:?}");
    }

    /// Emits `n` single-field records, then ends.
    struct Rows(usize);
    impl Command for Rows {
        fn signature(&self) -> &Signature {
            static SIG: std::sync::OnceLock<Signature> = std::sync::OnceLock::new();
            SIG.get_or_init(|| Signature::build("rows", "emit n records"))
        }
        fn run(
            &self,
            _ctx: ExecContext,
            _call: BoundCall,
            _input: crate::chan::Receiver,
            output: crate::chan::Sender,
        ) -> crate::registry::LocalBoxFuture<Result<(), ShellError>> {
            let n = self.0;
            Box::pin(async move {
                for i in 0..n {
                    let mut m = indexmap::IndexMap::new();
                    m.insert("id".to_string(), Value::Int(i as i64));
                    if output
                        .send(PipelineData::Value(Value::Record(m)))
                        .await
                        .is_err()
                    {
                        return Ok(());
                    }
                }
                Ok(())
            })
        }
    }

    #[test]
    fn a_finite_table_renders_the_same_through_the_progressive_path() {
        // The gate: a small result still paints a complete table -- header,
        // every row, bottom border -- exactly as the collect-once path did.
        let access = engine();
        access.with(|e| e.registry.register_builtin(Rc::new(Rows(3))));
        let out = output_text(&feed_and_run(&access, "rows\r"));
        assert!(out.contains("id"), "header: {out:?}");
        for expected in ['0', '1', '2'] {
            assert!(out.contains(expected), "row {expected} missing: {out:?}");
        }
    }

    /// Number of separate PaneOutput events — the observable difference
    /// between painting incrementally and emitting one block at the end.
    fn pane_output_count(events: &[EngineEvent]) -> usize {
        events
            .iter()
            .filter(|ev| matches!(ev, EngineEvent::PaneOutput { .. }))
            .count()
    }

    #[test]
    fn a_long_result_paints_incrementally_not_in_one_block() {
        // 60 rows against a 50-row probe: the probe commits (header + first
        // 50 rows), then each remaining row paints on its own. A collect-once
        // implementation would emit a single output event instead, so the
        // count is what distinguishes them -- asserting only that the header
        // precedes the last row would pass either way.
        let access = engine();
        access.with(|e| e.registry.register_builtin(Rc::new(Rows(60))));
        let events = feed_and_run(&access, "rows\r");
        let out = output_text(&events);

        assert!(out.contains("id"), "header painted: {out:?}");
        assert!(out.contains("59"), "last row painted: {out:?}");

        // Prompt/echo also emit PaneOutput, so compare against the finite
        // case rather than a magic number: 60 rows must produce many more
        // output events than a 3-row result does.
        let small = engine();
        small.with(|e| e.registry.register_builtin(Rc::new(Rows(3))));
        let small_events = feed_and_run(&small, "rows\r");

        let many = pane_output_count(&events);
        let few = pane_output_count(&small_events);
        assert!(
            many > few + 5,
            "expected incremental paints ({many}) to far exceed the finite case ({few})"
        );
    }

    #[test]
    fn a_host_deadline_commits_a_partial_probe() {
        // A slow source: fewer rows than PROBE_ROWS have arrived, so nothing
        // has painted. Driving a live pipeline to exactly this in-flight
        // state fights the `block_on`/`drive` scheduling model directly
        // (Part A's throttle processes at most one row per `drive` pass, so
        // there is no clean hook to pause "after N rows, before the probe
        // fills"). Instead this registers a renderer exactly as
        // `ProgressiveConsumer::new` does, then calls
        // `commit_pending_render` the way the host's `setTimeout` would --
        // proving the engine's half of the mechanism without re-implementing
        // the scheduler. The wasm half (arming the real timer) has no native
        // test; the browser proof covers it end to end.
        let access = engine();
        let pane = active_pane(&access);
        let run_id = 1;

        let renderer = Rc::new(RefCell::new(crate::render::stream::StreamRenderer::new(
            80,
            PROBE_ROWS,
        )));
        let mut m = indexmap::IndexMap::new();
        m.insert("id".to_string(), Value::Int(7));
        assert_eq!(
            renderer.borrow_mut().push(Value::Record(m)),
            None,
            "still probing: nothing painted yet"
        );
        access.with(|e| {
            e.pending_render.insert(pane, (run_id, renderer.clone()));
        });

        let forced = access
            .with(|e| e.commit_pending_render(pane, run_id))
            .expect("the deadline forces a commit of the buffered row");
        assert!(forced.contains("id"), "header painted: {forced:?}");
        assert!(forced.contains('7'), "buffered row painted: {forced:?}");

        // Already committed: a second deadline (or the run's own
        // end-of-stream commit) is a safe no-op, not a repainted header.
        assert_eq!(access.with(|e| e.commit_pending_render(pane, run_id)), None);

        // A pane with nothing pending -- no run ever started, or its run
        // already finished and deregistered -- is also a no-op.
        assert_eq!(access.with(|e| e.commit_pending_render(pane + 1, run_id)), None);
    }

    #[test]
    fn a_second_runs_deadline_does_not_hijack_the_first() {
        // A user can submit a second line into the same pane while the
        // first is still streaming -- nothing in `feed`/`spawn_pipeline`
        // stops it. `ProgressiveConsumer::new` would then overwrite
        // `pending_render[pane]` with the newer run's renderer. Without the
        // run-id check, the *older* run's still-armed timer would reach
        // through that shared pane key and force a commit on the *newer*
        // run's renderer -- silently abandoning its own, and (if the newer
        // run settles first) making `probe_settled` report "done" for a run
        // that never painted.
        let access = engine();
        let pane = active_pane(&access);
        let (run_a, run_b) = (1, 2);

        let renderer_a = Rc::new(RefCell::new(crate::render::stream::StreamRenderer::new(
            80,
            PROBE_ROWS,
        )));
        let mut m = indexmap::IndexMap::new();
        m.insert("id".to_string(), Value::Int(1));
        renderer_a.borrow_mut().push(Value::Record(m));
        access.with(|e| {
            e.pending_render.insert(pane, (run_a, renderer_a.clone()));
        });

        // Run B starts in the same pane before A's deadline fires, and
        // overwrites the map entry exactly as `ProgressiveConsumer::new`
        // does.
        let renderer_b = Rc::new(RefCell::new(crate::render::stream::StreamRenderer::new(
            80,
            PROBE_ROWS,
        )));
        let mut m = indexmap::IndexMap::new();
        m.insert("id".to_string(), Value::Int(2));
        renderer_b.borrow_mut().push(Value::Record(m));
        access.with(|e| {
            e.pending_render.insert(pane, (run_b, renderer_b.clone()));
        });

        // A's deadline fires: it must not touch B's renderer.
        assert_eq!(
            access.with(|e| e.commit_pending_render(pane, run_a)),
            None,
            "a superseded run's deadline must not drive the newer run's renderer"
        );
        assert!(
            !renderer_b.borrow().is_committed(),
            "A's deadline must not have force-committed B's renderer"
        );
        assert!(
            access.with(|e| e.probe_settled(pane, run_a)),
            "a superseded run reads as settled -- it has nothing left to do here"
        );

        // B's own deadline still works normally.
        let forced = access
            .with(|e| e.commit_pending_render(pane, run_b))
            .expect("B's own deadline still commits B's buffered row");
        // Check the coloured numeric cell specifically, not a bare digit --
        // the header's bold escape (`\x1b[1m`) itself contains a literal
        // '1' and would make a plain `contains('1')` a false positive.
        assert!(forced.contains("\x1b[36m2"), "B's row painted: {forced:?}");
        assert!(!forced.contains("\x1b[36m1"), "A's row must not appear: {forced:?}");
    }

    // --- host variables ---

    /// Evaluate a line the way the programmatic `run()` API does, so a test
    /// can assert on the resulting `Value` rather than on painted text.
    /// Diagnostics go to a throwaway sink; only the final value is of
    /// interest here.
    fn run_line(access: &Rc<RefCell<Engine>>, line: &str) -> Result<Value, ShellError> {
        let pane = active_pane(access);
        let sink: Rc<dyn Sink> = Rc::new(crate::sink::CollectingSink::default());
        block_on(eval_to_value(access.clone(), pane, line.to_string(), 0, sink))
    }

    #[test]
    fn a_host_variable_resolves_in_a_command() {
        let access = engine();
        access.with(|e| {
            e.set_host_var("greeting", Value::Str("hello".into())).expect("valid name");
        });
        let out = run_line(&access, "echo $greeting").expect("resolves");
        assert_eq!(out, Value::Str("hello".into()));
    }

    #[test]
    fn a_session_variable_overrides_a_host_variable() {
        let access = engine();
        access.with(|e| {
            e.set_host_var("x", Value::Str("host".into())).expect("valid name");
            let sid = e.mux.active_session;
            if let Some(s) = e.mux.sessions.get_mut(&sid) {
                s.vars.insert("x".into(), Value::Str("session".into()));
            }
        });
        assert_eq!(
            run_line(&access, "echo $x").expect("resolves"),
            Value::Str("session".into())
        );
    }

    #[test]
    fn setting_a_host_variable_again_replaces_it() {
        let access = engine();
        access.with(|e| {
            e.set_host_var("x", Value::Int(1)).expect("valid");
            e.set_host_var("x", Value::Int(2)).expect("valid");
        });
        assert_eq!(run_line(&access, "echo $x").expect("resolves"), Value::Int(2));
    }

    #[test]
    fn unsetting_restores_the_unknown_variable_error() {
        let access = engine();
        access.with(|e| {
            e.set_host_var("x", Value::Int(1)).expect("valid");
            assert!(e.unset_host_var("x"), "was set, so removal reports true");
            assert!(!e.unset_host_var("x"), "already gone, so a second removal reports false");
        });
        let err = run_line(&access, "echo $x").expect_err("no longer set");
        assert!(err.msg.contains("unknown variable `$x`"), "{}", err.msg);
    }

    #[test]
    fn an_invalid_name_is_rejected_rather_than_stored() {
        let access = engine();
        access.with(|e| {
            let err = e.set_host_var("a b", Value::Int(1)).expect_err("space is not a var char");
            assert!(err.msg.contains("a b"), "the error should name the offender: {}", err.msg);
            assert!(e.host_vars().is_empty(), "nothing should have been stored");
            assert_eq!(e.host_var("a b"), None);

            // Names the lexer accepts that an ASCII-only rule would not: this
            // is what pins the reuse of `is_valid_var_name` rather than a
            // hand-written pattern. `is_var_char` is Unicode-aware and
            // imposes no leading-character rule.
            e.set_host_var("café", Value::Int(7)).expect("unicode letters are var chars");
            e.set_host_var("1", Value::Int(8)).expect("a leading digit is legal");
        });
        // Accepted *and* referenceable: the rule is only right if the lexer
        // agrees, so assert through evaluation rather than on the validator's
        // return value alone.
        assert_eq!(run_line(&access, "echo $café").expect("resolves"), Value::Int(7));
        assert_eq!(run_line(&access, "echo $1").expect("resolves"), Value::Int(8));
    }

    #[test]
    fn interpolation_sees_host_variables() {
        let access = engine();
        access.with(|e| {
            e.set_host_var("name", Value::Str("world".into())).expect("valid");
        });
        assert_eq!(
            run_line(&access, r#"echo "hello-$name""#).expect("resolves"),
            Value::Str("hello-world".into())
        );
    }

    /// Sets `$x` to 2 when it runs, so a line can mutate a host variable
    /// partway through and the rest of that line can be checked against it.
    struct Bump(Rc<RefCell<Engine>>);
    impl Command for Bump {
        fn signature(&self) -> &Signature {
            static SIG: std::sync::OnceLock<Signature> = std::sync::OnceLock::new();
            SIG.get_or_init(|| Signature::build("bump", "sets the host variable x to 2"))
        }
        fn run(
            &self,
            _ctx: ExecContext,
            _call: BoundCall,
            _input: crate::chan::Receiver,
            _output: crate::chan::Sender,
        ) -> LocalBoxFuture<Result<(), ShellError>> {
            // A short synchronous borrow, released before this returns: the
            // future below never holds one across an await.
            let result = self
                .0
                .with(|e| e.set_host_var("x", Value::Int(2)));
            crate::registry::ready(result)
        }
    }

    #[test]
    fn a_pipeline_keeps_the_values_its_line_started_with() {
        // The teeth of the freshness guarantee: `bump` really does change the
        // engine's host variable mid-line, and `echo $x` -- a later pipeline
        // of the *same* line -- must still report what the line started with.
        // Re-deriving the scope per pipeline (live lookup) makes this say 2.
        let access = engine();
        access.with(|e| {
            e.registry.register_builtin(Rc::new(Bump(access.clone())));
            e.set_host_var("x", Value::Int(1)).expect("valid");
        });
        assert_eq!(
            run_line(&access, "bump; echo $x").expect("resolves"),
            Value::Int(1),
            "a line sees the values it started with, not a mid-line write"
        );
        // The write did land -- otherwise the assertion above would pass for
        // the wrong reason.
        access.with(|e| assert_eq!(e.host_var("x"), Some(&Value::Int(2)), "bump ran"));
        // And the next line picks it up.
        assert_eq!(run_line(&access, "echo $x").expect("resolves"), Value::Int(2));
    }

    #[test]
    fn scope_for_pane_returns_an_owned_snapshot() {
        // Narrower than the test above: it only shows that host variables
        // reach the derived scope and that the scope is owned, so a later
        // write cannot reach back into one already taken. It says nothing
        // about when evaluation takes it -- that is the mid-line test's job.
        let access = engine();
        let pane = active_pane(&access);
        access.with(|e| {
            e.set_host_var("x", Value::Int(1)).expect("valid");
        });
        let taken = scope_for_pane(&access, pane);
        access.with(|e| {
            e.set_host_var("x", Value::Int(2)).expect("valid");
        });
        assert_eq!(
            taken.get("x"),
            Some(&Value::Int(1)),
            "an already-taken scope must not see a later write"
        );
        assert_eq!(
            scope_for_pane(&access, pane).get("x"),
            Some(&Value::Int(2)),
            "but the next line does"
        );
    }

    // --- session variables ---

    #[test]
    fn a_session_variable_resolves_in_that_session() {
        let access = engine();
        let sid = access.with(|e| e.mux.active_session);
        access.with(|e| {
            e.set_session_var(sid, "scratch", Value::Str("mine".into())).expect("valid");
        });
        assert_eq!(
            run_line(&access, "echo $scratch").expect("resolves"),
            Value::Str("mine".into())
        );
    }

    #[test]
    fn a_session_variable_shadows_the_host_one_without_destroying_it() {
        let access = engine();
        let sid = access.with(|e| e.mux.active_session);
        access.with(|e| {
            e.set_host_var("x", Value::Str("host".into())).expect("valid");
            e.set_session_var(sid, "x", Value::Str("session".into())).expect("valid");
        });
        assert_eq!(
            run_line(&access, "echo $x").expect("resolves"),
            Value::Str("session".into())
        );
        // The host layer is untouched — shadowing hides a value, it does not
        // overwrite one. Reading the layer directly is how a host tells the
        // difference, since a read never merges.
        access.with(|e| {
            assert_eq!(e.host_var("x"), Some(&Value::Str("host".into())));
            assert_eq!(
                e.session_var(sid, "x").expect("live session"),
                Some(&Value::Str("session".into()))
            );
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
        assert_eq!(
            run_line(&access, "echo $x").expect("resolves"),
            Value::Str("host".into())
        );
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

    #[test]
    fn an_invalid_session_variable_name_is_rejected_by_the_lexer_rule() {
        let access = engine();
        let sid = access.with(|e| e.mux.active_session);
        access.with(|e| {
            assert!(e.set_session_var(sid, "a b", Value::Int(1)).is_err());
            // Names only the real rule allows, so an ASCII-only pattern
            // creeping in here is caught. `is_alphanumeric` is Unicode-aware
            // and there is no leading-character restriction.
            e.set_session_var(sid, "café", Value::Int(7))
                .expect("unicode letters are var chars");
            e.set_session_var(sid, "1", Value::Int(8)).expect("a leading digit is legal");
        });
        assert_eq!(run_line(&access, "echo $café").expect("resolves"), Value::Int(7));
        assert_eq!(run_line(&access, "echo $1").expect("resolves"), Value::Int(8));
    }
}

