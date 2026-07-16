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

    pub fn unknown_command_error(&self, word: &str, span: crate::error::Span) -> ShellError {
        let e = ShellError::new(ErrorKind::UnknownCommand, format!("unknown command `{word}`")).with_span(span);
        match did_you_mean(word, self.map.keys().map(|s| s.as_str())) {
            Some(s) => e.with_help(format!("did you mean `{s}`?")),
            None => e.with_help("run `help` to list commands"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegisterOutcome {
    Added,
    /// An earlier external registration with the same name was replaced.
    Replaced,
}
