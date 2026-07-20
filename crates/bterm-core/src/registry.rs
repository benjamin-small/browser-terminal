//! Command trait, execution context, and the command registry.

use crate::error::{did_you_mean, ErrorKind, ShellError};
use crate::signature::{BoundCall, Signature};
use crate::value::Value;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;

/// Hand-rolled alias; we don't pull in the futures crate.
pub type LocalBoxFuture<T> = Pin<Box<dyn Future<Output = T>>>;

/// Wrap a ready result as a command future (for sync builtins).
pub fn ready<T: 'static>(t: T) -> LocalBoxFuture<T> {
    Box::pin(std::future::ready(t))
}

/// What flows between pipeline stages. `Stream` is a documented v2 seam —
/// adding it is an additive variant, not a trait redesign.
#[derive(Clone, Debug, PartialEq)]
pub enum PipelineData {
    Empty,
    Value(Value),
}

impl PipelineData {
    pub fn into_value(self) -> Value {
        match self {
            PipelineData::Empty => Value::Null,
            PipelineData::Value(v) => v,
        }
    }
}

/// Multiplexer mutations requested by `mux …` / `session …` builtins. The
/// engine host applies them; hosts without a multiplexer (the CLI) reject
/// them.
#[derive(Clone, Debug, PartialEq)]
pub enum MuxAction {
    SplitRight,
    SplitDown,
    WindowNew,
    WindowNext,
    WindowPrev,
    KillPane,
    Focus(String),
    Zoom,
    Hide,
    SessionNew { name: Option<String> },
    SessionList,
    SessionSwitch { name: String },
    SessionNext,
    SessionPrev,
}

/// Host services a command may touch. Implemented by the CLI (stdout,
/// readline history) and by the wasm engine (pane output, pane history).
pub trait HostHooks {
    /// Progressive output: print a line above the pipeline's final render.
    fn emit_line(&self, line: &str);
    /// The current shell's history, newest last.
    fn history(&self) -> Vec<String> {
        Vec::new()
    }
    /// `clear` builtin: wipe the screen.
    fn request_clear(&self) {}
    /// `help` with no args: (name, summary) for every registered command.
    fn help_overview(&self) -> Vec<(String, String)> {
        Vec::new()
    }
    /// `help <name>`: rendered help text for a command, if it exists.
    fn help_for(&self, _name: &str) -> Option<String> {
        None
    }
    /// Apply a multiplexer action (`mux split`, `session new`, …).
    fn mux_action(&self, _action: MuxAction) -> Result<Value, ShellError> {
        Err(ShellError::runtime(
            "the multiplexer is not available in this host",
        ))
    }
    /// Compile a `grep` pattern. Hosts with a real regex engine (the browser,
    /// via JS `RegExp`) override this; the default is substring matching.
    fn compile_pattern(
        &self,
        pattern: &str,
        case_insensitive: bool,
    ) -> Result<Box<dyn crate::matcher::Pattern>, String> {
        use crate::matcher::PatternMatcher as _;
        crate::matcher::SubstringMatcher.compile(pattern, case_insensitive)
    }
    /// `regex` or `substring` — used in help and error text.
    fn pattern_dialect(&self) -> &'static str {
        "substring"
    }
    /// Compile inline callable source (`'(o) => o.id'`). The browser
    /// compiles JavaScript; native hosts have no engine.
    fn compile_fn(&self, _source: &str) -> Result<Rc<dyn crate::callable::HostFn>, String> {
        Err("inline functions need a JavaScript host; this shell is running natively".into())
    }
    /// Look up a function the host registered by name (`@byId`).
    fn lookup_fn(&self, name: &str) -> Result<Rc<dyn crate::callable::HostFn>, String> {
        Err(format!(
            "no registered function `{name}`; this host cannot register functions"
        ))
    }
}

/// Per-invocation context handed to every command.
#[derive(Clone)]
pub struct ExecContext {
    pub host: Rc<dyn HostHooks>,
    /// Render width (pane cols / terminal width).
    pub width: u16,
    /// Pane the pipeline is running in (0 for the CLI).
    pub pane: u32,
    /// Unique id of this pipeline run; the wasm layer keys AbortControllers
    /// by it so TS commands receive the right AbortSignal.
    pub run_id: u64,
}

pub trait Command {
    fn signature(&self) -> &Signature;
    fn run(
        &self,
        ctx: ExecContext,
        call: BoundCall,
        input: PipelineData,
    ) -> LocalBoxFuture<Result<PipelineData, ShellError>>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CmdOrigin {
    Builtin,
    External,
}

struct Entry {
    command: Rc<dyn Command>,
    origin: CmdOrigin,
}

/// Keyed by full (possibly multi-word) name, e.g. `"str upcase"`. Lookup
/// tries the longest bareword prefix, so multi-word names are the subcommand
/// mechanism.
#[derive(Default)]
pub struct CommandRegistry {
    map: HashMap<String, Entry>,
}

impl CommandRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register_builtin(&mut self, command: Rc<dyn Command>) {
        let name = command.signature().name.clone();
        debug_assert!(!self.map.contains_key(&name), "duplicate builtin `{name}`");
        self.map.insert(name, Entry { command, origin: CmdOrigin::Builtin });
    }

    /// Register an external (TS) command. Errors if the name is owned by a
    /// builtin. Replacing a previous external registration is allowed (the
    /// deliberate HMR behavior) and reported via `Replaced`.
    pub fn register_external(&mut self, command: Rc<dyn Command>) -> Result<RegisterOutcome, ShellError> {
        let name = command.signature().name.clone();
        match self.map.get(&name) {
            Some(e) if e.origin == CmdOrigin::Builtin => Err(ShellError::new(
                ErrorKind::Runtime,
                format!("cannot register `{name}`: it is a built-in command"),
            )),
            Some(_) => {
                self.map.insert(name, Entry { command, origin: CmdOrigin::External });
                Ok(RegisterOutcome::Replaced)
            }
            None => {
                self.map.insert(name, Entry { command, origin: CmdOrigin::External });
                Ok(RegisterOutcome::Added)
            }
        }
    }

    /// Remove an external command. Builtins are not removable.
    pub fn unregister_external(&mut self, name: &str) -> bool {
        match self.map.get(name) {
            Some(e) if e.origin == CmdOrigin::External => {
                self.map.remove(name);
                true
            }
            _ => false,
        }
    }

    /// Longest-prefix lookup over the call's leading barewords. Returns the
    /// command and how many words its name consumed.
    pub fn lookup(&self, words: &[String]) -> Option<(Rc<dyn Command>, usize)> {
        for take in (1..=words.len()).rev() {
            let key = words[..take].join(" ");
            if let Some(entry) = self.map.get(&key) {
                return Some((entry.command.clone(), take));
            }
        }
        None
    }

    pub fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.map.keys().cloned().collect();
        names.sort();
        names
    }

    pub fn get(&self, name: &str) -> Option<Rc<dyn Command>> {
        self.map.get(name).map(|e| e.command.clone())
    }

    /// Unknown-command diagnostic. Tries did-you-mean against multi-word
    /// prefixes first so `str upcsae` suggests `str upcase`, not nothing.
    pub fn unknown_command_error(&self, words: &[crate::ast::Spanned<String>]) -> ShellError {
        let max_k = words.len().min(3);
        for take in (1..=max_k).rev() {
            let target = words[..take]
                .iter()
                .map(|w| w.node.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            if let Some(suggestion) = did_you_mean(&target, self.map.keys().map(|s| s.as_str())) {
                let span = words[0].span.merge(words[take - 1].span);
                return ShellError::new(
                    ErrorKind::UnknownCommand,
                    format!("unknown command `{target}`"),
                )
                .with_span(span)
                .with_help(format!("did you mean `{suggestion}`?"));
            }
        }
        let word = &words[0];
        ShellError::new(ErrorKind::UnknownCommand, format!("unknown command `{}`", word.node))
            .with_span(word.span)
            .with_help("run `help` to list commands")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegisterOutcome {
    Added,
    /// An earlier external registration with the same name was replaced.
    Replaced,
}
