//! Core engine for browser-terminal: shell language, structured values,
//! pipeline evaluation, line editor, renderer, and multiplexer state.
//!
//! This crate has zero wasm dependencies and is tested natively.

pub mod abort;
pub mod ast;
pub mod builtins;
pub mod callable;
pub mod editor;
pub mod engine;
pub mod error;
pub mod eval;
pub mod lex;
pub mod matcher;
pub mod mux;
pub mod parse;
pub mod protocol;
pub mod registry;
pub mod render;
pub mod signature;
pub mod value;

pub use error::{ErrorKind, ShellError, Span};
pub use registry::{Command, CommandRegistry, ExecContext, HostHooks, PipelineData};
pub use signature::{BoundCall, Scope, Shape, Signature};
pub use value::Value;
