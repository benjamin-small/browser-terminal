//! Native REPL over bterm-core — the everyday dev harness for language and
//! engine work. Runs the exact same parse → eval → render path the browser
//! pane uses.

use bterm_core::builtins;
use bterm_core::eval::{block_on, eval_line};
use bterm_core::registry::{CommandRegistry, ExecContext, HostHooks, PipelineData};
use bterm_core::render::render;
use bterm_core::signature::Scope;
use rustyline::error::ReadlineError;
use rustyline::DefaultEditor;
use std::cell::RefCell;
use std::rc::Rc;

struct CliHost {
    registry: Rc<RefCell<CommandRegistry>>,
    history: RefCell<Vec<String>>,
}

impl HostHooks for CliHost {
    fn history(&self) -> Vec<String> {
        self.history.borrow().clone()
    }

    fn request_clear(&self) {
        print!("\x1b[2J\x1b[H");
    }

    fn help_overview(&self) -> Vec<(String, String)> {
        let registry = self.registry.borrow();
        registry
            .names()
            .into_iter()
            .filter_map(|name| {
                registry
                    .get(&name)
                    .map(|cmd| (name, cmd.signature().summary.clone()))
            })
            .collect()
    }

    fn help_for(&self, name: &str) -> Option<String> {
        self.registry
            .borrow()
            .get(name)
            .map(|cmd| cmd.signature().render_help())
    }
}

/// The native harness maps the channels onto real file descriptors, so
/// `bterm 2>/dev/null` behaves the way a shell user expects.
struct CliSink;

impl bterm_core::sink::Sink for CliSink {
    fn write(&self, record: bterm_core::sink::Record) {
        match record.channel() {
            bterm_core::sink::Channel::Log => println!("{}", record.text()),
            bterm_core::sink::Channel::Err => eprintln!("{}", record.text()),
        }
    }
}

fn terminal_width() -> u16 {
    std::env::var("COLUMNS")
        .ok()
        .and_then(|c| c.parse().ok())
        .unwrap_or(100)
}

fn main() {
    let registry = Rc::new(RefCell::new(CommandRegistry::new()));
    builtins::register_all(&mut registry.borrow_mut());

    let host = Rc::new(CliHost {
        registry: registry.clone(),
        history: RefCell::new(Vec::new()),
    });
    let ctx = ExecContext {
        host: host.clone(),
        sink: Rc::new(CliSink),
        width: terminal_width(),
        pane: 0,
        run_id: 0,
    };
    let scope = Scope::new();

    let mut editor = match DefaultEditor::new() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("failed to start line editor: {e}");
            std::process::exit(1);
        }
    };

    println!("bterm — structured shell (native harness). Ctrl-D to exit.");
    let mut last_ok = true;
    loop {
        let prompt = if last_ok { "\x1b[32m❯\x1b[0m " } else { "\x1b[31m❯\x1b[0m " };
        match editor.readline(prompt) {
            Ok(src) => {
                if src.trim().is_empty() {
                    continue;
                }
                let _ = editor.add_history_entry(&src);
                host.history.borrow_mut().push(src.clone());

                let parsed = bterm_core::parse::parse(&src);
                if !parsed.errors.is_empty() {
                    for err in &parsed.errors {
                        print!("{}", err.render(&src));
                    }
                    last_ok = false;
                    continue;
                }
                let (results, error) = block_on(eval_line(&parsed.line, &*registry, &ctx, &scope));
                // Exhaustive on purpose: an `if let` here silently swallowed
                // the `Rendered` variant when it was added.
                for data in results {
                    match data {
                        PipelineData::Value(v) => print!("{}", render(&v, ctx.width)),
                        // Already formatted (help, `table`) — print verbatim.
                        PipelineData::Rendered(s) => println!("{s}"),
                        PipelineData::Empty => {}
                    }
                }
                match error {
                    Some(err) => {
                        print!("{}", err.render(&src));
                        last_ok = false;
                    }
                    None => last_ok = true,
                }
            }
            Err(ReadlineError::Interrupted) => continue,
            Err(ReadlineError::Eof) => break,
            Err(e) => {
                eprintln!("readline error: {e}");
                break;
            }
        }
    }
}
