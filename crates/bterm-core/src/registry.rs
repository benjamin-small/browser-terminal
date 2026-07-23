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
    /// Text this shell formatted itself — help output, `table` — which the
    /// final renderer prints verbatim.
    ///
    /// Untrusted strings get their escape sequences stripped on the way to
    /// the terminal (a page-controlled value must not be able to clear the
    /// screen). Our own styled output would be destroyed by that same pass,
    /// so it is tagged here instead of being smuggled through as a `Str`.
    /// It still behaves as a string when piped onward.
    Rendered(String),
}

impl PipelineData {
    pub fn into_value(self) -> Value {
        match self {
            PipelineData::Empty => Value::Null,
            PipelineData::Value(v) => v,
            PipelineData::Rendered(s) => Value::Str(s),
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
    /// Where `log` and `err` records go. Swappable so a programmatic
    /// `run()` can capture what a pane would have printed.
    pub sink: Rc<dyn crate::sink::Sink>,
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

    /// Registered commands whose name starts with `prefix` plus a space,
    /// as (full name, summary), sorted. Empty when `prefix` names no group.
    pub fn subcommands(&self, prefix: &str) -> Vec<(String, String)> {
        let head = format!("{prefix} ");
        let mut subs: Vec<(String, String)> = self
            .map
            .iter()
            .filter(|(name, _)| name.starts_with(&head))
            .map(|(name, e)| (name.clone(), e.command.signature().summary.clone()))
            .collect();
        subs.sort();
        subs
    }

    /// Rendered help when `words` names a group rather than a command —
    /// what `task`, `mux --help`, and `mux window` print.
    ///
    /// Requires the *whole* head to be the group: `task nope` is a typo to
    /// diagnose, not a reason to dump the group page. Only consulted after
    /// `lookup` fails, so a group that later gains a command of its own name
    /// keeps working — the command wins.
    pub fn group_help(&self, words: &[String]) -> Option<String> {
        let prefix = words.join(" ");
        let subs = self.subcommands(&prefix);
        (!subs.is_empty()).then(|| crate::signature::render_group_help(&prefix, &subs))
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
        // A known group with an unknown word after it is a wrong-subcommand
        // mistake. Diagnosing it against its siblings beats the global
        // did-you-mean, which used to answer `task lst` with something from
        // the other end of the registry.
        if words.len() >= 2 {
            for take in (1..words.len()).rev() {
                let group: Vec<&str> = words[..take].iter().map(|w| w.node.as_str()).collect();
                let group = group.join(" ");
                let subs = self.subcommands(&group);
                if subs.is_empty() {
                    continue;
                }
                let attempted = &words[take];
                let err = ShellError::new(
                    ErrorKind::UnknownCommand,
                    format!("`{group}` has no subcommand `{}`", attempted.node),
                )
                .with_span(words[0].span.merge(attempted.span));
                // Suggest by the subcommand's own last word, not its full name.
                let tails: Vec<&str> = subs
                    .iter()
                    .map(|(n, _)| n[group.len() + 1..].split(' ').next().unwrap_or(""))
                    .collect();
                return match did_you_mean(&attempted.node, tails.iter().copied()) {
                    Some(s) => err.with_help(format!("did you mean `{group} {s}`?")),
                    None => err.with_help(format!("run `{group}` to list its subcommands")),
                };
            }
        }

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
