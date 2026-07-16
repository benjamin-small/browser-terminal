//! The engine: panes (a full mux tree in M5), the command registry, session
//! scope, and the outgoing event queue.
//!
//! Concurrency contract (enforced structurally in the wasm layer): all
//! engine access goes through `EngineAccess::with`, a synchronous closure —
//! no borrow can cross an await. Code holding the engine never invokes the
//! host callback; events are queued and flushed after the closure returns.
//! `execute_line` is the one shared async path, used verbatim by native
//! protocol tests and the browser.

use crate::editor::{Effects, LineEditor};
use crate::error::ShellError;
use crate::eval::{eval_line, CommandSource};
use crate::parse::parse;
use crate::protocol::EngineEvent;
use crate::registry::{Command, CommandRegistry, ExecContext, HostHooks, PipelineData};
use crate::render::render;
use crate::signature::Scope;
use std::collections::VecDeque;
use std::rc::Rc;

use indexmap::IndexMap;

pub struct PaneShell {
    pub editor: LineEditor,
    pub cols: u16,
    pub rows: u16,
    /// A pipeline task is in flight. (The abort handle lives host-side.)
    pub running: bool,
}

pub struct Engine {
    pub registry: CommandRegistry,
    pub scope: Scope,
    panes: IndexMap<u32, PaneShell>,
    next_pane_id: u32,
    events: VecDeque<EngineEvent>,
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
            scope: Scope::new(),
            panes: IndexMap::new(),
            next_pane_id: 0,
            events: VecDeque::new(),
        }
    }

    pub fn create_pane(&mut self, cols: u16, rows: u16) -> u32 {
        let id = self.next_pane_id;
        self.next_pane_id += 1;
        self.panes.insert(
            id,
            PaneShell { editor: LineEditor::new(), cols, rows, running: false },
        );
        id
    }

    pub fn pane(&self, id: u32) -> Option<&PaneShell> {
        self.panes.get(&id)
    }

    pub fn pane_mut(&mut self, id: u32) -> Option<&mut PaneShell> {
        self.panes.get_mut(&id)
    }

    /// Sync input hot path: feed raw input to the pane's editor.
    pub fn feed(&mut self, pane: u32, data: &str) -> Effects {
        match self.panes.get_mut(&pane) {
            Some(p) => p.editor.feed(data),
            None => Effects::default(),
        }
    }

    pub fn resize(&mut self, pane: u32, cols: u16, rows: u16) {
        if let Some(p) = self.panes.get_mut(&pane) {
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

    pub fn has_events(&self) -> bool {
        !self.events.is_empty()
    }

    /// Fresh prompt line for a pane (used after output settles).
    pub fn prompt_line(&self, pane: u32) -> String {
        self.panes
            .get(&pane)
            .map(|p| p.editor.prompt_line())
            .unwrap_or_default()
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

    fn unknown_command_error(&self, word: &str, span: crate::error::Span) -> ShellError {
        self.0.with(|e| e.registry.unknown_command_error(word, span))
    }
}

/// Host hooks for commands running inside a pane.
struct EngineHost<A: EngineAccess> {
    access: A,
    pane: u32,
}

impl<A: EngineAccess> HostHooks for EngineHost<A> {
    fn emit_line(&self, line: &str) {
        self.access.with(|e| e.emit_output(self.pane, &format!("{line}\n")));
        self.access.events_ready();
    }

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
}

/// Evaluate one submitted line in a pane: parse → eval → render → prompt.
/// The single shared execution path for native tests and the browser.
pub async fn execute_line<A: EngineAccess>(access: A, pane: u32, line: String) {
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

    let (cols, scope) = access.with(|e| {
        (
            e.pane(pane).map(|p| p.cols).unwrap_or(80),
            e.scope.clone(),
        )
    });
    let ctx = ExecContext {
        host: Rc::new(EngineHost { access: access.clone(), pane }),
        width: cols,
    };
    let source = EngineCommands(access.clone());

    let result = eval_line(&parsed.line, &source, &ctx, &scope).await;

    access.with(|e| {
        match &result {
            Ok(results) => {
                for data in results {
                    if let PipelineData::Value(v) = data {
                        let rendered = render(v, cols);
                        e.emit_output(pane, &rendered);
                    }
                }
                finish_pane(e, pane, true);
            }
            Err(err) => {
                e.emit_output(pane, &err.render(&line));
                finish_pane(e, pane, false);
            }
        }
    });
    access.events_ready();
}

/// Mark the pane idle, color the prompt by status, and print it.
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
        let mut e = Engine::new();
        e.create_pane(80, 24);
        Rc::new(RefCell::new(e))
    }

    fn feed_and_run(access: &Rc<RefCell<Engine>>, input: &str) -> Vec<EngineEvent> {
        let fx = access.with(|e| e.feed(0, input));
        for line in fx.submitted {
            block_on(execute_line(access.clone(), 0, line));
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
        let events = feed_and_run(&access, "echo (\r");
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

    #[test]
    fn emit_line_interleaves_before_final_render() {
        // `clear` uses request_clear → an event mid-execution.
        let access = engine();
        let events = feed_and_run(&access, "clear\r");
        let out = output_text(&events);
        assert!(out.contains("\x1b[2J"));
    }

    #[test]
    fn events_never_emitted_while_borrowed() {
        // EngineAccess::with is a sync closure; this test asserts the
        // execute path completes without RefCell double-borrow panics even
        // when commands touch host hooks that re-enter the engine.
        let access = engine();
        let events = feed_and_run(&access, "help | first 3\r");
        assert!(!events.is_empty());
    }
}
