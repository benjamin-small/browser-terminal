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

/// The exact bytes a record puts on the wire.
///
/// Split out from the sink so the reset-prefix invariant is testable without
/// capturing a file descriptor.
///
/// Every record is reset-prefixed, raw or not — the same invariant `PaneSink`
/// holds, and for a stronger reason. The allowlist lets a raw write carry SGR
/// on the argument that a command's styling is bounded by a reset at the
/// command boundary, and that argument fails on the abort path: an aborted
/// command's body never resumes, so its trailing reset never runs. In the
/// browser that leaks into the next command's output; here it leaks into a
/// real terminal, where conceal or a background colour outlives the process
/// and lands on the user's shell prompt. Prefixing per record makes the leak
/// structurally impossible rather than dependent on cleanup that cancellation
/// can skip.
///
/// A raw write otherwise owns its formatting: no appended newline, so a
/// partial write stays partial and a progress bar can redraw in place.
fn wire_text(record: &bterm_core::sink::Record) -> String {
    const RESET: &str = "\x1b[0m";
    if record.is_raw() {
        format!("{RESET}{}", record.text())
    } else {
        format!("{RESET}{}\n", record.text())
    }
}

/// The native harness maps the channels onto real file descriptors, so
/// `bterm 2>/dev/null` behaves the way a shell user expects.
struct CliSink;

impl bterm_core::sink::Sink for CliSink {
    fn write(&self, record: bterm_core::sink::Record) {
        use std::io::Write;
        let text = wire_text(&record);
        match record.channel() {
            bterm_core::sink::Channel::Log => {
                print!("{text}");
                // A raw write may carry no newline at all, so stdout's line
                // buffer would hold it: flush, or a progress bar only appears
                // once the command ends.
                if record.is_raw() {
                    let _ = std::io::stdout().flush();
                }
            }
            bterm_core::sink::Channel::Err => {
                eprint!("{text}");
                if record.is_raw() {
                    let _ = std::io::stderr().flush();
                }
            }
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

#[cfg(test)]
mod tests {
    use super::wire_text;
    use bterm_core::sink::Record;

    #[test]
    fn every_record_is_reset_prefixed_so_sgr_cannot_escape_into_the_shell() {
        // The CLI writes to the user's real terminal, so a command that
        // leaves conceal or a background colour set would follow bterm out
        // of the process. Raw records are the case that matters -- they are
        // the only ones that may carry SGR at all -- but the invariant is
        // unconditional so no future record type can slip through.
        let raw = wire_text(&Record::raw_log("\x1b[8mconceal"));
        assert!(raw.starts_with("\x1b[0m"), "raw record not reset-prefixed: {raw:?}");
        assert!(raw.ends_with("conceal"), "a partial write must stay partial: {raw:?}");

        let cooked = wire_text(&Record::log("plain"));
        assert_eq!(cooked, "\x1b[0mplain\n");

        assert!(wire_text(&Record::raw_err("\x1b[41m")).starts_with("\x1b[0m"));
        assert!(wire_text(&Record::err("boom")).starts_with("\x1b[0m"));
    }
}
